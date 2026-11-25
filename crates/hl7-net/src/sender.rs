//! Pure retry state machine and asynchronous MLLP client sender (NET-20..NET-34, D-18, D-23).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::codec::Framed;

use crate::envelope::{
    classify_malformed_bytes, format_raw_byte_preview, InboundFrame, MalformedPacketInfo,
    MllpCodec, DEFAULT_MAX_FRAME_BYTES, MAX_RAW_PREVIEW_BYTES,
};
use crate::error::NetError;
use crate::profile::{MaxTriesAction, NakAction, SendScope, SenderProfile};
use crate::tls::build_tls_connector;
use hl7_core::ack::{
    current_hl7_timestamp, evaluate_ack, extract_msa2_control_id, generate_unique_control_id,
    msh_suppresses_ack, AckMode, AckResult,
};
use hl7_core::delimiters::Delimiters;

/// User-facing message when connecting fails or times out (NET-23).
pub const MSG_UNABLE_TO_CONNECT: &str = "Unable to connect to remote host.";
/// User-facing message when max tries is exceeded with CloseAndStop (NET-29).
pub const MSG_SERVER_NOT_ACKING: &str = "Remote host is not acknowledging records.";
/// User-facing message when server abruptly closes or socket errors (NET-30).
pub const MSG_SERVER_ABRUPTLY_STOPPED: &str = "Remote host closed the connection unexpectedly.";

/// The internal state of the sender engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderState {
    /// Inactive.
    Idle,
    /// Establishing connection.
    Connecting,
    /// Delay after initial connection.
    DelayAfterConnect,
    /// Writing frame to wire.
    Sending,
    /// Waiting for response.
    WaitingForAck,
    /// Pause between records.
    DelayBetweenRecords,
    /// Closing and re-establishing socket.
    Reconnecting,
    /// Paused by user.
    Paused,
}

/// Commands delivered to the async sender task (NET-22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderCommand {
    /// Pause transmission after the current in-flight frame completes.
    Pause,
    /// Resume transmission.
    Resume,
    /// Immediate cancellation (NET-22, NET-25).
    Stop,
}

/// Per-record history entry for the send log (NET-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendHistoryEntry {
    /// 0-based record index in the send batch.
    pub record_index: usize,
    /// Outgoing MSH-10 control ID.
    pub control_id: String,
    /// HL7 timestamp when send completed.
    pub timestamp: String,
    /// Outcome description (`"ACK"`, `"NAK"`, `"Timeout"`, `"Malformed"`, `"Skipped"`).
    pub outcome: String,
    /// Round-trip time in milliseconds.
    pub rtt_ms: u64,
    /// Attempt number (1-based).
    pub attempt: u32,
    /// Resolved remote peer `IP:port` for this send attempt.
    pub peer_addr: Option<SocketAddr>,
}

/// Events emitted by the async sender task to the UI.
#[derive(Debug, Clone)]
pub enum SenderEvent {
    /// State transition notification.
    StateChanged(SenderState),
    /// Overall progress update (NET-22).
    Progress {
        /// 1-based index of current record in batch.
        current: usize,
        /// Total records in scope.
        total: usize,
        /// Current attempt count for active record.
        attempts: u32,
        /// Total socket reset cycles so far.
        resets: u32,
    },
    /// ACK or NAK message received (NET-27).
    AckReceived {
        /// Record index.
        record_index: usize,
        /// Sent MSH-10 control ID.
        control_id: String,
        /// Raw response payload (envelope stripped).
        response: Vec<u8>,
        /// Evaluation outcome (`true` = ACK, `false` = NAK).
        is_ack: bool,
        /// Resolved remote peer `IP:port`.
        peer_addr: SocketAddr,
    },
    /// Malformed MLLP frame or unexpected/stray bytes received from the remote peer.
    MalformedPacket {
        /// Record index awaiting response.
        record_index: usize,
        /// Sent MSH-10 control ID.
        control_id: String,
        /// Structured malformed-packet diagnostic details.
        info: MalformedPacketInfo,
    },
    /// Record skipped per retry policy.
    RecordSkipped {
        /// Record index.
        record_index: usize,
        /// Reason.
        reason: String,
    },
    /// Connection reset and retry.
    ConnectionReset {
        /// Record index.
        record_index: usize,
        /// Number of socket reconnect cycles.
        reset_count: u32,
    },
    /// Send history log entry recorded (NET-33).
    History(SendHistoryEntry),
    /// Fatal or completion status message (`"Unable to connect to remote host."`, `"Remote host is not acknowledging records."`, `"Remote host closed the connection unexpectedly."`).
    StatusMessage(String),
    /// Sender finished and returned to Idle.
    Finished {
        /// Records successfully sent.
        sent: usize,
        /// Records skipped.
        skipped: usize,
    },
}

/// Transition decision returned by the pure state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAction {
    /// Keep connection, resend the same record (`attempts += 1`).
    ResendRecord,
    /// Close socket, reconnect, and resend the same record.
    ResetConnectionAndResend,
    /// Skip the current record and move to the next.
    SkipRecord,
    /// Close connection, emit message, and stop.
    CloseAndStop(String),
    /// Proceed to the next record.
    AdvanceToNext,
    /// Delay for duration, then proceed.
    Delay(Duration),
    /// All records sent successfully.
    Done,
}

/// Pure deterministic state transition engine for testing all combinations of NAK × MaxTries.
#[derive(Debug, Clone)]
pub struct SenderStateMachine {
    profile: SenderProfile,
    current_index: usize,
    total_records: usize,
    attempts: u32,
    reset_count: u32,
    sent_count: usize,
    skipped_count: usize,
}

impl SenderStateMachine {
    /// Create a new state machine for `total_records` with `profile`.
    pub fn new(profile: SenderProfile, total_records: usize) -> Self {
        Self {
            profile,
            current_index: 0,
            total_records,
            attempts: 0,
            reset_count: 0,
            sent_count: 0,
            skipped_count: 0,
        }
    }

    /// Return current 0-based record index.
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Current attempt count for active record.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Total socket resets.
    pub fn reset_count(&self) -> u32 {
        self.reset_count
    }

    /// Records successfully sent.
    pub fn sent_count(&self) -> usize {
        self.sent_count
    }

    /// Records skipped.
    pub fn skipped_count(&self) -> usize {
        self.skipped_count
    }

    /// Mark an attempt sent.
    pub fn on_send_attempt(&mut self) {
        self.attempts += 1;
    }

    /// Event: Received positive ACK (or frame sent when `wait_for_ack = false`).
    pub fn on_ack_received(&mut self) -> NextAction {
        self.sent_count += 1;
        self.attempts = 0;
        self.current_index += 1;

        if self.current_index >= self.total_records {
            NextAction::Done
        } else if self.profile.delay_between_records_seconds > 0.0 {
            NextAction::Delay(Duration::from_secs_f64(
                self.profile.delay_between_records_seconds,
            ))
        } else {
            NextAction::AdvanceToNext
        }
    }

    /// Event: Received NAK or ACK timeout (NET-28, NET-29, D-18).
    pub fn on_nak_or_timeout(&mut self) -> NextAction {
        // NET-28: If NakAction is Skip, we skip immediately without checking max_tries (D-18).
        if self.profile.nak_action == NakAction::Skip {
            self.skipped_count += 1;
            self.attempts = 0;
            self.current_index += 1;
            if self.current_index >= self.total_records {
                return NextAction::Done;
            } else if self.profile.delay_between_records_seconds > 0.0 {
                return NextAction::Delay(Duration::from_secs_f64(
                    self.profile.delay_between_records_seconds,
                ));
            } else {
                return NextAction::AdvanceToNext;
            }
        }

        // Check if max tries exceeded
        let max_tries = self.profile.max_tries.max(1);
        if self.attempts >= max_tries {
            match self.profile.max_tries_action {
                MaxTriesAction::ResetAndResend => {
                    self.reset_count += 1;
                    self.attempts = 0;
                    NextAction::ResetConnectionAndResend
                }
                MaxTriesAction::Skip => {
                    self.skipped_count += 1;
                    self.attempts = 0;
                    self.current_index += 1;
                    if self.current_index >= self.total_records {
                        NextAction::Done
                    } else if self.profile.delay_between_records_seconds > 0.0 {
                        NextAction::Delay(Duration::from_secs_f64(
                            self.profile.delay_between_records_seconds,
                        ))
                    } else {
                        NextAction::AdvanceToNext
                    }
                }
                MaxTriesAction::CloseAndStop => {
                    NextAction::CloseAndStop(MSG_SERVER_NOT_ACKING.to_string())
                }
            }
        } else {
            // attempts < max_tries: apply NakAction
            match self.profile.nak_action {
                NakAction::Resend => NextAction::ResendRecord,
                NakAction::ResetAndResend => {
                    self.reset_count += 1;
                    NextAction::ResetConnectionAndResend
                }
                NakAction::Skip => unreachable!(),
            }
        }
    }
}

/// Validate and resolve a [`SendScope`] against `total_records` into 0-based record indices (NET-21).
pub fn resolve_send_scope(scope: &SendScope, total_records: usize) -> Result<Vec<usize>, NetError> {
    match scope {
        SendScope::All => Ok((0..total_records).collect()),
        SendScope::Selected(indices) => Ok(indices
            .iter()
            .copied()
            .filter(|&i| i < total_records)
            .collect()),
        SendScope::Range { start, end } => {
            if *start < 1 || *start > *end || *end > total_records {
                return Err(NetError::Io(format!(
                    "Invalid send range {start}..={end} (total records: {total_records})"
                )));
            }
            Ok(((start - 1)..*end).collect())
        }
    }
}

/// Rewrite outgoing payload if `rewrite_control_id` or `refresh_timestamp` is active (NET-31, NET-32).
///
/// Modifies outgoing copy only; returns `(effective_payload, effective_control_id)`.
pub fn prepare_outgoing_record(
    raw_bytes: &[u8],
    rewrite_control_id: bool,
    refresh_timestamp: bool,
) -> (Vec<u8>, String) {
    let delims = Delimiters::from_header_bytes(raw_bytes).unwrap_or(Delimiters::STANDARD);
    let fs = delims.field;

    let mut segments: Vec<Vec<u8>> = raw_bytes
        .split(|&b| b == b'\r')
        .map(|s| s.to_vec())
        .collect();

    let mut control_id = String::new();

    if let Some(first_seg) = segments.first_mut() {
        if first_seg.len() >= 9 && &first_seg[0..3] == b"MSH" && first_seg[8] == fs {
            let mut fields: Vec<Vec<u8>> = first_seg[9..]
                .split(|&b| b == fs)
                .map(|f| f.to_vec())
                .collect();

            // MSH-7 is index 4 (7 - 3)
            if refresh_timestamp && fields.len() > 4 {
                fields[4] = current_hl7_timestamp().into_bytes();
            }

            // MSH-10 is index 7 (10 - 3)
            if fields.len() > 7 {
                if rewrite_control_id {
                    let new_id = generate_unique_control_id();
                    fields[7] = new_id.as_bytes().to_vec();
                    control_id = new_id;
                } else {
                    control_id = String::from_utf8_lossy(&fields[7]).to_string();
                }
            }

            let mut new_msh = first_seg[..9].to_vec();
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    new_msh.push(fs);
                }
                new_msh.extend_from_slice(f);
            }
            *first_seg = new_msh;
        }
    }

    let mut out = Vec::with_capacity(raw_bytes.len());
    for (i, s) in segments.iter().enumerate() {
        if i > 0 {
            out.push(b'\r');
        }
        out.extend_from_slice(s);
    }

    (out, control_id)
}

enum ClientFramedStream {
    Plain {
        framed: Framed<TcpStream, MllpCodec>,
        peer_addr: SocketAddr,
    },
    Tls {
        framed: Box<Framed<tokio_rustls::client::TlsStream<TcpStream>, MllpCodec>>,
        peer_addr: SocketAddr,
    },
}

impl ClientFramedStream {
    fn peer_addr(&self) -> SocketAddr {
        match self {
            Self::Plain { peer_addr, .. } | Self::Tls { peer_addr, .. } => *peer_addr,
        }
    }

    fn drain_read_buffer(&mut self) -> Vec<u8> {
        let buf = match self {
            Self::Plain { framed, .. } => framed.read_buffer_mut(),
            Self::Tls { framed, .. } => framed.read_buffer_mut(),
        };
        if buf.is_empty() {
            Vec::new()
        } else {
            buf.split().to_vec()
        }
    }

    async fn send_frame(&mut self, payload: &[u8]) -> Result<(), NetError> {
        match self {
            Self::Plain { framed, .. } => framed.send(payload).await,
            Self::Tls { framed, .. } => framed.send(payload).await,
        }
    }

    async fn next_frame(&mut self) -> Option<Result<InboundFrame, NetError>> {
        match self {
            Self::Plain { framed, .. } => framed.next().await,
            Self::Tls { framed, .. } => framed.next().await,
        }
    }
}

/// Establish a plaintext TCP or TLS 1.2/1.3 MLLP connection with connect timeout (NET-23, NET-34).
async fn connect_client_stream(profile: &SenderProfile) -> Result<ClientFramedStream, NetError> {
    let addr = format!("{}:{}", profile.host, profile.port);
    let connect_dur = Duration::from_secs_f64(profile.connect_timeout_seconds.max(0.1));

    let tcp = match timeout(connect_dur, TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        _ => return Err(NetError::Io(MSG_UNABLE_TO_CONNECT.to_string())),
    };
    let peer_addr = tcp
        .peer_addr()
        .map_err(|_| NetError::Io(MSG_UNABLE_TO_CONNECT.to_string()))?;

    let codec = MllpCodec::new(profile.envelope, DEFAULT_MAX_FRAME_BYTES);
    if profile.tls.enabled {
        let connector = build_tls_connector(&profile.tls)?;
        let sni = profile
            .tls
            .sni_server_name
            .clone()
            .unwrap_or_else(|| profile.host.clone());
        let server_name = ServerName::try_from(sni)
            .map_err(|_| NetError::Io(MSG_UNABLE_TO_CONNECT.to_string()))?;
        let tls_stream = match timeout(connect_dur, connector.connect(server_name, tcp)).await {
            Ok(Ok(s)) => s,
            _ => return Err(NetError::Io(MSG_UNABLE_TO_CONNECT.to_string())),
        };
        Ok(ClientFramedStream::Tls {
            framed: Box::new(Framed::new(tls_stream, codec)),
            peer_addr,
        })
    } else {
        Ok(ClientFramedStream::Plain {
            framed: Framed::new(tcp, codec),
            peer_addr,
        })
    }
}

/// Run the asynchronous MLLP sender task over TCP or TLS (NET-20..NET-34, D-18, D-23).
///
/// `records`: Raw bytes of the records selected by [`resolve_send_scope`].
pub async fn run_sender(
    profile: SenderProfile,
    records: Vec<Vec<u8>>,
    event_tx: mpsc::UnboundedSender<SenderEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<SenderCommand>,
) -> Result<(), NetError> {
    if records.is_empty() {
        let _ = event_tx.send(SenderEvent::Finished {
            sent: 0,
            skipped: 0,
        });
        return Ok(());
    }

    let mut sm = SenderStateMachine::new(profile.clone(), records.len());
    let mut paused = false;

    let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Connecting));
    let mut stream = match connect_or_cancel(&profile, &mut cmd_rx, &event_tx).await {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(()),
        Err(e) => return Err(e),
    };

    if profile.delay_after_connect_seconds > 0.0 {
        let _ = event_tx.send(SenderEvent::StateChanged(SenderState::DelayAfterConnect));
        if wait_delay_or_cmd(
            Duration::from_secs_f64(profile.delay_after_connect_seconds),
            &mut paused,
            &mut cmd_rx,
            &event_tx,
        )
        .await
        {
            return Ok(());
        }
    }

    loop {
        // Check queued control commands before sending next attempt.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                SenderCommand::Stop => {
                    finish_idle(&sm, &event_tx);
                    return Ok(());
                }
                SenderCommand::Pause => paused = true,
                SenderCommand::Resume => paused = false,
            }
        }

        if paused && wait_while_paused(&mut paused, &mut cmd_rx, &event_tx).await {
            finish_idle(&sm, &event_tx);
            return Ok(());
        }

        let rec_idx = sm.current_index();
        if rec_idx >= records.len() {
            finish_idle(&sm, &event_tx);
            return Ok(());
        }

        let (outgoing_bytes, control_id) = prepare_outgoing_record(
            &records[rec_idx],
            profile.rewrite_control_id,
            profile.refresh_timestamp,
        );

        sm.on_send_attempt();
        let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Sending));
        let _ = event_tx.send(SenderEvent::Progress {
            current: rec_idx + 1,
            total: records.len(),
            attempts: sm.attempts(),
            resets: sm.reset_count(),
        });

        let send_start = Instant::now();
        if stream.send_frame(&outgoing_bytes).await.is_err() {
            let _ = event_tx.send(SenderEvent::StatusMessage(
                MSG_SERVER_ABRUPTLY_STOPPED.to_string(),
            ));
            finish_idle(&sm, &event_tx);
            return Err(NetError::Io(MSG_SERVER_ABRUPTLY_STOPPED.to_string()));
        }

        let peer_addr = stream.peer_addr();
        let configured_host =
            (profile.host != peer_addr.ip().to_string()).then(|| profile.host.clone());

        let skip_ack_by_msh = profile.respect_ack_modes && msh_suppresses_ack(&outgoing_bytes);
        let next_action = if !profile.wait_for_ack || skip_ack_by_msh {
            let outcome = if skip_ack_by_msh {
                "Sent (No ACK expected — MSH-15/16=NE)".to_string()
            } else {
                "Sent (No ACK)".to_string()
            };
            let _ = event_tx.send(SenderEvent::History(SendHistoryEntry {
                record_index: rec_idx,
                control_id,
                timestamp: current_hl7_timestamp(),
                outcome,
                rtt_ms: send_start.elapsed().as_millis() as u64,
                attempt: sm.attempts(),
                peer_addr: Some(peer_addr),
            }));
            sm.on_ack_received()
        } else {
            let _ = event_tx.send(SenderEvent::StateChanged(SenderState::WaitingForAck));
            let wait_dur = Duration::from_secs_f64(profile.ack_wait_seconds.max(0.05));

            enum WaitOutcome {
                Frame(Option<Result<InboundFrame, NetError>>),
                Timeout,
                Stopped,
            }

            let wait_res = {
                let sleep_fut = sleep(wait_dur);
                tokio::pin!(sleep_fut);
                loop {
                    tokio::select! {
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                Some(SenderCommand::Stop) => break WaitOutcome::Stopped,
                                Some(SenderCommand::Pause) => paused = true,
                                Some(SenderCommand::Resume) => paused = false,
                                None => {}
                            }
                        }
                        frame = stream.next_frame() => break WaitOutcome::Frame(frame),
                        () = &mut sleep_fut => break WaitOutcome::Timeout,
                    }
                }
            };

            match wait_res {
                WaitOutcome::Stopped => {
                    finish_idle(&sm, &event_tx);
                    return Ok(());
                }
                WaitOutcome::Frame(Some(Ok(frame))) => {
                    let rtt_ms = send_start.elapsed().as_millis() as u64;
                    let ts = current_hl7_timestamp();

                    if !frame.leading_garbage.is_empty() {
                        let _ = event_tx.send(SenderEvent::MalformedPacket {
                            record_index: rec_idx,
                            control_id: control_id.clone(),
                            info: MalformedPacketInfo {
                                timestamp: ts.clone(),
                                peer_addr,
                                configured_host: configured_host.clone(),
                                reason: format!(
                                    "Leading non-0x{:02X} garbage bytes skipped before MLLP frame",
                                    profile.envelope.start_block
                                ),
                                raw_preview: format_raw_byte_preview(
                                    &frame.leading_garbage,
                                    MAX_RAW_PREVIEW_BYTES,
                                ),
                                byte_len: frame.leading_garbage.len(),
                            },
                        });
                    }
                    if frame.missing_start_block {
                        let _ = event_tx.send(SenderEvent::MalformedPacket {
                            record_index: rec_idx,
                            control_id: control_id.clone(),
                            info: MalformedPacketInfo {
                                timestamp: ts.clone(),
                                peer_addr,
                                configured_host: configured_host.clone(),
                                reason: format!(
                                    "Missing MLLP start byte 0x{:02X}",
                                    profile.envelope.start_block
                                ),
                                raw_preview: format_raw_byte_preview(
                                    &frame.payload,
                                    MAX_RAW_PREVIEW_BYTES,
                                ),
                                byte_len: frame.payload.len(),
                            },
                        });
                    } else if frame.payload.trim_ascii().is_empty() {
                        let _ = event_tx.send(SenderEvent::MalformedPacket {
                            record_index: rec_idx,
                            control_id: control_id.clone(),
                            info: MalformedPacketInfo {
                                timestamp: ts.clone(),
                                peer_addr,
                                configured_host: configured_host.clone(),
                                reason: "Empty MLLP payload".to_string(),
                                raw_preview: String::new(),
                                byte_len: 0,
                            },
                        });
                    } else if matches!(profile.ack_mode, AckMode::Hl7)
                        && !payload_starts_with_hl7_header(&frame.payload)
                    {
                        let _ = event_tx.send(SenderEvent::MalformedPacket {
                            record_index: rec_idx,
                            control_id: control_id.clone(),
                            info: MalformedPacketInfo {
                                timestamp: ts.clone(),
                                peer_addr,
                                configured_host: configured_host.clone(),
                                reason: "Unexpected non-HL7 payload inside MLLP frame".to_string(),
                                raw_preview: format_raw_byte_preview(
                                    &frame.payload,
                                    MAX_RAW_PREVIEW_BYTES,
                                ),
                                byte_len: frame.payload.len(),
                            },
                        });
                    }

                    // Always-on MSA-2 Control ID hygiene check regardless of respect_ack_modes
                    if let Some(msa2) = extract_msa2_control_id(&frame.payload) {
                        if msa2 != control_id.trim() {
                            let _ = event_tx.send(SenderEvent::StatusMessage(format!(
                                "Warning: ACK MSA-2 mismatch on Message #{} (expected MSH-10='{}', received MSA-2='{}')",
                                rec_idx + 1,
                                control_id.trim(),
                                msa2
                            )));
                        }
                    }

                    let ack_res = evaluate_ack(&frame.payload, &control_id, &profile.ack_mode);
                    let is_ack = ack_res == AckResult::Ack;
                    let _ = event_tx.send(SenderEvent::AckReceived {
                        record_index: rec_idx,
                        control_id: control_id.clone(),
                        response: frame.payload,
                        is_ack,
                        peer_addr,
                    });
                    let _ = event_tx.send(SenderEvent::History(SendHistoryEntry {
                        record_index: rec_idx,
                        control_id,
                        timestamp: ts,
                        outcome: if is_ack { "ACK" } else { "NAK" }.to_string(),
                        rtt_ms,
                        attempt: sm.attempts(),
                        peer_addr: Some(peer_addr),
                    }));
                    if is_ack {
                        sm.on_ack_received()
                    } else {
                        sm.on_nak_or_timeout()
                    }
                }
                WaitOutcome::Frame(Some(Err(err))) => {
                    let rtt_ms = send_start.elapsed().as_millis() as u64;
                    let ts = current_hl7_timestamp();
                    match &err {
                        NetError::MalformedFrame {
                            reason,
                            raw_preview,
                            total_bytes,
                        } => {
                            let _ = event_tx.send(SenderEvent::MalformedPacket {
                                record_index: rec_idx,
                                control_id: control_id.clone(),
                                info: MalformedPacketInfo {
                                    timestamp: ts.clone(),
                                    peer_addr,
                                    configured_host: configured_host.clone(),
                                    reason: reason.clone(),
                                    raw_preview: raw_preview.clone(),
                                    byte_len: *total_bytes,
                                },
                            });
                            let _ = event_tx.send(SenderEvent::History(SendHistoryEntry {
                                record_index: rec_idx,
                                control_id,
                                timestamp: ts,
                                outcome: "Malformed".to_string(),
                                rtt_ms,
                                attempt: sm.attempts(),
                                peer_addr: Some(peer_addr),
                            }));
                        }
                        NetError::FrameTooLarge {
                            max_bytes,
                            actual_bytes,
                            raw_preview,
                        } => {
                            let _ = event_tx.send(SenderEvent::MalformedPacket {
                                record_index: rec_idx,
                                control_id: control_id.clone(),
                                info: MalformedPacketInfo {
                                    timestamp: ts.clone(),
                                    peer_addr,
                                    configured_host: configured_host.clone(),
                                    reason: format!(
                                        "Oversized MLLP frame exceeded {max_bytes} bytes ({actual_bytes} bytes)"
                                    ),
                                    raw_preview: raw_preview.clone(),
                                    byte_len: *actual_bytes,
                                },
                            });
                            let _ = event_tx.send(SenderEvent::History(SendHistoryEntry {
                                record_index: rec_idx,
                                control_id,
                                timestamp: ts,
                                outcome: "Oversized".to_string(),
                                rtt_ms,
                                attempt: sm.attempts(),
                                peer_addr: Some(peer_addr),
                            }));
                        }
                        NetError::Io(_) => {}
                    }
                    // NET-30: Server abruptly closed or socket/frame error.
                    let _ = event_tx.send(SenderEvent::StatusMessage(
                        MSG_SERVER_ABRUPTLY_STOPPED.to_string(),
                    ));
                    finish_idle(&sm, &event_tx);
                    return Err(NetError::Io(MSG_SERVER_ABRUPTLY_STOPPED.to_string()));
                }
                WaitOutcome::Frame(None) => {
                    // NET-30: Server abruptly closed.
                    let _ = event_tx.send(SenderEvent::StatusMessage(
                        MSG_SERVER_ABRUPTLY_STOPPED.to_string(),
                    ));
                    finish_idle(&sm, &event_tx);
                    return Err(NetError::Io(MSG_SERVER_ABRUPTLY_STOPPED.to_string()));
                }
                WaitOutcome::Timeout => {
                    let rtt_ms = send_start.elapsed().as_millis() as u64;
                    let ts = current_hl7_timestamp();
                    let stray = stream.drain_read_buffer();
                    let outcome = if !stray.is_empty() {
                        let reason = classify_malformed_bytes(&stray, profile.envelope, "timeout");
                        let raw_preview = format_raw_byte_preview(&stray, MAX_RAW_PREVIEW_BYTES);
                        let _ = event_tx.send(SenderEvent::MalformedPacket {
                            record_index: rec_idx,
                            control_id: control_id.clone(),
                            info: MalformedPacketInfo {
                                timestamp: ts.clone(),
                                peer_addr,
                                configured_host: configured_host.clone(),
                                reason,
                                raw_preview,
                                byte_len: stray.len(),
                            },
                        });
                        "Malformed (Timeout)"
                    } else {
                        "Timeout"
                    };
                    let _ = event_tx.send(SenderEvent::History(SendHistoryEntry {
                        record_index: rec_idx,
                        control_id,
                        timestamp: ts,
                        outcome: outcome.to_string(),
                        rtt_ms,
                        attempt: sm.attempts(),
                        peer_addr: Some(peer_addr),
                    }));
                    sm.on_nak_or_timeout()
                }
            }
        };

        // Check if Pause was requested while in-flight before taking the next action.
        if paused && wait_while_paused(&mut paused, &mut cmd_rx, &event_tx).await {
            finish_idle(&sm, &event_tx);
            return Ok(());
        }

        match next_action {
            NextAction::ResendRecord | NextAction::AdvanceToNext => {}
            NextAction::SkipRecord => {
                let _ = event_tx.send(SenderEvent::RecordSkipped {
                    record_index: rec_idx,
                    reason: "Skipped per retry policy".to_string(),
                });
            }
            NextAction::ResetConnectionAndResend => {
                let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Reconnecting));
                let _ = event_tx.send(SenderEvent::ConnectionReset {
                    record_index: rec_idx,
                    reset_count: sm.reset_count(),
                });
                drop(stream);
                stream = match connect_or_cancel(&profile, &mut cmd_rx, &event_tx).await {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        finish_idle(&sm, &event_tx);
                        return Ok(());
                    }
                    Err(e) => {
                        finish_idle(&sm, &event_tx);
                        return Err(e);
                    }
                };
                if profile.delay_after_connect_seconds > 0.0 {
                    let _ =
                        event_tx.send(SenderEvent::StateChanged(SenderState::DelayAfterConnect));
                    if wait_delay_or_cmd(
                        Duration::from_secs_f64(profile.delay_after_connect_seconds),
                        &mut paused,
                        &mut cmd_rx,
                        &event_tx,
                    )
                    .await
                    {
                        finish_idle(&sm, &event_tx);
                        return Ok(());
                    }
                }
            }
            NextAction::Delay(dur) => {
                let _ = event_tx.send(SenderEvent::StateChanged(SenderState::DelayBetweenRecords));
                if wait_delay_or_cmd(dur, &mut paused, &mut cmd_rx, &event_tx).await {
                    finish_idle(&sm, &event_tx);
                    return Ok(());
                }
            }
            NextAction::CloseAndStop(msg) => {
                let _ = event_tx.send(SenderEvent::StatusMessage(msg.clone()));
                finish_idle(&sm, &event_tx);
                return Err(NetError::Io(msg));
            }
            NextAction::Done => {
                finish_idle(&sm, &event_tx);
                return Ok(());
            }
        }
    }
}

async fn connect_or_cancel(
    profile: &SenderProfile,
    cmd_rx: &mut mpsc::UnboundedReceiver<SenderCommand>,
    event_tx: &mpsc::UnboundedSender<SenderEvent>,
) -> Result<Option<ClientFramedStream>, NetError> {
    tokio::select! {
        cmd = cmd_rx.recv() => {
            if matches!(cmd, Some(SenderCommand::Stop)) {
                let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Idle));
                let _ = event_tx.send(SenderEvent::Finished { sent: 0, skipped: 0 });
                return Ok(None);
            }
            match connect_client_stream(profile).await {
                Ok(s) => Ok(Some(s)),
                Err(e) => {
                    let _ = event_tx.send(SenderEvent::StatusMessage(MSG_UNABLE_TO_CONNECT.to_string()));
                    let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Idle));
                    Err(e)
                }
            }
        }
        res = connect_client_stream(profile) => {
            match res {
                Ok(s) => Ok(Some(s)),
                Err(e) => {
                    let _ = event_tx.send(SenderEvent::StatusMessage(MSG_UNABLE_TO_CONNECT.to_string()));
                    let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Idle));
                    Err(e)
                }
            }
        }
    }
}

/// Wait for `dur`, handling `Stop` (returns `true`) or `Pause` (enters paused state).
async fn wait_delay_or_cmd(
    dur: Duration,
    paused: &mut bool,
    cmd_rx: &mut mpsc::UnboundedReceiver<SenderCommand>,
    event_tx: &mpsc::UnboundedSender<SenderEvent>,
) -> bool {
    let sleep_fut = sleep(dur);
    tokio::pin!(sleep_fut);
    loop {
        tokio::select! {
            () = &mut sleep_fut => return false,
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SenderCommand::Stop) => return true,
                    Some(SenderCommand::Pause) => {
                        *paused = true;
                        return wait_while_paused(paused, cmd_rx, event_tx).await;
                    }
                    Some(SenderCommand::Resume) => *paused = false,
                    None => {}
                }
            }
        }
    }
}

/// Block until `Resume` (returns `false`) or `Stop` (returns `true`).
async fn wait_while_paused(
    paused: &mut bool,
    cmd_rx: &mut mpsc::UnboundedReceiver<SenderCommand>,
    event_tx: &mpsc::UnboundedSender<SenderEvent>,
) -> bool {
    let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Paused));
    while *paused {
        match cmd_rx.recv().await {
            Some(SenderCommand::Resume) => {
                *paused = false;
                return false;
            }
            Some(SenderCommand::Stop) | None => return true,
            Some(SenderCommand::Pause) => {}
        }
    }
    false
}

fn finish_idle(sm: &SenderStateMachine, event_tx: &mpsc::UnboundedSender<SenderEvent>) {
    let _ = event_tx.send(SenderEvent::StateChanged(SenderState::Idle));
    let _ = event_tx.send(SenderEvent::Finished {
        sent: sm.sent_count(),
        skipped: sm.skipped_count(),
    });
}

fn payload_starts_with_hl7_header(payload: &[u8]) -> bool {
    let trimmed = payload.trim_ascii_start();
    trimmed.len() >= 4
        && (&trimmed[0..3] == b"MSH" || &trimmed[0..3] == b"FHS" || &trimmed[0..3] == b"BHS")
        && trimmed[3].is_ascii_punctuation()
}

/// Generic helper to run an MLLP client round-trip on any `AsyncRead + AsyncWrite + Unpin + Send` stream.
pub async fn send_single_frame_over_stream<S>(
    stream: S,
    profile: &SenderProfile,
    payload: &[u8],
) -> Result<Option<InboundFrame>, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let codec = MllpCodec::new(profile.envelope, DEFAULT_MAX_FRAME_BYTES);
    let mut framed = Framed::new(stream, codec);
    framed.send(payload).await?;
    if !profile.wait_for_ack {
        return Ok(None);
    }
    let wait_dur = Duration::from_secs_f64(profile.ack_wait_seconds.max(0.05));
    match timeout(wait_dur, framed.next()).await {
        Ok(Some(Ok(frame))) => Ok(Some(frame)),
        Ok(Some(Err(e))) => Err(e),
        Ok(None) => Err(NetError::Io(MSG_SERVER_ABRUPTLY_STOPPED.to_string())),
        Err(_) => Err(NetError::Io("ACK Timeout".to_string())),
    }
}
