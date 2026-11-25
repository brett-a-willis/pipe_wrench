//! Listener and Sender network profile configurations and TLS options (NET-10, NET-20, NET-34, D-23).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::envelope::MllpEnvelope;
use hl7_core::ack::AckMode;

/// TLS configuration for MLLP listener (NET-17, D-23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListenerTlsConfig {
    /// Enable TLS encryption (default `false`).
    pub enabled: bool,
    /// Path to server certificate chain in PEM format.
    pub cert_chain_path: Option<PathBuf>,
    /// Path to private key in PEM format.
    pub private_key_path: Option<PathBuf>,
    /// Path to client CA bundle in PEM format for mutual TLS (mTLS).
    pub client_ca_path: Option<PathBuf>,
}

/// Profile for inbound MLLP server listener (NET-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerProfile {
    /// Profile identifier name.
    pub name: String,
    /// Local address or interface to bind (defaults to loopback `127.0.0.1` per AGENTS §9).
    pub bind_address: String,
    /// Port to listen on.
    pub port: u16,
    /// MLLP envelope markers.
    pub envelope: MllpEnvelope,
    /// Optional path to append received records to a log file on disk.
    pub log_to_disk_path: Option<PathBuf>,
    /// Whether to automatically send ACKs.
    pub send_ack: bool,
    /// Opt-in HL7 v2 Enhanced Mode ACK handling (`MSH-15` / `MSH-16`).
    /// Defaults to `false` so Auto-ACK is always sent unless explicitly enabled.
    #[serde(default)]
    pub respect_ack_modes: bool,
    /// Form of ACK to send.
    pub ack_setting: ListenerAckSetting,
    /// Optional TLS configuration (NET-17, D-23).
    pub tls: ListenerTlsConfig,
}

/// ACK generation setting for the listener (NET-15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListenerAckSetting {
    /// Space-separated hex bytes sent verbatim, default `0b 06 1c 0d`.
    SimpleHex(String),
    /// Generated HL7 ACK record (NET-16).
    GeneratedHl7 {
        /// Code for MSA-1 (AA/AE/AR).
        code: String,
    },
    /// ACK read from a specified file (default `custom_ack.txt`).
    FromFile(PathBuf),
}

impl Default for ListenerAckSetting {
    fn default() -> Self {
        Self::GeneratedHl7 {
            code: "AA".to_string(),
        }
    }
}

impl Default for ListenerProfile {
    fn default() -> Self {
        Self {
            name: "Default Listener".to_string(),
            bind_address: "127.0.0.1".to_string(),
            port: 7772,
            envelope: MllpEnvelope::DEFAULT,
            log_to_disk_path: None,
            send_ack: true,
            respect_ack_modes: false,
            ack_setting: ListenerAckSetting::default(),
            tls: ListenerTlsConfig::default(),
        }
    }
}

/// TLS configuration for MLLP client sender (NET-34, D-23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SenderTlsConfig {
    /// Enable TLS encryption (default `false`).
    pub enabled: bool,
    /// SNI server name override (defaults to configured host if None).
    pub sni_server_name: Option<String>,
    /// Optional custom CA root certificate PEM bundle.
    pub custom_ca_path: Option<PathBuf>,
    /// Explicit opt-in toggle to allow self-signed / invalid certificates for testing.
    pub allow_invalid_certificates: bool,
    /// Optional client certificate PEM path for mutual TLS (mTLS).
    pub client_cert_path: Option<PathBuf>,
    /// Optional client private key PEM path for mutual TLS (mTLS).
    pub client_key_path: Option<PathBuf>,
}

/// Action to perform when a NAK or timeout occurs before max tries is exceeded (NET-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NakAction {
    /// Resend the current record over the existing connection.
    #[default]
    Resend,
    /// Close socket, reconnect to server, and resend record.
    ResetAndResend,
    /// Skip the current record and move to the next.
    Skip,
}

/// Action to perform when max tries is exceeded for a record (NET-29, D-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MaxTriesAction {
    /// Close socket, reconnect to server, reset attempt counter to 0, and retry.
    ResetAndResend,
    /// Skip the current record and proceed to the next record.
    Skip,
    /// Close socket, emit `"Remote host is not acknowledging records."`, and stop sender.
    #[default]
    CloseAndStop,
}

/// Scope of records to send (NET-21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SendScope {
    /// Send all loaded records.
    #[default]
    All,
    /// Send only currently selected records.
    Selected(Vec<usize>),
    /// 1-based inclusive range [start, end].
    Range {
        /// 1-based start record index.
        start: usize,
        /// 1-based end record index.
        end: usize,
    },
}

/// Profile for outbound MLLP client sender (NET-20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SenderProfile {
    /// Profile identifier name.
    pub name: String,
    /// Destination hostname or IP address (default `127.0.0.1` per AGENTS §9).
    pub host: String,
    /// Destination port.
    pub port: u16,
    /// MLLP envelope markers.
    pub envelope: MllpEnvelope,
    /// Timeout in seconds to establish TCP connection (default 10.0 s).
    pub connect_timeout_seconds: f64,
    /// Delay in seconds before sending the first record after connecting.
    pub delay_after_connect_seconds: f64,
    /// Delay in seconds between subsequent records.
    pub delay_between_records_seconds: f64,
    /// Whether to wait for an ACK response after sending each record.
    pub wait_for_ack: bool,
    /// Opt-in HL7 v2 Enhanced Mode ACK handling (`MSH-15` / `MSH-16`).
    /// Defaults to `false` so the sender always waits for an ACK unless explicitly enabled.
    #[serde(default)]
    pub respect_ack_modes: bool,
    /// Maximum seconds to wait for ACK before timing out (default 10.0 s).
    pub ack_wait_seconds: f64,
    /// How received responses are classified as ACK vs NAK (NET-26).
    #[serde(default)]
    pub ack_mode: AckMode,
    /// Maximum send attempts per record (minimum 1, default 3).
    pub max_tries: u32,
    /// Action on NAK or timeout before max_tries (NET-28).
    pub nak_action: NakAction,
    /// Action when max_tries exceeded (NET-29).
    pub max_tries_action: MaxTriesAction,
    /// Rewrite MSH-10 with a fresh unique ID on outgoing copy only (NET-31).
    pub rewrite_control_id: bool,
    /// Refresh MSH-7 timestamp to current time on outgoing copy only (NET-32).
    pub refresh_timestamp: bool,
    /// Optional TLS configuration (NET-34, D-23).
    pub tls: SenderTlsConfig,
}

impl Default for SenderProfile {
    fn default() -> Self {
        Self {
            name: "Default Sender".to_string(),
            host: "127.0.0.1".to_string(),
            port: 7772,
            envelope: MllpEnvelope::DEFAULT,
            connect_timeout_seconds: 10.0,
            delay_after_connect_seconds: 0.0,
            delay_between_records_seconds: 0.0,
            wait_for_ack: true,
            respect_ack_modes: false,
            ack_wait_seconds: 10.0,
            ack_mode: AckMode::Hl7,
            max_tries: 3,
            nak_action: NakAction::Resend,
            max_tries_action: MaxTriesAction::CloseAndStop,
            rewrite_control_id: false,
            refresh_timestamp: false,
            tls: SenderTlsConfig::default(),
        }
    }
}
