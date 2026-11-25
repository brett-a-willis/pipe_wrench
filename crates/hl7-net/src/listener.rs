//! Asynchronous multi-connection MLLP listener (NET-10..NET-17, D-19, D-23).

use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::envelope::{
    format_raw_byte_preview, InboundFrame, MalformedPacketInfo, MllpCodec, DEFAULT_MAX_FRAME_BYTES,
    MAX_RAW_PREVIEW_BYTES,
};
use crate::error::NetError;
use crate::profile::{ListenerAckSetting, ListenerProfile};
use crate::tls::build_tls_acceptor;
use hl7_core::ack::{build_hl7_ack, current_hl7_timestamp, frame_ack_from_file, AckCode};
use hl7_core::record::{Record, RecordFlags, SourceTerminator};
use hl7_core::scanner::detect_record_start;

/// Events emitted by the listener to the application UI.
#[derive(Debug, Clone)]
pub enum ListenerEvent {
    /// Listener started successfully.
    Started {
        /// Listening port.
        port: u16,
    },
    /// A new client connected.
    ClientConnected {
        /// Peer socket address.
        addr: SocketAddr,
        /// Connection ID.
        conn_id: u64,
    },
    /// A client disconnected.
    ClientDisconnected {
        /// Peer socket address.
        addr: SocketAddr,
        /// Connection ID.
        conn_id: u64,
        /// Messages processed on this connection.
        messages: u64,
    },
    /// A valid HL7 message record was received over TCP/IP (NET-13).
    RecordReceived {
        /// Parsed record.
        record: Record,
        /// Raw bytes.
        raw: Vec<u8>,
        /// Remote peer address.
        peer: SocketAddr,
    },
    /// Malformed MLLP frame or unexpected/stray bytes received from a remote client.
    MalformedPacket {
        /// Connection ID.
        conn_id: u64,
        /// Structured malformed-packet diagnostic details (including `peer_addr`).
        info: MalformedPacketInfo,
    },
    /// Socket error occurred on a specific client connection (isolated per D-19).
    ClientError {
        /// Connection ID.
        conn_id: u64,
        /// Error description.
        error: String,
    },
    /// Non-fatal listener warning (e.g. missing user-defined ACK file, NET-15).
    WarningMessage(String),
    /// Fatal listener error on listening socket (stops listener, NET-14, D-19).
    FatalError(String),
    /// Listener stopped.
    Stopped,
}

/// Commands to control the running listener.
#[derive(Debug)]
pub enum ListenerCommand {
    /// Stop listening and close all connections.
    Stop,
}

/// Run the async multi-connection listener server task.
pub async fn run_listener_server(
    profile: ListenerProfile,
    event_tx: mpsc::UnboundedSender<ListenerEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<ListenerCommand>,
) -> Result<(), NetError> {
    let bind_addr = format!("{}:{}", profile.bind_address, profile.port);
    let tcp_listener = match TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Port {} is already in use.", profile.port)
            } else {
                format!("Failed to bind {}: {}", bind_addr, e)
            };
            let _ = event_tx.send(ListenerEvent::FatalError(msg.clone()));
            return Err(NetError::Io(msg));
        }
    };

    let tls_acceptor = if profile.tls.enabled {
        Some(build_tls_acceptor(&profile.tls)?)
    } else {
        None
    };

    let _ = event_tx.send(ListenerEvent::Started { port: profile.port });

    let running = Arc::new(AtomicBool::new(true));
    let next_conn_id = Arc::new(AtomicU64::new(1));
    let profile_arc = Arc::new(profile);

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                if let Some(ListenerCommand::Stop) = cmd {
                    running.store(false, Ordering::Relaxed);
                    let _ = event_tx.send(ListenerEvent::Stopped);
                    break;
                }
            }
            accept_res = tcp_listener.accept() => {
                match accept_res {
                    Ok((stream, addr)) => {
                        let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
                        let event_tx_clone = event_tx.clone();
                        let profile_clone = Arc::clone(&profile_arc);
                        let tls_acceptor_clone = tls_acceptor.clone();

                        tokio::spawn(async move {
                            let _ = event_tx_clone.send(ListenerEvent::ClientConnected { addr, conn_id });

                            if let Some(acceptor) = tls_acceptor_clone {
                                match acceptor.accept(stream).await {
                                    Ok(tls_stream) => {
                                        handle_connection(tls_stream, addr, conn_id, profile_clone, event_tx_clone).await;
                                    }
                                    Err(e) => {
                                        let _ = event_tx_clone.send(ListenerEvent::ClientError {
                                            conn_id,
                                            error: format!("TLS handshake error: {e}"),
                                        });
                                    }
                                }
                            } else {
                                handle_connection(stream, addr, conn_id, profile_clone, event_tx_clone).await;
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("Fatal error accepting connection: {e}");
                        let _ = event_tx.send(ListenerEvent::FatalError(msg.clone()));
                        return Err(NetError::Io(msg));
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_connection<S>(
    stream: S,
    addr: SocketAddr,
    conn_id: u64,
    profile: Arc<ListenerProfile>,
    event_tx: mpsc::UnboundedSender<ListenerEvent>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let codec = MllpCodec::new(profile.envelope, DEFAULT_MAX_FRAME_BYTES);
    let mut framed = Framed::new(stream, codec);
    let mut msg_counter = 0u64;

    while let Some(frame_res) = framed.next().await {
        match frame_res {
            Ok(InboundFrame {
                payload,
                missing_start_block,
                leading_garbage,
            }) => {
                let ts = current_hl7_timestamp();
                if !leading_garbage.is_empty() {
                    let _ = event_tx.send(ListenerEvent::MalformedPacket {
                        conn_id,
                        info: MalformedPacketInfo {
                            timestamp: ts.clone(),
                            peer_addr: addr,
                            configured_host: None,
                            reason: format!(
                                "Leading non-0x{:02X} garbage bytes skipped before MLLP frame",
                                profile.envelope.start_block
                            ),
                            raw_preview: format_raw_byte_preview(
                                &leading_garbage,
                                MAX_RAW_PREVIEW_BYTES,
                            ),
                            byte_len: leading_garbage.len(),
                        },
                    });
                }
                if missing_start_block {
                    let _ = event_tx.send(ListenerEvent::MalformedPacket {
                        conn_id,
                        info: MalformedPacketInfo {
                            timestamp: ts.clone(),
                            peer_addr: addr,
                            configured_host: None,
                            reason: format!(
                                "Missing MLLP start byte 0x{:02X}",
                                profile.envelope.start_block
                            ),
                            raw_preview: format_raw_byte_preview(&payload, MAX_RAW_PREVIEW_BYTES),
                            byte_len: payload.len(),
                        },
                    });
                }

                if payload.trim_ascii().is_empty() {
                    let _ = event_tx.send(ListenerEvent::MalformedPacket {
                        conn_id,
                        info: MalformedPacketInfo {
                            timestamp: ts,
                            peer_addr: addr,
                            configured_host: None,
                            reason: "Empty MLLP payload".to_string(),
                            raw_preview: String::new(),
                            byte_len: 0,
                        },
                    });
                    continue;
                }

                // Detect delimiters and record kind
                let delims = hl7_core::delimiters::Delimiters::from_header_bytes(&payload)
                    .unwrap_or(hl7_core::delimiters::Delimiters::STANDARD);
                let Some((kind, active_delims)) = detect_record_start(&payload, delims) else {
                    let _ = event_tx.send(ListenerEvent::MalformedPacket {
                        conn_id,
                        info: MalformedPacketInfo {
                            timestamp: ts,
                            peer_addr: addr,
                            configured_host: None,
                            reason: "Unexpected non-HL7 payload inside MLLP frame".to_string(),
                            raw_preview: format_raw_byte_preview(&payload, MAX_RAW_PREVIEW_BYTES),
                            byte_len: payload.len(),
                        },
                    });
                    continue;
                };

                msg_counter += 1;

                // Optionally append to disk file (NET-10, NET-13)
                if let Some(log_path) = &profile.log_to_disk_path {
                    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
                        let _ = f.write_all(&payload);
                        if !payload.ends_with(b"\r") && !payload.ends_with(b"\n") {
                            let _ = f.write_all(b"\r\n");
                        }
                    }
                }

                let record = Record::from_normalized_bytes(
                    payload.clone(),
                    kind,
                    active_delims,
                    SourceTerminator::Unknown,
                    RecordFlags {
                        missing_start_block,
                        unknown_charset: false,
                    },
                );

                let _ = event_tx.send(ListenerEvent::RecordReceived {
                    record,
                    raw: payload.clone(),
                    peer: addr,
                });

                // Send ACK if configured (NET-15), respecting opt-in MSH-15/16=NE suppression
                if profile.send_ack {
                    if profile.respect_ack_modes && hl7_core::msh_suppresses_ack(&payload) {
                        let _ = event_tx.send(ListenerEvent::WarningMessage(format!(
                            "ACK suppressed per MSH-15/16=NE (peer {addr})"
                        )));
                        continue;
                    }
                    let ack_payload = match &profile.ack_setting {
                        ListenerAckSetting::SimpleHex(hex_str) => {
                            parse_hex_bytes(hex_str).unwrap_or_else(|| vec![0x0B, 0x06, 0x1C, 0x0D])
                        }
                        ListenerAckSetting::GeneratedHl7 { code } => {
                            let ack_code =
                                AckCode::from_bytes(code.as_bytes()).unwrap_or(AckCode::AA);
                            build_hl7_ack(&payload, ack_code, None)
                        }
                        ListenerAckSetting::FromFile(path) => {
                            let content = std::fs::read(path).ok();
                            if content.is_none() {
                                let _ = event_tx.send(ListenerEvent::WarningMessage(
                                    "Custom ACK file not found; sending null ACK frame."
                                        .to_string(),
                                ));
                            }
                            let env = hl7_core::MllpEnvelope {
                                start_block: profile.envelope.start_block,
                                end_data: profile.envelope.end_data,
                                end_block: profile.envelope.end_block,
                            };
                            frame_ack_from_file(content.as_deref(), env)
                        }
                    };

                    if let Err(e) = framed.send(&ack_payload[..]).await {
                        let _ = event_tx.send(ListenerEvent::ClientError {
                            conn_id,
                            error: format!("Failed sending ACK: {e}"),
                        });
                        break;
                    }
                }
            }
            Err(e) => {
                let ts = current_hl7_timestamp();
                match &e {
                    NetError::MalformedFrame {
                        reason,
                        raw_preview,
                        total_bytes,
                    } => {
                        let _ = event_tx.send(ListenerEvent::MalformedPacket {
                            conn_id,
                            info: MalformedPacketInfo {
                                timestamp: ts,
                                peer_addr: addr,
                                configured_host: None,
                                reason: reason.clone(),
                                raw_preview: raw_preview.clone(),
                                byte_len: *total_bytes,
                            },
                        });
                    }
                    NetError::FrameTooLarge {
                        max_bytes,
                        actual_bytes,
                        raw_preview,
                    } => {
                        let _ = event_tx.send(ListenerEvent::MalformedPacket {
                            conn_id,
                            info: MalformedPacketInfo {
                                timestamp: ts,
                                peer_addr: addr,
                                configured_host: None,
                                reason: format!(
                                    "Oversized MLLP frame exceeded {max_bytes} bytes ({actual_bytes} bytes)"
                                ),
                                raw_preview: raw_preview.clone(),
                                byte_len: *actual_bytes,
                            },
                        });
                    }
                    NetError::Io(_) => {}
                }
                let _ = event_tx.send(ListenerEvent::ClientError {
                    conn_id,
                    error: format!("Frame error from {addr}: {e}"),
                });
                break;
            }
        }
    }

    let _ = event_tx.send(ListenerEvent::ClientDisconnected {
        addr,
        conn_id,
        messages: msg_counter,
    });
}

fn parse_hex_bytes(input: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for token in input.split_whitespace() {
        let clean = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);
        let b = u8::from_str_radix(clean, 16).ok()?;
        bytes.push(b);
    }
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}
