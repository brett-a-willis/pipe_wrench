//! MLLP Client (Sender), MLLP Server (Listener), Connection List, and Received ACK Viewer
//! UI panels (`NET-1`, `NET-10..NET-17`, `NET-20..NET-34`, `G-5`, `G-6`, `D-15`, `D-18`, `D-19`, `D-23`).

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, Context, RichText, ScrollArea, Ui, Window};
use egui_extras::{Column, TableBuilder};
use hl7_core::ack::current_hl7_timestamp;
use hl7_core::{AckMode, Record};
use hl7_net::{
    resolve_send_scope, run_listener_server, run_sender, ListenerAckSetting, ListenerCommand,
    ListenerEvent, ListenerProfile, MaxTriesAction, MllpEnvelope, NakAction, SendHistoryEntry,
    SendScope, SenderCommand, SenderEvent, SenderProfile, SenderState,
};
use tokio::sync::mpsc;

/// Default bounded ring-buffer capacity for logged ACKs / responses / malformed packets.
pub const MAX_ACK_LOG_ENTRIES: usize = 1_000;
/// Default bounded ring-buffer capacity for send history entries.
pub const MAX_SEND_HISTORY_ENTRIES: usize = 10_000;

use crate::config::{
    load_listener_profile_file, load_sender_profile_file, save_listener_profile_in,
    save_sender_profile_in,
};
use crate::strings;
use crate::ui::icons::{
    icon_labeled_button, icon_toggle_labeled_button, paint_icon, IconKind, ICON_SIZE_INLINE,
};
use crate::ui::theme::{
    ack_code_pill_colors, apply_unified_control_metrics, render_pill_badge,
    styled_singleline_text_edit, ThemePalette, CONTROL_HEIGHT,
};

/// Editable envelope byte fields accepting Decimal (`11`), Octal (`0o13`), or Hex (`0x0B`) (`NET-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeInputState {
    /// Start Block byte string (default `"0x0B"`).
    pub sb_input: String,
    /// End Data byte string (default `"0x1C"`).
    pub ed_input: String,
    /// End Block byte string (default `"0x0D"`).
    pub eb_input: String,
}

impl Default for EnvelopeInputState {
    fn default() -> Self {
        Self::from_envelope(MllpEnvelope::DEFAULT)
    }
}

impl EnvelopeInputState {
    /// Initialize from an [`MllpEnvelope`].
    pub fn from_envelope(env: MllpEnvelope) -> Self {
        Self {
            sb_input: format!("0x{:02X}", env.start_block),
            ed_input: format!("0x{:02X}", env.end_data),
            eb_input: format!("0x{:02X}", env.end_block),
        }
    }

    /// Restore standard MLLP defaults (`0x0B`, `0x1C`, `0x0D`) (`NET-1`).
    pub fn reset_defaults(&mut self) {
        *self = Self::from_envelope(MllpEnvelope::DEFAULT);
    }

    /// Parse all three inputs (Decimal, Octal, or Hex) into an [`MllpEnvelope`] (`NET-1`).
    pub fn parse_envelope(&self) -> Result<MllpEnvelope, String> {
        let sb = MllpEnvelope::parse_byte(&self.sb_input)
            .ok_or_else(|| format!("Invalid Start Block byte '{}'", self.sb_input))?;
        let ed = MllpEnvelope::parse_byte(&self.ed_input)
            .ok_or_else(|| format!("Invalid End Data byte '{}'", self.ed_input))?;
        let eb = MllpEnvelope::parse_byte(&self.eb_input)
            .ok_or_else(|| format!("Invalid End Block byte '{}'", self.eb_input))?;
        Ok(MllpEnvelope {
            start_block: sb,
            end_data: ed,
            end_block: eb,
        })
    }
}

/// UI radio selector for [`SendScope`] (`NET-21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendScopeUiMode {
    /// Send all loaded records.
    #[default]
    All,
    /// Send only currently selected records.
    Selected,
    /// Send an inclusive 1-based range (`start..=end`).
    Range,
}

/// One entry in the Received ACK / NAK / Malformed Packet Viewer (`NET-27`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckViewerEntry {
    /// `"Received ACK"`, `"Received NAK"`,
    /// or `"Malformed / Unexpected MLLP Packet"`.
    pub heading: String,
    /// 0-based record index in the batch (0 for listener-side packets).
    pub record_index: usize,
    /// Sent MSH-10 control ID (or connection label for listener packets).
    pub control_id: String,
    /// HL7 timestamp when the response or malformed packet was received.
    pub timestamp: String,
    /// Resolved remote peer `IP:port` (plus configured hostname if different).
    pub peer_addr: Option<String>,
    /// Formatted response payload text or sanitized raw byte preview.
    pub payload_text: String,
    /// `true` if classified as ACK, `false` if NAK or malformed.
    pub is_ack: bool,
    /// `true` when this entry represents a malformed MLLP wrapper or unexpected/stray bytes.
    pub is_malformed: bool,
    /// Specific failure reason when `is_malformed` is `true`.
    pub failure_reason: Option<String>,
}

/// One client row in the listener connection list (`NET-12`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerConnectionRow {
    /// Unique connection ID.
    pub conn_id: u64,
    /// Remote peer `IP:port`.
    pub peer_addr: String,
    /// `true` while connected.
    pub active: bool,
    /// Number of MLLP frames received on this connection.
    pub messages_received: u64,
    /// Total payload bytes received on this connection.
    pub bytes_received: u64,
    /// Optional per-connection error description (`D-19`).
    pub last_error: Option<String>,
}

/// Active tab in the docked TCP/IP section (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TcpIpDockTab {
    /// Outbound MLLP / TLS 1.2/1.3 Client Sender (`NET-20..NET-34`).
    #[default]
    ClientSender,
    /// Inbound MLLP / TLS 1.2/1.3 Server Listener (`NET-10..NET-17`).
    ServerListener,
    /// Live and historical peer connections table (`NET-12`).
    Connections,
    /// Received ACK/NAK log & Send History export (`NET-27`, `NET-33`).
    AckLog,
}

/// Validation result for the MLLP Client Sender form fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderValidation {
    /// Error on the Host / IP input, if any.
    pub host: Option<String>,
    /// Error on the Target Port input (`1..=65535`), if any.
    pub port: Option<String>,
    /// Error on the Connect Timeout input (`> 0`), if any.
    pub connect_timeout: Option<String>,
    /// Error on the Delay After Connect input (`>= 0`), if any.
    pub delay_after_connect: Option<String>,
    /// Error on the Delay Between Records input (`>= 0`), if any.
    pub delay_between_records: Option<String>,
    /// Error on the ACK Max Wait input (`> 0` when `wait_for_ack`), if any.
    pub ack_wait: Option<String>,
    /// Error on the Max Tries input (`1..=100` when `wait_for_ack`), if any.
    pub max_tries: Option<String>,
    /// Error on the Range Start / End inputs (when `SendScopeUiMode::Range`), if any.
    pub range: Option<String>,
    /// Error on the MLLP Envelope byte inputs, if any.
    pub envelope: Option<String>,
}

impl SenderValidation {
    /// Returns `true` when all required Sender fields are valid.
    pub fn is_valid(&self) -> bool {
        self.first_error().is_none()
    }

    /// Returns the first validation error message, if any.
    pub fn first_error(&self) -> Option<&str> {
        self.host
            .as_deref()
            .or(self.port.as_deref())
            .or(self.connect_timeout.as_deref())
            .or(self.delay_after_connect.as_deref())
            .or(self.delay_between_records.as_deref())
            .or(self.ack_wait.as_deref())
            .or(self.max_tries.as_deref())
            .or(self.range.as_deref())
            .or(self.envelope.as_deref())
    }
}

/// Validation result for the MLLP Server Listener form fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerValidation {
    /// Error on the Bind Address input, if any.
    pub bind_address: Option<String>,
    /// Error on the Listener Port input (`1..=65535`), if any.
    pub port: Option<String>,
    /// Error on the MLLP Envelope byte inputs, if any.
    pub envelope: Option<String>,
}

impl ListenerValidation {
    /// Returns `true` when all required Listener fields are valid.
    pub fn is_valid(&self) -> bool {
        self.first_error().is_none()
    }

    /// Returns the first validation error message, if any.
    pub fn first_error(&self) -> Option<&str> {
        self.bind_address
            .as_deref()
            .or(self.port.as_deref())
            .or(self.envelope.as_deref())
    }
}

/// Complete state for the MLLP Sender, Listener, and ACK Viewer panels (`NET-10..NET-34`, `D-26`).
pub struct NetworkPanelState {
    runtime: Option<Arc<tokio::runtime::Runtime>>,
    /// Whether the integrated docked TCP/IP panel is open in the main window (`D-26`).
    pub docked_open: bool,
    /// Active tab in the docked TCP/IP panel (`D-26`).
    pub active_tab: TcpIpDockTab,
    // --- Sender (NET-20..NET-34) ---
    /// Whether the Sender panel is active (kept synchronized with `docked_open`).
    pub sender_open: bool,
    /// Active sender profile.
    pub sender_profile: SenderProfile,
    /// Digits-only UI buffer for target port (`1..=65535`).
    pub sender_port_input: String,
    /// Numeric UI buffer for connect timeout in seconds (`> 0`).
    pub sender_connect_timeout_input: String,
    /// Numeric UI buffer for delay after connect in seconds (`>= 0`).
    pub sender_delay_connect_input: String,
    /// Numeric UI buffer for delay between records in seconds (`>= 0`).
    pub sender_delay_between_input: String,
    /// Numeric UI buffer for ACK max wait in seconds (`> 0`).
    pub sender_ack_wait_input: String,
    /// Digits-only UI buffer for max retry attempts (`1..=100`).
    pub sender_max_tries_input: String,
    /// Digits-only UI buffer for 1-based range start.
    pub range_start_input: String,
    /// Digits-only UI buffer for 1-based range end.
    pub range_end_input: String,
    /// Editable envelope inputs for the sender.
    pub sender_envelope_ui: EnvelopeInputState,
    /// Selected send scope radio mode (`NET-21`).
    pub send_scope_mode: SendScopeUiMode,
    /// 1-based range start (`NET-21`).
    pub range_start: usize,
    /// 1-based range end (`NET-21`).
    pub range_end: usize,
    /// Hex string input when `AckMode::Simple` is selected (`NET-26`).
    pub simple_ack_hex_input: String,
    /// Token string input when `AckMode::LegacySubstring` is selected (`NET-26`).
    pub legacy_ack_token_input: String,
    /// Live sender state (`Idle`, `Connecting`, `Sending`, `WaitingForAck`, `Paused`, ...).
    pub sender_state: SenderState,
    /// Current 1-based record index being sent (`NET-22`).
    pub progress_current: usize,
    /// Total records in current send batch (`NET-22`).
    pub progress_total: usize,
    /// Current attempt counter for active record (`NET-22`).
    pub progress_attempts: u32,
    /// Cumulative connection reset cycles (`D-18`).
    pub progress_resets: u32,
    /// Last per-record outcome readout (`NET-22`).
    pub last_result: String,
    /// Show received ACKs in the ACK viewer (`NET-27`).
    pub view_received_acks: bool,
    /// Also pop up a modal window on each received ACK/NAK (`NET-27`).
    pub modal_ack_popups: bool,
    /// Bounded ring buffer of received ACK/NAK and malformed-packet entries (`NET-27`).
    pub ack_log: VecDeque<AckViewerEntry>,
    /// Maximum in-memory entries retained in `ack_log` (defaults to [`MAX_ACK_LOG_ENTRIES`]).
    pub ack_log_capacity: usize,
    /// Total ACK/NAK/malformed responses observed (including entries rolled off the ring buffer).
    pub total_ack_responses: u64,
    /// Total positive ACKs observed.
    pub total_acked: u64,
    /// Total malformed/unexpected packets observed across sender and listener.
    pub total_malformed: u64,
    /// Number of oldest entries evicted from `ack_log` due to ring-buffer capacity.
    pub ack_log_dropped_oldest: u64,
    /// Optional modal ACK/NAK alert (`NET-27`).
    pub modal_ack_alert: Option<AckViewerEntry>,
    /// Bounded ring buffer of send history log entries (`NET-33`).
    pub send_history: VecDeque<SendHistoryEntry>,
    /// Maximum in-memory entries retained in `send_history` (defaults to [`MAX_SEND_HISTORY_ENTRIES`]).
    pub send_history_capacity: usize,
    /// Total send history entries recorded (including entries rolled off).
    pub total_send_history: u64,
    /// Number of oldest entries evicted from `send_history`.
    pub send_history_dropped_oldest: u64,
    /// Status or error message shown in the sender window.
    pub sender_status_or_error: Option<String>,
    sender_cmd_tx: Option<mpsc::UnboundedSender<SenderCommand>>,
    sender_ev_rx: Option<mpsc::UnboundedReceiver<SenderEvent>>,

    // --- Listener (NET-10..NET-17) ---
    /// Whether the Listener panel window is open.
    pub listener_open: bool,
    /// Active listener profile.
    pub listener_profile: ListenerProfile,
    /// Digits-only UI buffer for listener port (`1..=65535`).
    pub listener_port_input: String,
    /// Editable envelope inputs for the listener.
    pub listener_envelope_ui: EnvelopeInputState,
    /// `true` while the listener task is actively bound and listening.
    pub listener_running: bool,
    /// Auto-scroll main record list when new TCP/IP records arrive (`NET-13`).
    pub auto_scroll_received: bool,
    /// Set to `true` when user clicks Start on a non-loopback bind address.
    pub pending_non_loopback_confirm: bool,
    /// Active and historical client connections (`NET-12`).
    pub connections: Vec<PeerConnectionRow>,
    /// Total records received across all connections (`NET-13`).
    pub total_received_count: u64,
    /// Status or error message shown in the listener window.
    pub listener_status_or_error: Option<String>,
    listener_cmd_tx: Option<mpsc::UnboundedSender<ListenerCommand>>,
    listener_ev_rx: Option<mpsc::UnboundedReceiver<ListenerEvent>>,
}

impl Default for NetworkPanelState {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkPanelState {
    /// Construct a new network panel manager with a dedicated background Tokio runtime (`G-5`).
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .or_else(|_| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
            })
            .ok()
            .map(Arc::new);

        let sender_profile = SenderProfile::default();
        let listener_profile = ListenerProfile::default();

        Self {
            runtime,
            docked_open: false,
            active_tab: TcpIpDockTab::ClientSender,
            sender_open: false,
            sender_port_input: sender_profile.port.to_string(),
            sender_connect_timeout_input: format_clean_f64(sender_profile.connect_timeout_seconds),
            sender_delay_connect_input: format_clean_f64(
                sender_profile.delay_after_connect_seconds,
            ),
            sender_delay_between_input: format_clean_f64(
                sender_profile.delay_between_records_seconds,
            ),
            sender_ack_wait_input: format_clean_f64(sender_profile.ack_wait_seconds),
            sender_max_tries_input: sender_profile.max_tries.to_string(),
            range_start_input: "1".to_string(),
            range_end_input: "1".to_string(),
            sender_envelope_ui: EnvelopeInputState::from_envelope(sender_profile.envelope),
            sender_profile,
            send_scope_mode: SendScopeUiMode::All,
            range_start: 1,
            range_end: 1,
            simple_ack_hex_input: "0b 06 1c 0d".to_string(),
            legacy_ack_token_input: "MSA|AA".to_string(),
            sender_state: SenderState::Idle,
            progress_current: 0,
            progress_total: 0,
            progress_attempts: 0,
            progress_resets: 0,
            last_result: "Idle".to_string(),
            view_received_acks: true,
            modal_ack_popups: false,
            ack_log: VecDeque::new(),
            ack_log_capacity: MAX_ACK_LOG_ENTRIES,
            total_ack_responses: 0,
            total_acked: 0,
            total_malformed: 0,
            ack_log_dropped_oldest: 0,
            modal_ack_alert: None,
            send_history: VecDeque::new(),
            send_history_capacity: MAX_SEND_HISTORY_ENTRIES,
            total_send_history: 0,
            send_history_dropped_oldest: 0,
            sender_status_or_error: None,
            sender_cmd_tx: None,
            sender_ev_rx: None,

            listener_open: false,
            listener_port_input: listener_profile.port.to_string(),
            listener_envelope_ui: EnvelopeInputState::from_envelope(listener_profile.envelope),
            listener_profile,
            listener_running: false,
            auto_scroll_received: true,
            pending_non_loopback_confirm: false,
            connections: Vec::new(),
            total_received_count: 0,
            listener_status_or_error: None,
            listener_cmd_tx: None,
            listener_ev_rx: None,
        }
    }

    /// Append an entry to the bounded `ack_log` ring buffer, evicting the oldest entry if full
    /// while keeping cumulative counters accurate.
    pub fn push_ack_log_entry(&mut self, entry: AckViewerEntry) {
        self.total_ack_responses = self.total_ack_responses.saturating_add(1);
        if entry.is_malformed {
            self.total_malformed = self.total_malformed.saturating_add(1);
        } else if entry.is_ack {
            self.total_acked = self.total_acked.saturating_add(1);
        }
        let cap = self.ack_log_capacity.max(1);
        while self.ack_log.len() >= cap {
            self.ack_log.pop_front();
            self.ack_log_dropped_oldest = self.ack_log_dropped_oldest.saturating_add(1);
        }
        self.ack_log.push_back(entry);
    }

    /// Clear `ack_log` and reset its cumulative response counters.
    pub fn clear_ack_log(&mut self) {
        self.ack_log.clear();
        self.total_ack_responses = 0;
        self.total_acked = 0;
        self.total_malformed = 0;
        self.ack_log_dropped_oldest = 0;
    }

    /// Append a send history entry to the bounded `send_history` ring buffer.
    pub fn push_send_history_entry(&mut self, entry: SendHistoryEntry) {
        self.total_send_history = self.total_send_history.saturating_add(1);
        let cap = self.send_history_capacity.max(1);
        while self.send_history.len() >= cap {
            self.send_history.pop_front();
            self.send_history_dropped_oldest = self.send_history_dropped_oldest.saturating_add(1);
        }
        self.send_history.push_back(entry);
    }

    /// Returns `true` if `addr` is a loopback interface (`127.0.0.1`, `localhost`, `::1`).
    pub fn is_loopback_address(addr: &str) -> bool {
        let trimmed = addr.trim();
        trimmed.eq_ignore_ascii_case("localhost") || trimmed.starts_with("127.") || trimmed == "::1"
    }

    /// Build the [`SendScope`] from current UI selections (`NET-21`).
    pub fn build_send_scope(&self, selected_records: &BTreeSet<usize>) -> SendScope {
        match self.send_scope_mode {
            SendScopeUiMode::All => SendScope::All,
            SendScopeUiMode::Selected => {
                SendScope::Selected(selected_records.iter().copied().collect())
            }
            SendScopeUiMode::Range => SendScope::Range {
                start: self.range_start,
                end: self.range_end,
            },
        }
    }

    /// Sync all Sender text input buffers from `self.sender_profile` (e.g. after loading a profile).
    pub fn sync_sender_inputs_from_profile(&mut self) {
        self.sender_port_input = self.sender_profile.port.to_string();
        self.sender_connect_timeout_input =
            format_clean_f64(self.sender_profile.connect_timeout_seconds);
        self.sender_delay_connect_input =
            format_clean_f64(self.sender_profile.delay_after_connect_seconds);
        self.sender_delay_between_input =
            format_clean_f64(self.sender_profile.delay_between_records_seconds);
        self.sender_ack_wait_input = format_clean_f64(self.sender_profile.ack_wait_seconds);
        self.sender_max_tries_input = self.sender_profile.max_tries.to_string();
        self.sender_envelope_ui = EnvelopeInputState::from_envelope(self.sender_profile.envelope);
    }

    /// Sync all Listener text input buffers from `self.listener_profile` (e.g. after loading a profile).
    pub fn sync_listener_inputs_from_profile(&mut self) {
        self.listener_port_input = self.listener_profile.port.to_string();
        self.listener_envelope_ui =
            EnvelopeInputState::from_envelope(self.listener_profile.envelope);
    }

    /// Validate all MLLP Client Sender form inputs (host, port `1..=65535`, timeouts `> 0`,
    /// delays `>= 0`, max tries `1..=100`, range bounds, and MLLP envelope bytes).
    pub fn validate_sender_inputs(&self) -> SenderValidation {
        let mut v = SenderValidation::default();

        if self.sender_profile.host.trim().is_empty() {
            v.host = Some("Host / IP cannot be empty.".to_string());
        }
        if parse_valid_port(&self.sender_port_input).is_none() {
            v.port = Some("Port must be a number between 1 and 65535.".to_string());
        }
        if parse_bounded_f64(&self.sender_connect_timeout_input, 0.001, 300.0).is_none() {
            v.connect_timeout =
                Some("Connect timeout must be greater than 0 (up to 300s).".to_string());
        }
        if parse_bounded_f64(&self.sender_delay_connect_input, 0.0, 3600.0).is_none() {
            v.delay_after_connect =
                Some("Delay after connect must be between 0 and 3600s.".to_string());
        }
        if parse_bounded_f64(&self.sender_delay_between_input, 0.0, 3600.0).is_none() {
            v.delay_between_records =
                Some("Delay between records must be between 0 and 3600s.".to_string());
        }
        if self.sender_profile.wait_for_ack {
            if parse_bounded_f64(&self.sender_ack_wait_input, 0.001, 600.0).is_none() {
                v.ack_wait = Some("ACK max wait must be greater than 0 (up to 600s).".to_string());
            }
            if parse_bounded_u32(&self.sender_max_tries_input, 1, 100).is_none() {
                v.max_tries = Some("Max tries must be an integer between 1 and 100.".to_string());
            }
        }
        if self.send_scope_mode == SendScopeUiMode::Range {
            match (
                parse_bounded_usize(&self.range_start_input, 1, 1_000_000),
                parse_bounded_usize(&self.range_end_input, 1, 1_000_000),
            ) {
                (Some(s), Some(e)) if s <= e => {}
                (Some(_), Some(_)) => {
                    v.range = Some("Range start must be <= range end.".to_string());
                }
                _ => {
                    v.range = Some("Range start and end must be integers >= 1.".to_string());
                }
            }
        }
        if let Err(e) = self.sender_envelope_ui.parse_envelope() {
            v.envelope = Some(e);
        }
        v
    }

    /// Validate all MLLP Server Listener form inputs (bind address, port `1..=65535`, envelope).
    pub fn validate_listener_inputs(&self) -> ListenerValidation {
        let mut v = ListenerValidation::default();
        if self.listener_profile.bind_address.trim().is_empty() {
            v.bind_address = Some("Bind address cannot be empty.".to_string());
        }
        if parse_valid_port(&self.listener_port_input).is_none() {
            v.port = Some("Port must be a number between 1 and 65535.".to_string());
        }
        if let Err(e) = self.listener_envelope_ui.parse_envelope() {
            v.envelope = Some(e);
        }
        v
    }

    /// Validate settings and launch the async MLLP sender task (`NET-20..NET-34`).
    pub fn start_sender(
        &mut self,
        doc_records: &[Record],
        selected_records: &BTreeSet<usize>,
        ctx: Option<Context>,
    ) -> Result<(), String> {
        let trimmed_host = self.sender_profile.host.trim().to_string();
        if trimmed_host.is_empty() {
            return Err("Host / IP cannot be empty.".to_string());
        }
        self.sender_profile.host = trimmed_host;

        if self.sender_profile.port == 0 {
            return Err("Port must be a number between 1 and 65535.".to_string());
        }
        if !self.sender_profile.connect_timeout_seconds.is_finite()
            || self.sender_profile.connect_timeout_seconds <= 0.0
        {
            return Err("Connect timeout must be greater than 0.".to_string());
        }
        if self.sender_profile.wait_for_ack
            && (!self.sender_profile.ack_wait_seconds.is_finite()
                || self.sender_profile.ack_wait_seconds <= 0.0)
        {
            return Err("ACK max wait must be greater than 0.".to_string());
        }
        if self.sender_profile.wait_for_ack && self.sender_profile.max_tries == 0 {
            return Err("Max tries must be at least 1.".to_string());
        }

        let env = self.sender_envelope_ui.parse_envelope()?;
        self.sender_profile.envelope = env;

        let scope = self.build_send_scope(selected_records);
        let indices = resolve_send_scope(&scope, doc_records.len()).map_err(|e| e.to_string())?;
        if indices.is_empty() {
            return Err("No records match the chosen send scope.".to_string());
        }

        let payloads: Vec<Vec<u8>> = indices
            .into_iter()
            .filter_map(|i| doc_records.get(i).map(|r| r.bytes().to_vec()))
            .collect();

        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        self.progress_current = 0;
        self.progress_total = payloads.len();
        self.progress_attempts = 0;
        self.progress_resets = 0;
        self.last_result = "Starting...".to_string();
        self.sender_status_or_error = None;
        self.sender_cmd_tx = Some(cmd_tx);

        // Bridge background events so `ctx.request_repaint()` wakes egui immediately (G-5).
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
        self.sender_ev_rx = Some(bridge_rx);

        let Some(ref rt) = self.runtime else {
            return Err("Async runtime unavailable.".to_string());
        };
        let profile_clone = self.sender_profile.clone();
        rt.spawn(async move {
            let mut inner_rx = ev_rx;
            let sender_handle = tokio::spawn(run_sender(profile_clone, payloads, ev_tx, cmd_rx));
            while let Some(ev) = inner_rx.recv().await {
                let _ = bridge_tx.send(ev);
                if let Some(ref c) = ctx {
                    c.request_repaint();
                }
            }
            let _ = sender_handle.await;
        });

        Ok(())
    }

    /// Request the running sender to pause after the current in-flight operation (`NET-22`).
    pub fn pause_sender(&mut self) {
        if let Some(ref tx) = self.sender_cmd_tx {
            let _ = tx.send(SenderCommand::Pause);
        }
    }

    /// Resume a paused sender (`NET-22`).
    pub fn resume_sender(&mut self) {
        if let Some(ref tx) = self.sender_cmd_tx {
            let _ = tx.send(SenderCommand::Resume);
        }
    }

    /// Immediately stop the running sender (`NET-22`, `NET-25`).
    pub fn stop_sender(&mut self) {
        if let Some(ref tx) = self.sender_cmd_tx {
            let _ = tx.send(SenderCommand::Stop);
        }
    }

    /// Start the MLLP listener, gating non-loopback binds on explicit user confirmation (`NET-11`).
    ///
    /// Returns `Ok(true)` if started, or `Ok(false)` if waiting on the non-loopback confirmation modal.
    pub fn request_start_listener(
        &mut self,
        force_non_loopback: bool,
        ctx: Option<Context>,
    ) -> Result<bool, String> {
        let trimmed_bind = self.listener_profile.bind_address.trim().to_string();
        if trimmed_bind.is_empty() {
            return Err("Bind address cannot be empty.".to_string());
        }
        self.listener_profile.bind_address = trimmed_bind;
        if self.listener_profile.port == 0 {
            return Err("Port must be a number between 1 and 65535.".to_string());
        }
        let env = self.listener_envelope_ui.parse_envelope()?;
        self.listener_profile.envelope = env;

        if !force_non_loopback && !Self::is_loopback_address(&self.listener_profile.bind_address) {
            self.pending_non_loopback_confirm = true;
            return Ok(false);
        }
        self.pending_non_loopback_confirm = false;

        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();

        self.listener_cmd_tx = Some(cmd_tx);
        self.listener_ev_rx = Some(bridge_rx);
        self.listener_status_or_error = None;

        let Some(ref rt) = self.runtime else {
            return Err("Async runtime unavailable.".to_string());
        };
        let profile_clone = self.listener_profile.clone();
        rt.spawn(async move {
            let mut inner_rx = ev_rx;
            let listener_handle = tokio::spawn(run_listener_server(profile_clone, ev_tx, cmd_rx));
            while let Some(ev) = inner_rx.recv().await {
                let _ = bridge_tx.send(ev);
                if let Some(ref c) = ctx {
                    c.request_repaint();
                }
            }
            let _ = listener_handle.await;
        });

        Ok(true)
    }

    /// Stop the running MLLP listener (`NET-11`).
    pub fn stop_listener(&mut self) {
        if let Some(ref tx) = self.listener_cmd_tx {
            let _ = tx.send(ListenerCommand::Stop);
        }
        self.listener_running = false;
    }

    /// Poll sender and listener event channels without blocking (`G-5`), returning any newly
    /// received HL7 [`Record`]s so the session appends them with source `"TCP/IP Listener"` (`NET-13`).
    pub fn poll_events(&mut self) -> Vec<Record> {
        let mut sender_events = Vec::new();
        if let Some(ref mut rx) = self.sender_ev_rx {
            while let Ok(ev) = rx.try_recv() {
                sender_events.push(ev);
            }
        }
        for ev in sender_events {
            match ev {
                SenderEvent::StateChanged(st) => {
                    self.sender_state = st;
                }
                SenderEvent::Progress {
                    current,
                    total,
                    attempts,
                    resets,
                } => {
                    self.progress_current = current;
                    self.progress_total = total;
                    self.progress_attempts = attempts;
                    self.progress_resets = resets;
                }
                SenderEvent::AckReceived {
                    record_index,
                    control_id,
                    response,
                    is_ack,
                    peer_addr,
                } => {
                    let heading = if is_ack {
                        strings::MSG_RECEIVED_ACK.to_string()
                    } else {
                        strings::MSG_RECEIVED_NAK.to_string()
                    };
                    let peer_label = peer_addr.to_string();
                    self.last_result = if is_ack {
                        format!(
                            "Message #{}: ACK ({control_id}) from {peer_label}",
                            record_index + 1
                        )
                    } else {
                        format!(
                            "Message #{}: NAK ({control_id}) from {peer_label}",
                            record_index + 1
                        )
                    };
                    let payload_text = String::from_utf8_lossy(&response)
                        .replace('\r', "\n")
                        .trim()
                        .to_string();
                    let entry = AckViewerEntry {
                        heading,
                        record_index,
                        control_id,
                        timestamp: current_hl7_timestamp(),
                        peer_addr: Some(peer_label),
                        payload_text,
                        is_ack,
                        is_malformed: false,
                        failure_reason: None,
                    };
                    if self.view_received_acks {
                        if self.modal_ack_popups {
                            self.modal_ack_alert = Some(entry.clone());
                        }
                        self.push_ack_log_entry(entry);
                    }
                }
                SenderEvent::MalformedPacket {
                    record_index,
                    control_id,
                    info,
                } => {
                    let peer_label = match &info.configured_host {
                        Some(host) => format!("{} ({host})", info.peer_addr),
                        None => info.peer_addr.to_string(),
                    };
                    self.last_result = format!(
                        "Message #{}: Malformed packet from {peer_label} — {}",
                        record_index + 1,
                        info.reason
                    );
                    self.sender_status_or_error = Some(self.last_result.clone());
                    let entry = AckViewerEntry {
                        heading: "Malformed / Unexpected MLLP Response".to_string(),
                        record_index,
                        control_id,
                        timestamp: info.timestamp,
                        peer_addr: Some(peer_label),
                        payload_text: info.raw_preview,
                        is_ack: false,
                        is_malformed: true,
                        failure_reason: Some(info.reason),
                    };
                    self.push_ack_log_entry(entry);
                }
                SenderEvent::RecordSkipped {
                    record_index,
                    reason,
                } => {
                    self.last_result = format!("Message #{} skipped: {reason}", record_index + 1);
                }
                SenderEvent::ConnectionReset {
                    record_index,
                    reset_count,
                } => {
                    self.progress_resets = reset_count;
                    self.last_result = format!(
                        "Message #{}: connection reset (#{reset_count})",
                        record_index + 1
                    );
                }
                SenderEvent::History(h) => {
                    self.push_send_history_entry(h);
                }
                SenderEvent::StatusMessage(msg) => {
                    self.last_result = msg.clone();
                    self.sender_status_or_error = Some(msg);
                }
                SenderEvent::Finished { sent, skipped } => {
                    self.sender_state = SenderState::Idle;
                    let summary = format!("Finished: {sent} sent, {skipped} skipped.");
                    if self.sender_status_or_error.is_none() {
                        self.sender_status_or_error = Some(summary.clone());
                    }
                    self.last_result = summary;
                }
            }
        }

        let mut received_records = Vec::new();
        let mut listener_events = Vec::new();
        if let Some(ref mut rx) = self.listener_ev_rx {
            while let Ok(ev) = rx.try_recv() {
                listener_events.push(ev);
            }
        }
        for ev in listener_events {
            match ev {
                ListenerEvent::Started { port } => {
                    self.listener_running = true;
                    self.listener_status_or_error = Some(format!("Listening on port {port}."));
                }
                ListenerEvent::ClientConnected { addr, conn_id } => {
                    self.connections.push(PeerConnectionRow {
                        conn_id,
                        peer_addr: addr.to_string(),
                        active: true,
                        messages_received: 0,
                        bytes_received: 0,
                        last_error: None,
                    });
                }
                ListenerEvent::ClientDisconnected {
                    conn_id, messages, ..
                } => {
                    if let Some(row) = self.connections.iter_mut().find(|c| c.conn_id == conn_id) {
                        row.active = false;
                        row.messages_received = messages;
                    }
                }
                ListenerEvent::RecordReceived {
                    record, raw, peer, ..
                } => {
                    self.total_received_count += 1;
                    let peer_str = peer.to_string();
                    if let Some(row) = self
                        .connections
                        .iter_mut()
                        .rev()
                        .find(|c| c.peer_addr == peer_str && c.active)
                    {
                        row.messages_received += 1;
                        row.bytes_received += raw.len() as u64;
                    }
                    received_records.push(record);
                }
                ListenerEvent::MalformedPacket { conn_id, info } => {
                    let peer_label = info.peer_addr.to_string();
                    let summary = format!("Malformed packet from {peer_label}: {}", info.reason);
                    if let Some(row) = self.connections.iter_mut().find(|c| c.conn_id == conn_id) {
                        row.last_error = Some(info.reason.clone());
                    }
                    self.listener_status_or_error = Some(summary);
                    self.push_ack_log_entry(AckViewerEntry {
                        heading: "Malformed / Unexpected Inbound Packet".to_string(),
                        record_index: 0,
                        control_id: format!("Conn #{conn_id}"),
                        timestamp: info.timestamp,
                        peer_addr: Some(peer_label),
                        payload_text: info.raw_preview,
                        is_ack: false,
                        is_malformed: true,
                        failure_reason: Some(info.reason),
                    });
                }
                ListenerEvent::ClientError { conn_id, error } => {
                    if let Some(row) = self.connections.iter_mut().find(|c| c.conn_id == conn_id) {
                        row.last_error = Some(error.clone());
                    }
                    self.listener_status_or_error = Some(error);
                }
                ListenerEvent::WarningMessage(msg) => {
                    self.listener_status_or_error = Some(msg);
                }
                ListenerEvent::FatalError(err) => {
                    self.listener_running = false;
                    self.listener_status_or_error = Some(err);
                }
                ListenerEvent::Stopped => {
                    self.listener_running = false;
                    self.listener_status_or_error = Some("Listener stopped.".to_string());
                }
            }
        }

        received_records
    }

    /// Export the send history log as RFC 4180 CSV text (`NET-33`).
    pub fn format_history_csv(&self) -> String {
        let mut out =
            String::from("record_index,control_id,timestamp,outcome,rtt_ms,attempt,peer_addr\r\n");
        for entry in &self.send_history {
            let peer = entry
                .peer_addr
                .map_or_else(String::new, |addr| addr.to_string());
            out.push_str(&format!(
                "{},{},{},{},{},{},{}\r\n",
                entry.record_index + 1,
                entry.control_id,
                entry.timestamp,
                entry.outcome,
                entry.rtt_ms,
                entry.attempt,
                peer
            ));
        }
        out
    }

    /// Toggle the docked TCP/IP section open or closed (`D-26`).
    pub fn toggle_dock(&mut self) {
        self.docked_open = !self.docked_open;
        self.sender_open = self.docked_open;
        self.listener_open = self.docked_open;
    }

    /// Open the docked TCP/IP section on a specific tab (`D-26`).
    pub fn open_dock_tab(&mut self, tab: TcpIpDockTab) {
        self.docked_open = true;
        self.active_tab = tab;
        self.sender_open = true;
        self.listener_open = true;
    }
}

/// Render the docked TCP/IP section inside a main-window panel (`D-26`).
pub fn show_docked_tcpip_panel(
    ui: &mut Ui,
    ctx: &Context,
    net: &mut NetworkPanelState,
    doc_records: &[Record],
    selected_records: &BTreeSet<usize>,
) {
    let palette = ThemePalette::for_visuals(ui.visuals());
    ui.scope(|ui| {
        apply_unified_control_metrics(ui);

        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP/IP (MLLP & TLS 1.2/1.3)").size(15.0));
            ui.separator();

            if icon_toggle_labeled_button(
                ui,
                IconKind::Send,
                "Client Sender",
                net.active_tab == TcpIpDockTab::ClientSender,
                true,
                palette.accent,
            )
            .on_hover_text("Configure and send HL7 messages outbound over MLLP or TLS 1.2/1.3")
            .clicked()
            {
                net.active_tab = TcpIpDockTab::ClientSender;
            }

            let listener_label = if net.listener_running {
                "Server Listener (Listening)"
            } else {
                "Server Listener"
            };
            let listener_accent = if net.listener_running {
                palette.accent_green
            } else {
                palette.accent
            };
            if icon_toggle_labeled_button(
                ui,
                IconKind::Listen,
                listener_label,
                net.active_tab == TcpIpDockTab::ServerListener,
                true,
                listener_accent,
            )
            .on_hover_text(
                "Run a multi-connection inbound MLLP / TLS server to receive HL7 messages and send ACKs",
            )
            .clicked()
            {
                net.active_tab = TcpIpDockTab::ServerListener;
            }

            let conn_label = format!("Connections ({})", net.connections.len());
            if icon_toggle_labeled_button(
                ui,
                IconKind::Network,
                &conn_label,
                net.active_tab == TcpIpDockTab::Connections,
                true,
                palette.accent,
            )
            .on_hover_text("Inspect active and historical peer connections to the MLLP listener")
            .clicked()
            {
                net.active_tab = TcpIpDockTab::Connections;
            }

            let ack_label = format!("ACK Log ({})", net.ack_log.len());
            if icon_toggle_labeled_button(
                ui,
                IconKind::Log,
                &ack_label,
                net.active_tab == TcpIpDockTab::AckLog,
                true,
                palette.accent,
            )
            .on_hover_text(
                "View received ACK, NAK, and malformed MLLP packets and export send history CSV",
            )
            .clicked()
            {
                net.active_tab = TcpIpDockTab::AckLog;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_labeled_button(ui, IconKind::Close, "Close", true, None)
                    .on_hover_text("Close the docked TCP/IP panel")
                    .clicked()
                {
                    net.docked_open = false;
                    net.sender_open = false;
                    net.listener_open = false;
                }
            });
        });
        ui.separator();

        ScrollArea::vertical()
            .id_salt("docked_tcpip_body_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match net.active_tab {
                TcpIpDockTab::ClientSender => {
                    show_sender_tab_ui(ui, ctx, net, doc_records, selected_records);
                }
                TcpIpDockTab::ServerListener => {
                    show_listener_tab_ui(ui, ctx, net);
                }
                TcpIpDockTab::Connections => {
                    show_connections_tab_ui(ui, net);
                }
                TcpIpDockTab::AckLog => {
                    show_ack_log_tab_ui(ui, net);
                }
            });
    });
}

/// Render only confirmation/alert modals for the network subsystem (no floating Sender/Listener windows per `D-26`).
pub fn show_network_modals(ctx: &Context, net: &mut NetworkPanelState) {
    show_non_loopback_confirm_window(ctx, net);
    show_modal_ack_alert_window(ctx, net);
}

fn nak_action_label(action: NakAction) -> &'static str {
    match action {
        NakAction::Resend => "Resend record",
        NakAction::ResetAndResend => "Reset connection & resend",
        NakAction::Skip => "Skip record",
    }
}

fn max_tries_action_label(action: MaxTriesAction) -> &'static str {
    match action {
        MaxTriesAction::ResetAndResend => "Reset connection & resend",
        MaxTriesAction::Skip => "Skip record",
        MaxTriesAction::CloseAndStop => "Close & stop",
    }
}

fn show_tls_header_label(ui: &mut Ui, enabled: bool, palette: &ThemePalette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ICON_SIZE_INLINE, ICON_SIZE_INLINE),
            egui::Sense::hover(),
        );
        let fg = if enabled {
            palette.accent_green
        } else {
            ui.visuals().text_color()
        };
        paint_icon(
            ui.painter(),
            IconKind::Lock,
            rect.center(),
            fg,
            ui.visuals().panel_fill,
            ICON_SIZE_INLINE,
        );
        let suffix = if enabled { " (Enabled)" } else { "" };
        ui.strong(format!("TLS 1.2 / 1.3 & Mutual TLS (mTLS){suffix}"));
    });
}

fn show_envelope_controls(ui: &mut Ui, id_salt: &str, env_ui: &mut EnvelopeInputState) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Envelope (Hex/Dec/Oct) — SB:")
            .on_hover_text("MLLP framing wrapper bytes: Start Block (SB), End Data (ED), and End Block (EB)");
        ui.add(styled_singleline_text_edit(&mut env_ui.sb_input, 54.0, Some("0x0B")))
            .on_hover_text(
                "Start Block (SB) framing byte prepended before each HL7 message (default 0x0B / VT). Accepts Hex (0x0B), Decimal (11), or Octal (0o13).",
            );
        ui.label("ED:");
        ui.add(styled_singleline_text_edit(&mut env_ui.ed_input, 54.0, Some("0x1C")))
            .on_hover_text(
                "End Data (ED) framing byte appended after each HL7 message (default 0x1C / FS). Accepts Hex (0x1C), Decimal (28), or Octal (0o34).",
            );
        ui.label("EB:");
        ui.add(styled_singleline_text_edit(&mut env_ui.eb_input, 54.0, Some("0x0D")))
            .on_hover_text(
                "End Block (EB) trailing byte after End Data (default 0x0D / CR). Accepts Hex (0x0D), Decimal (13), or Octal (0o15).",
            );
        if icon_labeled_button(ui, IconKind::Revert, "Defaults", true, None)
            .on_hover_text("Restore standard MLLP framing bytes (0x0B / 0x1C / 0x0D)")
            .clicked()
        {
            env_ui.reset_defaults();
        }
        let _ = id_salt;
    });
}

fn show_sender_tab_ui(
    ui: &mut Ui,
    ctx: &Context,
    net: &mut NetworkPanelState,
    doc_records: &[Record],
    selected_records: &BTreeSet<usize>,
) {
    let mut start_clicked = false;
    let mut pause_clicked = false;
    let mut resume_clicked = false;
    let mut stop_clicked = false;
    let mut save_prof = false;
    let mut load_prof = false;

    // Profile bar (NET-20)
    ui.horizontal_wrapped(|ui| {
        ui.label("Profile:");
        ui.add(styled_singleline_text_edit(
            &mut net.sender_profile.name,
            180.0,
            Some("Profile name"),
        ))
        .on_hover_text("Name used when saving or loading this Client Sender profile as TOML");
        if icon_labeled_button(ui, IconKind::Save, "Save Profile...", true, None)
            .on_hover_text("Save the current Client Sender configuration to a .toml profile file")
            .clicked()
        {
            save_prof = true;
        }
        if icon_labeled_button(ui, IconKind::Open, "Load Profile...", true, None)
            .on_hover_text("Load a saved Client Sender .toml profile file")
            .clicked()
        {
            load_prof = true;
        }
    });
    ui.separator();

    let palette = ThemePalette::for_visuals(ui.visuals());
    let validation = net.validate_sender_inputs();

    // Target & Envelope (NET-1, NET-20, NET-23, NET-24)
    ui.horizontal_wrapped(|ui| {
        ui.label("Host / IP:");
        if validated_text_edit(
            ui,
            &mut net.sender_profile.host,
            165.0,
            Some("127.0.0.1"),
            NumericFilter::None,
            validation.host.as_deref(),
            "Destination hostname or IP address of the remote MLLP receiver (e.g. 127.0.0.1 or engine.local)",
            &palette,
        )
        .lost_focus()
        {
            let trimmed = net.sender_profile.host.trim().to_string();
            net.sender_profile.host = trimmed;
        }
        ui.label("Port:");
        if validated_text_edit(
            ui,
            &mut net.sender_port_input,
            68.0,
            Some("7772"),
            NumericFilter::DigitsOnly,
            validation.port.as_deref(),
            "Destination TCP port on the remote MLLP receiver (1–65535)",
            &palette,
        )
        .changed()
        {
            if let Some(p) = parse_valid_port(&net.sender_port_input) {
                net.sender_profile.port = p;
            }
        }
        ui.label("Connect Timeout (s):");
        if validated_text_edit(
            ui,
            &mut net.sender_connect_timeout_input,
            60.0,
            Some("10"),
            NumericFilter::UnsignedDecimal,
            validation.connect_timeout.as_deref(),
            "Maximum seconds to wait for the TCP/TLS connection handshake before failing (> 0, up to 300s)",
            &palette,
        )
        .changed()
        {
            if let Some(val) = parse_bounded_f64(&net.sender_connect_timeout_input, 0.001, 300.0) {
                net.sender_profile.connect_timeout_seconds = val;
            }
        }
    });

    show_envelope_controls(ui, "sender_env", &mut net.sender_envelope_ui);

    ui.horizontal_wrapped(|ui| {
        ui.label("Delay after connect (s):");
        if validated_text_edit(
            ui,
            &mut net.sender_delay_connect_input,
            60.0,
            Some("0"),
            NumericFilter::UnsignedDecimal,
            validation.delay_after_connect.as_deref(),
            "Optional pause in seconds after connecting before transmitting the first HL7 message (0–3600s)",
            &palette,
        )
        .changed()
        {
            if let Some(val) = parse_bounded_f64(&net.sender_delay_connect_input, 0.0, 3600.0) {
                net.sender_profile.delay_after_connect_seconds = val;
            }
        }
        ui.label("Delay between records (s):");
        if validated_text_edit(
            ui,
            &mut net.sender_delay_between_input,
            60.0,
            Some("0"),
            NumericFilter::UnsignedDecimal,
            validation.delay_between_records.as_deref(),
            "Optional throttle delay in seconds between consecutive messages in the batch (0–3600s)",
            &palette,
        )
        .changed()
        {
            if let Some(val) = parse_bounded_f64(&net.sender_delay_between_input, 0.0, 3600.0) {
                net.sender_profile.delay_between_records_seconds = val;
            }
        }
    });

    ui.separator();

    // Send Scope (NET-21)
    ui.horizontal_wrapped(|ui| {
        ui.label("Send Scope:");
        ui.radio_value(
            &mut net.send_scope_mode,
            SendScopeUiMode::All,
            "All Records",
        )
        .on_hover_text("Transmit every HL7 message currently loaded in the session");
        ui.radio_value(
            &mut net.send_scope_mode,
            SendScopeUiMode::Selected,
            "Selected Only",
        )
        .on_hover_text("Transmit only the message(s) currently selected in the left message list");
        ui.radio_value(
            &mut net.send_scope_mode,
            SendScopeUiMode::Range,
            "Range (1-based):",
        )
        .on_hover_text("Transmit an inclusive 1-based slice of messages by list position");
        if net.send_scope_mode == SendScopeUiMode::Range {
            if validated_text_edit(
                ui,
                &mut net.range_start_input,
                60.0,
                Some("1"),
                NumericFilter::DigitsOnly,
                validation.range.as_deref(),
                "First 1-based message number in the range to send",
                &palette,
            )
            .changed()
            {
                if let Some(s) = parse_bounded_usize(&net.range_start_input, 1, 1_000_000) {
                    net.range_start = s;
                }
            }
            ui.label("to");
            if validated_text_edit(
                ui,
                &mut net.range_end_input,
                60.0,
                Some("1"),
                NumericFilter::DigitsOnly,
                validation.range.as_deref(),
                "Last 1-based message number in the range to send (inclusive)",
                &palette,
            )
            .changed()
            {
                if let Some(e) = parse_bounded_usize(&net.range_end_input, 1, 1_000_000) {
                    net.range_end = e;
                }
            }
        }
    });

    // Outgoing MSH-10 / MSH-7 rewrite toggles (NET-31, NET-32)
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(
            &mut net.sender_profile.rewrite_control_id,
            "Rewrite outgoing MSH-10 Control ID",
        )
        .on_hover_text(
            "Generate a unique outgoing MSH-10 Message Control ID per send so downstream engines do not reject replayed messages as duplicates",
        );
        ui.checkbox(
            &mut net.sender_profile.refresh_timestamp,
            "Refresh outgoing MSH-7 Timestamp",
        )
        .on_hover_text(
            "Overwrite outgoing MSH-7 Date/Time of Message with the current local timestamp at send time (without modifying the loaded session copy)",
        );
    });

    ui.separator();

    // ACK & Retry Policy (NET-25..NET-29)
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut net.sender_profile.wait_for_ack, "Wait for ACK")
            .on_hover_text(
                "Wait for an MLLP acknowledgement response after each message before sending the next",
            );
        if net.sender_profile.wait_for_ack {
            ui.checkbox(
                &mut net.sender_profile.respect_ack_modes,
                "Respect MSH-15 / MSH-16 ACK rules",
            )
            .on_hover_text(
                "Honor HL7 Enhanced Mode rules: skip waiting for an ACK when outgoing MSH-15/16 is NE instead of timing out",
            );
            ui.label("Max Wait (s):");
            if validated_text_edit(
                ui,
                &mut net.sender_ack_wait_input,
                60.0,
                Some("30"),
                NumericFilter::UnsignedDecimal,
                validation.ack_wait.as_deref(),
                "Maximum seconds to wait for an ACK/NAK response before treating the send as timed out (> 0, up to 600s)",
                &palette,
            )
            .changed()
            {
                if let Some(val) = parse_bounded_f64(&net.sender_ack_wait_input, 0.001, 600.0) {
                    net.sender_profile.ack_wait_seconds = val;
                }
            }
        }
        ui.checkbox(&mut net.view_received_acks, "View Received ACKs")
            .on_hover_text(
                "Record received ACK, NAK, and malformed MLLP responses in the ACK Log tab",
            );
        if net.view_received_acks {
            ui.checkbox(&mut net.modal_ack_popups, "Modal ACK Pop-ups")
                .on_hover_text(
                    "Display a modal alert dialog for each received ACK/NAK response (handy for inspecting single test sends)",
                );
        }
    });

    if net.sender_profile.wait_for_ack {
        ui.horizontal_wrapped(|ui| {
            ui.label("ACK Evaluation Mode:");
            let is_hl7 = matches!(net.sender_profile.ack_mode, AckMode::Hl7);
            let is_simple = matches!(net.sender_profile.ack_mode, AckMode::Simple { .. });
            let is_legacy = matches!(net.sender_profile.ack_mode, AckMode::LegacySubstring { .. });
            if ui
                .radio(is_hl7, "HL7 MSA")
                .on_hover_text(
                    "Parse the HL7 MSA segment: treat MSA-1 AA/CA as ACK and AE/AR/CE/CR as NAK, and warn on MSA-2/MSH-10 control-ID mismatch",
                )
                .clicked()
            {
                net.sender_profile.ack_mode = AckMode::Hl7;
            }
            if ui
                .radio(is_simple, "Simple Hex")
                .on_hover_text(
                    "Compare raw response bytes against an exact expected hex sequence (e.g. single-byte 0x06 ACK)",
                )
                .clicked()
            {
                net.sender_profile.ack_mode = AckMode::Simple {
                    expected_bytes: parse_hex_input(&net.simple_ack_hex_input),
                };
            }
            if ui
                .radio(is_legacy, "Legacy Substring")
                .on_hover_text(
                    "Treat a response as ACK if its payload text contains a required literal substring (e.g. MSA|AA)",
                )
                .clicked()
            {
                net.sender_profile.ack_mode = AckMode::LegacySubstring {
                    token: net.legacy_ack_token_input.clone(),
                };
            }
        });
        if matches!(net.sender_profile.ack_mode, AckMode::Simple { .. }) {
            ui.horizontal_wrapped(|ui| {
                ui.label("Expected Hex Bytes:");
                if ui
                    .add(styled_singleline_text_edit(
                        &mut net.simple_ack_hex_input,
                        200.0,
                        Some("0b 06 1c 0d"),
                    ))
                    .on_hover_text(
                        "Space-separated hex/dec/oct bytes that constitute a valid Simple Hex ACK response",
                    )
                    .changed()
                {
                    net.sender_profile.ack_mode = AckMode::Simple {
                        expected_bytes: parse_hex_input(&net.simple_ack_hex_input),
                    };
                }
            });
        } else if matches!(net.sender_profile.ack_mode, AckMode::LegacySubstring { .. }) {
            ui.horizontal_wrapped(|ui| {
                ui.label("Required Substring Token:");
                if ui
                    .add(styled_singleline_text_edit(
                        &mut net.legacy_ack_token_input,
                        200.0,
                        Some("MSA|AA"),
                    ))
                    .on_hover_text(
                        "Case-sensitive literal text that must appear in the response payload to classify it as an ACK",
                    )
                    .changed()
                {
                    net.sender_profile.ack_mode = AckMode::LegacySubstring {
                        token: net.legacy_ack_token_input.clone(),
                    };
                }
            });
        }

        ui.horizontal_wrapped(|ui| {
            ui.label("On NAK / Timeout:");
            egui::ComboBox::from_id_salt("nak_action_combo")
                .selected_text(nak_action_label(net.sender_profile.nak_action))
                .width(185.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut net.sender_profile.nak_action,
                        NakAction::Resend,
                        "Resend record",
                    )
                    .on_hover_text("Retry sending the same message over the existing connection");
                    ui.selectable_value(
                        &mut net.sender_profile.nak_action,
                        NakAction::ResetAndResend,
                        "Reset connection & resend",
                    )
                    .on_hover_text(
                        "Close and reconnect the TCP/TLS socket before retrying the message",
                    );
                    ui.selectable_value(
                        &mut net.sender_profile.nak_action,
                        NakAction::Skip,
                        "Skip record",
                    )
                    .on_hover_text("Skip this message and continue with the next message");
                })
                .response
                .on_hover_text(
                    "Action taken immediately when a message receives a NAK or times out waiting for an ACK",
                );

            ui.label("Max Tries:");
            if validated_text_edit(
                ui,
                &mut net.sender_max_tries_input,
                52.0,
                Some("3"),
                NumericFilter::DigitsOnly,
                validation.max_tries.as_deref(),
                "Maximum attempts per message (1–100) before triggering the 'When Exceeded' fallback action",
                &palette,
            )
            .changed()
            {
                if let Some(val) = parse_bounded_u32(&net.sender_max_tries_input, 1, 100) {
                    net.sender_profile.max_tries = val;
                }
            }

            ui.label("When Exceeded:");
            egui::ComboBox::from_id_salt("max_tries_action_combo")
                .selected_text(max_tries_action_label(net.sender_profile.max_tries_action))
                .width(195.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut net.sender_profile.max_tries_action,
                        MaxTriesAction::ResetAndResend,
                        "Reset connection & resend (restart counter)",
                    )
                    .on_hover_text(
                        "Reconnect the socket, reset the attempt counter, and continue retrying",
                    );
                    ui.selectable_value(
                        &mut net.sender_profile.max_tries_action,
                        MaxTriesAction::Skip,
                        "Skip record",
                    )
                    .on_hover_text("Skip the failed message and continue sending remaining messages");
                    ui.selectable_value(
                        &mut net.sender_profile.max_tries_action,
                        MaxTriesAction::CloseAndStop,
                        "Close & stop",
                    )
                    .on_hover_text("Close the TCP/TLS connection and halt the send batch");
                })
                .response
                .on_hover_text(
                    "Fallback action when a message exhausts Max Tries without receiving a valid ACK",
                );
        });
    }

    // Collapsible TLS 1.2/1.3 & mTLS section (NET-34, D-23)
    let tls_enabled = net.sender_profile.tls.enabled;
    let tls_header = egui::CollapsingHeader::new(if tls_enabled {
        "TLS 1.2 / 1.3 & Mutual TLS (mTLS) — Enabled"
    } else {
        "TLS 1.2 / 1.3 & Mutual TLS (mTLS)"
    })
    .id_salt("sender_tls_section")
    .default_open(tls_enabled)
    .show(ui, |ui| {
        show_tls_header_label(ui, net.sender_profile.tls.enabled, &palette);
        ui.checkbox(&mut net.sender_profile.tls.enabled, "Enable TLS 1.2/1.3")
            .on_hover_text(
                "Encrypt the outbound MLLP connection using TLS 1.2 or TLS 1.3 (rustls)",
            );
        if net.sender_profile.tls.enabled {
            ui.horizontal_wrapped(|ui| {
                let mut sni = net
                    .sender_profile
                    .tls
                    .sni_server_name
                    .clone()
                    .unwrap_or_default();
                ui.label("SNI Override (optional):");
                if ui
                    .add(styled_singleline_text_edit(
                        &mut sni,
                        220.0,
                        Some("e.g. mllp.hospital.local"),
                    ))
                    .on_hover_text(
                        "Override the TLS Server Name Indication (SNI) hostname sent during handshake (useful when connecting by IP address)",
                    )
                    .changed()
                {
                    net.sender_profile.tls.sni_server_name =
                        (!sni.trim().is_empty()).then(|| sni.trim().to_string());
                }
            });
            ui.checkbox(
                &mut net.sender_profile.tls.allow_invalid_certificates,
                "Allow invalid / self-signed certificates (local testing only)",
            )
            .on_hover_text(
                "Bypass server certificate chain and hostname verification. Use strictly for local or test MLLP listeners.",
            );
            show_optional_path_picker(
                ui,
                "Custom CA Bundle (PEM):",
                &mut net.sender_profile.tls.custom_ca_path,
                Some("Path to CA bundle (.pem)"),
                "PEM-encoded root/intermediate CA certificate bundle used to verify the remote MLLP server certificate",
            );
            show_optional_path_picker(
                ui,
                "mTLS Client Certificate (PEM):",
                &mut net.sender_profile.tls.client_cert_path,
                Some("Path to client cert (.pem)"),
                "PEM-encoded client certificate chain presented to the server for mutual TLS (mTLS) authentication",
            );
            show_optional_path_picker(
                ui,
                "mTLS Client Private Key (PEM):",
                &mut net.sender_profile.tls.client_key_path,
                Some("Path to client key (.pem)"),
                "PEM-encoded private key matching the mTLS client certificate",
            );
        }
    });
    tls_header.header_response.on_hover_text(
        "Expand to configure outbound TLS 1.2/1.3 encryption, custom CA verification, and client certificates (mTLS)",
    );

    ui.separator();

    // Controls & Progress Readout (NET-22)
    let is_idle = net.sender_state == SenderState::Idle;
    let is_paused = net.sender_state == SenderState::Paused;
    let sender_valid = net.validate_sender_inputs();
    let can_start_send = is_idle && sender_valid.is_valid();
    ui.horizontal_wrapped(|ui| {
        let mut send_btn = icon_labeled_button(
            ui,
            IconKind::Play,
            "Start Send",
            can_start_send,
            Some(palette.accent_green),
        )
        .on_hover_text("Connect and transmit the configured HL7 message scope over MLLP");
        if let Some(err) = sender_valid.first_error() {
            send_btn = send_btn.on_disabled_hover_text(err);
        } else if !is_idle {
            send_btn = send_btn.on_disabled_hover_text("A send batch is already in progress");
        }
        if send_btn.clicked() {
            start_clicked = true;
        }
        if is_paused {
            if icon_labeled_button(ui, IconKind::Play, "Resume", true, Some(palette.accent))
                .on_hover_text("Resume sending the paused HL7 batch")
                .clicked()
            {
                resume_clicked = true;
            }
        } else if icon_labeled_button(ui, IconKind::Pause, "Pause", !is_idle, Some(palette.accent))
            .on_hover_text("Pause the send batch after the current in-flight message completes")
            .on_disabled_hover_text("Start sending before pausing")
            .clicked()
        {
            pause_clicked = true;
        }
        if icon_labeled_button(
            ui,
            IconKind::Stop,
            "Stop",
            !is_idle,
            Some(palette.accent_red),
        )
        .on_hover_text("Stop transmission immediately and close the outbound MLLP connection")
        .on_disabled_hover_text("No active send batch to stop")
        .clicked()
        {
            stop_clicked = true;
        }
        ui.separator();
        ui.label(format!(
            "State: {:?} | Record {} of {} | Attempt: {} | Resets: {} | Last: {}",
            net.sender_state,
            net.progress_current,
            net.progress_total,
            net.progress_attempts,
            net.progress_resets,
            net.last_result
        ))
        .on_hover_text(
            "Live outbound sender state, batch progress, retry attempts, and connection resets",
        );
    });
    if let Some(err) = sender_valid.first_error() {
        ui.colored_label(palette.accent_red, format!("Validation: {err}"));
    }
    if let Some(ref msg) = net.sender_status_or_error {
        ui.colored_label(Color32::LIGHT_YELLOW, msg);
    }

    if start_clicked {
        if let Err(e) = net.start_sender(doc_records, selected_records, Some(ctx.clone())) {
            net.sender_status_or_error = Some(e);
        }
    }
    if pause_clicked {
        net.pause_sender();
    }
    if resume_clicked {
        net.resume_sender();
    }
    if stop_clicked {
        net.stop_sender();
    }
    if save_prof {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.toml", net.sender_profile.name))
            .add_filter("Sender Profile (*.toml)", &["toml"])
            .save_file()
        {
            if let Some(parent) = path.parent() {
                match save_sender_profile_in(&net.sender_profile, parent) {
                    Ok(p) => {
                        net.sender_status_or_error =
                            Some(format!("Saved sender profile to {}", p.display()));
                    }
                    Err(e) => net.sender_status_or_error = Some(e.to_string()),
                }
            }
        }
    }
    if load_prof {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Sender Profile (*.toml)", &["toml"])
            .pick_file()
        {
            match load_sender_profile_file(&path) {
                Ok(prof) => {
                    net.sender_profile = prof;
                    net.sync_sender_inputs_from_profile();
                }
                Err(e) => net.sender_status_or_error = Some(e.to_string()),
            }
        }
    }
}

fn show_listener_tab_ui(ui: &mut Ui, ctx: &Context, net: &mut NetworkPanelState) {
    let mut start_clicked = false;
    let mut stop_clicked = false;
    let mut save_prof = false;
    let mut load_prof = false;

    // Profile bar (NET-10)
    ui.horizontal_wrapped(|ui| {
        ui.label("Profile:");
        ui.add(styled_singleline_text_edit(
            &mut net.listener_profile.name,
            180.0,
            Some("Profile name"),
        ))
        .on_hover_text("Name used when saving or loading this Server Listener profile as TOML");
        if icon_labeled_button(ui, IconKind::Save, "Save Profile...", true, None)
            .on_hover_text("Save the current Server Listener configuration to a .toml profile file")
            .clicked()
        {
            save_prof = true;
        }
        if icon_labeled_button(ui, IconKind::Open, "Load Profile...", true, None)
            .on_hover_text("Load a saved Server Listener .toml profile file")
            .clicked()
        {
            load_prof = true;
        }
    });
    ui.separator();

    let palette = ThemePalette::for_visuals(ui.visuals());
    let validation = net.validate_listener_inputs();

    // Bind address & Port (NET-10, NET-11)
    ui.horizontal_wrapped(|ui| {
        ui.label("Bind Address:");
        if validated_text_edit(
            ui,
            &mut net.listener_profile.bind_address,
            155.0,
            Some("127.0.0.1"),
            NumericFilter::None,
            validation.bind_address.as_deref(),
            "Local IP interface to bind (defaults to 127.0.0.1 loopback; binding 0.0.0.0 or a LAN IP prompts for confirmation)",
            &palette,
        )
        .lost_focus()
        {
            let trimmed = net.listener_profile.bind_address.trim().to_string();
            net.listener_profile.bind_address = trimmed;
        }
        ui.label("Port:");
        if validated_text_edit(
            ui,
            &mut net.listener_port_input,
            68.0,
            Some("7772"),
            NumericFilter::DigitsOnly,
            validation.port.as_deref(),
            "Local TCP port to listen on for inbound MLLP connections (1–65535)",
            &palette,
        )
        .changed()
        {
            if let Some(p) = parse_valid_port(&net.listener_port_input) {
                net.listener_profile.port = p;
            }
        }
        ui.checkbox(
            &mut net.auto_scroll_received,
            "Auto-scroll record list on receive",
        )
        .on_hover_text(
            "Automatically scroll to and select newly received HL7 messages in the left message list",
        );
    });

    show_envelope_controls(ui, "listener_env", &mut net.listener_envelope_ui);

    show_optional_path_picker(
        ui,
        "Append Received Messages to Log File (optional):",
        &mut net.listener_profile.log_to_disk_path,
        Some("Optional output log file path"),
        "Append every inbound raw HL7 message to this file on disk as soon as it is received",
    );

    ui.separator();

    // ACK Settings (NET-15, NET-16)
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut net.listener_profile.send_ack, "Send ACK")
            .on_hover_text(
                "Automatically transmit an MLLP acknowledgement back to the sending peer for each received message",
            );
        if net.listener_profile.send_ack {
            let is_gen = matches!(
                net.listener_profile.ack_setting,
                ListenerAckSetting::GeneratedHl7 { .. }
            );
            let is_simple = matches!(
                net.listener_profile.ack_setting,
                ListenerAckSetting::SimpleHex(_)
            );
            let is_file = matches!(
                net.listener_profile.ack_setting,
                ListenerAckSetting::FromFile(_)
            );
            if ui
                .radio(is_gen, "Generated HL7 ACK")
                .on_hover_text(
                    "Build a valid HL7 ACK swapping MSH-3/4 with MSH-5/6 and echoing inbound MSH-10 into MSA-2",
                )
                .clicked()
            {
                net.listener_profile.ack_setting = ListenerAckSetting::GeneratedHl7 {
                    code: "AA".to_string(),
                };
            }
            if ui
                .radio(is_simple, "Simple Hex ACK")
                .on_hover_text("Reply to every received message with a fixed hex byte sequence")
                .clicked()
            {
                net.listener_profile.ack_setting =
                    ListenerAckSetting::SimpleHex("0b 06 1c 0d".to_string());
            }
            if ui
                .radio(is_file, "ACK from File")
                .on_hover_text(
                    "Reply to every received message with the static raw payload from a file on disk",
                )
                .clicked()
            {
                net.listener_profile.ack_setting =
                    ListenerAckSetting::FromFile(PathBuf::from("custom_ack.txt"));
            }
        }
    });

    if net.listener_profile.send_ack {
        ui.checkbox(
            &mut net.listener_profile.respect_ack_modes,
            "Respect MSH-15 / MSH-16 ACK rules",
        )
        .on_hover_text(
            "Honor HL7 Enhanced Mode rules: suppress Auto-ACK when inbound MSH-15/16 is NE",
        );
        match &mut net.listener_profile.ack_setting {
            ListenerAckSetting::GeneratedHl7 { code } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("MSA-1 ACK Code:");
                    ui.radio_value(code, "AA".to_string(), "AA (Accept)")
                        .on_hover_text("Original Mode: Application Accept (positive ACK)");
                    ui.radio_value(code, "AE".to_string(), "AE (Error)")
                        .on_hover_text("Original Mode: Application Error (negative NAK)");
                    ui.radio_value(code, "AR".to_string(), "AR (Reject)")
                        .on_hover_text("Original Mode: Application Reject (negative NAK)");
                    ui.separator();
                    ui.radio_value(code, "CA".to_string(), "CA (Commit Accept)")
                        .on_hover_text("Enhanced Mode: Commit Accept (positive ACK)");
                    ui.radio_value(code, "CE".to_string(), "CE (Commit Error)")
                        .on_hover_text("Enhanced Mode: Commit Error (negative NAK)");
                    ui.radio_value(code, "CR".to_string(), "CR (Commit Reject)")
                        .on_hover_text("Enhanced Mode: Commit Reject (negative NAK)");
                });
            }
            ListenerAckSetting::SimpleHex(hex_str) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Hex Bytes:");
                    ui.add(styled_singleline_text_edit(
                        hex_str,
                        200.0,
                        Some("0b 06 1c 0d"),
                    ))
                    .on_hover_text(
                        "Space-separated hex/dec/oct bytes sent back as the acknowledgement for each inbound message",
                    );
                });
            }
            ListenerAckSetting::FromFile(path) => {
                let mut opt = Some(path.clone());
                show_optional_path_picker(
                    ui,
                    "ACK File Path:",
                    &mut opt,
                    Some("Path to static ACK file"),
                    "File whose raw contents are sent back as the MLLP acknowledgement for each inbound message",
                );
                if let Some(p) = opt {
                    *path = p;
                }
            }
        }
    }

    // Collapsible TLS 1.2/1.3 & mTLS section (NET-17, D-23)
    let tls_enabled = net.listener_profile.tls.enabled;
    let tls_header = egui::CollapsingHeader::new(if tls_enabled {
        "TLS 1.2 / 1.3 & Mutual TLS (mTLS) — Enabled"
    } else {
        "TLS 1.2 / 1.3 & Mutual TLS (mTLS)"
    })
    .id_salt("listener_tls_section")
    .default_open(tls_enabled)
    .show(ui, |ui| {
        show_tls_header_label(ui, net.listener_profile.tls.enabled, &palette);
        ui.checkbox(&mut net.listener_profile.tls.enabled, "Enable TLS 1.2/1.3")
            .on_hover_text(
                "Require TLS 1.2 or TLS 1.3 encryption on all inbound MLLP connections",
            );
        if net.listener_profile.tls.enabled {
            show_optional_path_picker(
                ui,
                "Server Certificate Chain (PEM):",
                &mut net.listener_profile.tls.cert_chain_path,
                Some("Path to server cert (.pem)"),
                "PEM-encoded server certificate chain presented to connecting MLLP clients",
            );
            show_optional_path_picker(
                ui,
                "Server Private Key (PEM):",
                &mut net.listener_profile.tls.private_key_path,
                Some("Path to server key (.pem)"),
                "PEM-encoded private key matching the server certificate chain",
            );
            show_optional_path_picker(
                ui,
                "Client CA Bundle for mTLS (optional PEM):",
                &mut net.listener_profile.tls.client_ca_path,
                Some("Optional client CA bundle (.pem)"),
                "Optional PEM-encoded CA bundle; when set, connecting clients must authenticate with a valid client certificate (mTLS)",
            );
        }
    });
    tls_header.header_response.on_hover_text(
        "Expand to configure inbound TLS 1.2/1.3 server certificates and optional client CA verification (mTLS)",
    );

    ui.separator();

    // Start / Stop Controls (NET-11)
    let listener_valid = net.validate_listener_inputs();
    ui.horizontal_wrapped(|ui| {
        if !net.listener_running {
            let mut start_btn = icon_labeled_button(
                ui,
                IconKind::Listen,
                "Start Listening",
                listener_valid.is_valid(),
                Some(palette.accent_green),
            )
            .on_hover_text(
                "Bind the local TCP/TLS listener socket and begin accepting inbound MLLP connections",
            );
            if let Some(err) = listener_valid.first_error() {
                start_btn = start_btn.on_disabled_hover_text(err);
            }
            if start_btn.clicked() {
                start_clicked = true;
            }
        } else if icon_labeled_button(
            ui,
            IconKind::Stop,
            "Stop Listening",
            true,
            Some(palette.accent_red),
        )
        .on_hover_text("Stop accepting new connections and disconnect all active inbound clients")
        .clicked()
        {
            stop_clicked = true;
        }
        ui.label(format!(
            "Total Records Received: {}",
            net.total_received_count
        ))
        .on_hover_text("Cumulative HL7 messages received across all inbound listener connections");
    });
    if let Some(err) = listener_valid.first_error() {
        ui.colored_label(palette.accent_red, format!("Validation: {err}"));
    }
    if let Some(ref msg) = net.listener_status_or_error {
        ui.colored_label(Color32::LIGHT_YELLOW, msg);
    }

    if start_clicked {
        if let Err(e) = net.request_start_listener(false, Some(ctx.clone())) {
            net.listener_status_or_error = Some(e);
        }
    }
    if stop_clicked {
        net.stop_listener();
    }
    if save_prof {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.toml", net.listener_profile.name))
            .add_filter("Listener Profile (*.toml)", &["toml"])
            .save_file()
        {
            if let Some(parent) = path.parent() {
                match save_listener_profile_in(&net.listener_profile, parent) {
                    Ok(p) => {
                        net.listener_status_or_error =
                            Some(format!("Saved listener profile to {}", p.display()));
                    }
                    Err(e) => net.listener_status_or_error = Some(e.to_string()),
                }
            }
        }
    }
    if load_prof {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Listener Profile (*.toml)", &["toml"])
            .pick_file()
        {
            match load_listener_profile_file(&path) {
                Ok(prof) => {
                    net.listener_profile = prof;
                    net.sync_listener_inputs_from_profile();
                }
                Err(e) => net.listener_status_or_error = Some(e.to_string()),
            }
        }
    }
}

fn show_connections_tab_ui(ui: &mut Ui, net: &mut NetworkPanelState) {
    let palette = ThemePalette::for_visuals(ui.visuals());
    let active_count = net.connections.iter().filter(|c| c.active).count();
    let has_closed = net.connections.iter().any(|c| !c.active);

    ui.horizontal_wrapped(|ui| {
        ui.strong(format!(
            "Client Connections ({} total, {} active)",
            net.connections.len(),
            active_count
        ));
        ui.label(format!(
            "Total Messages Received: {}",
            net.total_received_count
        ))
        .on_hover_text("Total valid HL7 messages received across all inbound peer connections");
        ui.separator();
        if icon_labeled_button(
            ui,
            IconKind::Disconnect,
            "Disconnect All",
            net.listener_running,
            Some(palette.accent_red),
        )
        .on_hover_text("Stop the MLLP listener and close all active inbound client connections")
        .on_disabled_hover_text("Listener is not currently running")
        .clicked()
        {
            net.stop_listener();
        }
        if icon_labeled_button(ui, IconKind::Delete, "Clear Closed", has_closed, None)
            .on_hover_text("Remove closed historical connections from this table")
            .on_disabled_hover_text("No closed connections to clear")
            .clicked()
        {
            net.connections.retain(|c| c.active);
        }
    });
    ui.separator();

    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .column(Column::initial(60.0))
        .column(Column::initial(180.0))
        .column(Column::initial(90.0))
        .column(Column::initial(90.0))
        .column(Column::remainder())
        .header(CONTROL_HEIGHT, |mut h| {
            h.col(|ui| {
                ui.strong("ID")
                    .on_hover_text("Sequential connection identifier assigned by the listener");
            });
            h.col(|ui| {
                ui.strong("Remote Peer")
                    .on_hover_text("Remote client IP address and ephemeral TCP port");
            });
            h.col(|ui| {
                ui.strong("Status").on_hover_text(
                    "Whether the remote TCP/TLS socket is currently Active or Closed",
                );
            });
            h.col(|ui| {
                ui.strong("Messages").on_hover_text(
                    "Count of valid MLLP-framed HL7 messages received on this connection",
                );
            });
            h.col(|ui| {
                ui.strong("Bytes / Info")
                    .on_hover_text("Total payload bytes received and any framing or socket errors");
            });
        })
        .body(|body| {
            body.rows(22.0, net.connections.len(), |mut row| {
                let c = &net.connections[row.index()];
                row.col(|ui| {
                    ui.label(format!("#{}", c.conn_id));
                });
                row.col(|ui| {
                    ui.label(RichText::new(&c.peer_addr).monospace());
                });
                row.col(|ui| {
                    if c.active {
                        ui.colored_label(palette.accent_green, "Active");
                    } else {
                        ui.weak("Closed");
                    }
                });
                row.col(|ui| {
                    ui.label(format!("{}", c.messages_received));
                });
                row.col(|ui| {
                    let info = match &c.last_error {
                        Some(err) => format!("{} B ({err})", c.bytes_received),
                        None => format!("{} B", c.bytes_received),
                    };
                    ui.label(info);
                });
            });
        });
}

fn show_ack_log_tab_ui(ui: &mut Ui, net: &mut NetworkPanelState) {
    let mut export_history = false;
    let palette = ThemePalette::for_visuals(ui.visuals());
    let has_ack_entries = !net.ack_log.is_empty() || net.total_ack_responses > 0;
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("ACK / Traffic Log ({})", net.ack_log.len()));
        if net.ack_log_dropped_oldest > 0 {
            ui.weak(format!(
                "Showing latest {} of {} responses",
                net.ack_log.len(),
                net.total_ack_responses
            ));
        }
        if net.total_malformed > 0 {
            ui.colored_label(
                palette.accent_red,
                format!("Malformed: {}", net.total_malformed),
            )
            .on_hover_text(
                "Count of received packets with invalid MLLP framing or unexpected stray bytes",
            );
        }
        if icon_labeled_button(
            ui,
            IconKind::Delete,
            "Clear Log",
            has_ack_entries,
            Some(palette.accent_red),
        )
        .on_hover_text("Clear all logged ACK, NAK, and malformed entries and reset counters")
        .on_disabled_hover_text("ACK log is already empty")
        .clicked()
        {
            net.clear_ack_log();
        }
        ui.separator();
        ui.strong(format!("Send History ({})", net.send_history.len()));
        if icon_labeled_button(
            ui,
            IconKind::Save,
            "Export History CSV...",
            !net.send_history.is_empty(),
            None,
        )
        .on_hover_text(
            "Export outbound send history (record index, MSH-10 control ID, timestamp, outcome, RTT ms, peer) to CSV",
        )
        .on_disabled_hover_text("Send at least one message to export send history")
        .clicked()
        {
            export_history = true;
        }
    });
    ui.separator();

    let is_dark = palette.is_dark;
    for entry in net.ack_log.iter().rev() {
        let peer_suffix = entry
            .peer_addr
            .as_deref()
            .map_or_else(String::new, |p| format!(" [{p}]"));
        let ts_prefix = if entry.timestamp.is_empty() {
            String::new()
        } else {
            format!("[{}] ", entry.timestamp)
        };

        if entry.is_malformed {
            let pill_colors = ack_code_pill_colors("AR", is_dark);
            ui.horizontal_wrapped(|ui| {
                render_pill_badge(ui, "MALFORMED", pill_colors);
                ui.colored_label(
                    palette.accent_red,
                    RichText::new(format!(
                        "{ts_prefix}{}{peer_suffix} ({})",
                        entry.heading, entry.control_id
                    ))
                    .strong(),
                );
            });
            if let Some(ref reason) = entry.failure_reason {
                let reason_color = if is_dark {
                    Color32::from_rgb(0xFB, 0xBF, 0x24)
                } else {
                    Color32::from_rgb(0xB4, 0x53, 0x09)
                };
                ui.colored_label(reason_color, format!("Reason: {reason}"));
            }
            if !entry.payload_text.is_empty() {
                ui.label(
                    RichText::new(&entry.payload_text)
                        .monospace()
                        .color(palette.muted_text),
                );
            }
        } else {
            let code_badge = extract_msa1_code(&entry.payload_text).unwrap_or(if entry.is_ack {
                "ACK"
            } else {
                "NAK"
            });
            let pill_colors = ack_code_pill_colors(code_badge, is_dark);
            ui.horizontal_wrapped(|ui| {
                render_pill_badge(ui, code_badge, pill_colors);
                ui.strong(format!(
                    "{ts_prefix}{}{peer_suffix} (Message #{}, ID {}):",
                    entry.heading,
                    entry.record_index + 1,
                    entry.control_id
                ));
            });
            ui.label(RichText::new(&entry.payload_text).monospace());
        }
        ui.separator();
    }

    if export_history {
        let csv_body = net.format_history_csv();
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("send_history.csv")
            .add_filter("CSV (*.csv)", &["csv"])
            .save_file()
        {
            let _ = std::fs::write(path, csv_body);
        }
    }
}

fn show_non_loopback_confirm_window(ctx: &Context, net: &mut NetworkPanelState) {
    if !net.pending_non_loopback_confirm {
        return;
    }
    let mut confirm = false;
    let mut cancel = false;

    Window::new("Confirm Non-Loopback Listener Bind")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            apply_unified_control_metrics(ui);
            let palette = ThemePalette::for_visuals(ui.visuals());
            ui.label(strings::WARN_NON_LOOPBACK_BIND);
            ui.label(format!(
                "Target bind address: {}:{}",
                net.listener_profile.bind_address, net.listener_profile.port
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if icon_labeled_button(
                    ui,
                    IconKind::Listen,
                    "Bind Anyway",
                    true,
                    Some(palette.accent_red),
                )
                .on_hover_text(
                    "Proceed with binding the listener on a non-loopback network interface",
                )
                .clicked()
                {
                    confirm = true;
                }
                if icon_labeled_button(ui, IconKind::Close, "Cancel", true, None)
                    .on_hover_text("Cancel starting the listener")
                    .clicked()
                {
                    cancel = true;
                }
            });
        });

    if confirm {
        if let Err(e) = net.request_start_listener(true, Some(ctx.clone())) {
            net.listener_status_or_error = Some(e);
        }
    } else if cancel {
        net.pending_non_loopback_confirm = false;
    }
}

fn show_modal_ack_alert_window(ctx: &Context, net: &mut NetworkPanelState) {
    let Some(entry) = net.modal_ack_alert.clone() else {
        return;
    };
    let mut close = false;
    Window::new(&entry.heading)
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            apply_unified_control_metrics(ui);
            let palette = ThemePalette::for_visuals(ui.visuals());
            ui.label(format!(
                "Record #{} (Control ID: {})",
                entry.record_index + 1,
                entry.control_id
            ));
            ui.separator();
            ui.label(RichText::new(&entry.payload_text).monospace());
            ui.add_space(8.0);
            if icon_labeled_button(ui, IconKind::Check, "OK", true, Some(palette.accent))
                .on_hover_text("Dismiss this ACK/NAK alert")
                .clicked()
            {
                close = true;
            }
        });
    if close {
        net.modal_ack_alert = None;
    }
}

fn show_optional_path_picker(
    ui: &mut Ui,
    label: &str,
    path_opt: &mut Option<PathBuf>,
    hint: Option<&str>,
    tooltip: &str,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label).on_hover_text(tooltip);
        let mut text = path_opt
            .as_ref()
            .map_or_else(String::new, |p| p.display().to_string());
        if ui
            .add(styled_singleline_text_edit(&mut text, 220.0, hint))
            .on_hover_text(tooltip)
            .changed()
        {
            let trimmed = text.trim();
            *path_opt = (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
        }
        if icon_labeled_button(ui, IconKind::Open, "Browse...", true, None)
            .on_hover_text(format!("Choose a file for {}", label.trim_end_matches(':')))
            .clicked()
        {
            if let Some(p) = rfd::FileDialog::new().pick_file() {
                *path_opt = Some(p);
            }
        }
        if path_opt.is_some()
            && icon_labeled_button(ui, IconKind::Close, "Clear", true, None)
                .on_hover_text("Clear this file path")
                .clicked()
        {
            *path_opt = None;
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericFilter {
    None,
    DigitsOnly,
    UnsignedDecimal,
}

fn filter_numeric_buffer(buf: &mut String, mode: NumericFilter) {
    match mode {
        NumericFilter::None => {}
        NumericFilter::DigitsOnly => {
            buf.retain(|c| c.is_ascii_digit());
        }
        NumericFilter::UnsignedDecimal => {
            let mut saw_dot = false;
            buf.retain(|c| {
                if c.is_ascii_digit() {
                    true
                } else if c == '.' && !saw_dot {
                    saw_dot = true;
                    true
                } else {
                    false
                }
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validated_text_edit(
    ui: &mut Ui,
    buf: &mut String,
    width: f32,
    hint: Option<&str>,
    filter: NumericFilter,
    error: Option<&str>,
    tooltip: &str,
    palette: &ThemePalette,
) -> egui::Response {
    let mut resp = ui
        .scope(|ui| {
            if error.is_some() {
                let err_stroke = egui::Stroke::new(1.5_f32, palette.accent_red);
                ui.visuals_mut().widgets.inactive.bg_stroke = err_stroke;
                ui.visuals_mut().widgets.hovered.bg_stroke = err_stroke;
                ui.visuals_mut().widgets.active.bg_stroke = err_stroke;
                ui.visuals_mut().selection.stroke = err_stroke;
            }
            ui.add(styled_singleline_text_edit(buf, width, hint))
        })
        .inner;
    if resp.changed() {
        filter_numeric_buffer(buf, filter);
    }
    resp = match error {
        Some(err) => resp.on_hover_text(format!("{err}\n\n{tooltip}")),
        None => resp.on_hover_text(tooltip),
    };
    resp
}

fn parse_valid_port(s: &str) -> Option<u16> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = t.parse().ok()?;
    (port >= 1).then_some(port)
}

fn parse_bounded_f64(s: &str, min: f64, max: f64) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let val: f64 = t.parse().ok()?;
    (val.is_finite() && val >= min && val <= max).then_some(val)
}

fn parse_bounded_u32(s: &str, min: u32, max: u32) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let val: u32 = t.parse().ok()?;
    (val >= min && val <= max).then_some(val)
}

fn parse_bounded_usize(s: &str, min: usize, max: usize) -> Option<usize> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let val: usize = t.parse().ok()?;
    (val >= min && val <= max).then_some(val)
}

fn format_clean_f64(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn parse_hex_input(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .filter_map(MllpEnvelope::parse_byte)
        .collect()
}

fn extract_msa1_code(payload_text: &str) -> Option<&str> {
    let field_sep = payload_text
        .lines()
        .find_map(|line| {
            let mut chars = line.chars();
            if line.starts_with("MSH") {
                chars.nth(3)
            } else {
                None
            }
        })
        .unwrap_or('|');
    for line in payload_text.lines() {
        if let Some(rest) = line.strip_prefix("MSA") {
            if let Some(after_sep) = rest.strip_prefix(field_sep) {
                let code = after_sep.split(field_sep).next().unwrap_or("").trim();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
    }
    None
}
