//! Implementation of the `pipe_wrench_cli send` subcommand (`NET-20..NET-34`, `D-18`, `D-23`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use directories::ProjectDirs;
use hl7_core::ack::AckMode;
use hl7_core::{load_files, FilterMatchOptions, RecordFilter, RecoveryMode, ScannerConfig};
use hl7_net::{
    resolve_send_scope, run_sender, MaxTriesAction, NakAction, SendHistoryEntry, SendScope,
    SenderEvent, SenderProfile,
};
use tokio::sync::mpsc;

/// CLI ACK classification mode (`NET-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliAckMode {
    /// Standard HL7 MSA-1 (`AA`/`CA`) and MSA-2 == MSH-10 check.
    Hl7,
    /// Simple raw frame or envelope-stripped byte match (default `0b 06 1c 0d`).
    Simple,
    /// Legacy substring match (default token `MSA|AA`).
    Legacy,
}

/// CLI action on NAK or timeout (`NET-28`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliNakAction {
    /// Resend the current record over the existing connection.
    Resend,
    /// Close socket, reconnect, and resend record.
    Reset,
    /// Skip the current record and proceed to the next.
    Skip,
}

/// CLI action when max tries is exceeded (`NET-29`, `D-18`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliMaxTriesAction {
    /// Close socket, reconnect, reset attempt counter, and retry.
    Reset,
    /// Skip the current record and proceed to the next.
    Skip,
    /// Close socket, report `"Remote host is not acknowledging records."`, and stop.
    Stop,
}

/// Arguments for `pipe_wrench_cli send` (`NET-20..NET-34`, `D-23`).
#[derive(Debug, Clone, Parser)]
pub struct SendArgs {
    /// One or more input files (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`) to load and send.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Load a saved `SenderProfile` by name or path from the platform config directory,
    /// overridden by any explicit CLI flags (`NET-20`).
    #[arg(long)]
    pub profile: Option<String>,

    /// Destination hostname or IP address (default: `127.0.0.1`).
    #[arg(long)]
    pub host: Option<String>,

    /// Destination TCP port (default: `7772`).
    #[arg(long)]
    pub port: Option<u16>,

    /// 1-based inclusive record range to send (`start..end` or `start..=end`) (`NET-21`).
    #[arg(long)]
    pub scope: Option<String>,

    /// Optional plain-text or field-path filter expression (`D-26`).
    #[arg(long)]
    pub filter: Option<String>,

    /// Optional field-path filter expression (`<PATH>=<VALUE>`, `<PATH>!=<VALUE>`, `<PATH>><VALUE>`, or `'<PATH> contains <VALUE>'`) (`D-26`).
    #[arg(long)]
    pub filter_field: Vec<String>,

    /// Case-insensitive filter matching (default: true; pass `--ignore-case=false` for case-sensitive) (`D-26`).
    #[arg(long, short = 'i', default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub ignore_case: bool,

    /// Interpret `--filter` and `--filter-field` values as regular expressions (`D-26`).
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Invert filter match (`-v` / `--invert-match`) (`D-26`).
    #[arg(long, short = 'v', default_value_t = false)]
    pub invert_match: bool,

    /// Connection timeout in seconds (default: `10.0`) (`NET-23`).
    #[arg(long)]
    pub connect_timeout: Option<f64>,

    /// Delay in seconds after connecting before sending the first record (`NET-24`).
    #[arg(long)]
    pub delay_connect: Option<f64>,

    /// Delay in seconds between records (`NET-24`).
    #[arg(long)]
    pub delay_between: Option<f64>,

    /// Do not wait for an ACK after sending each record (`NET-25`).
    #[arg(long, default_value_t = false)]
    pub no_wait_ack: bool,

    /// Opt-in HL7 v2 Enhanced Mode: skip waiting for an ACK when MSH-15/16=NE.
    #[arg(long, default_value_t = false)]
    pub respect_ack_modes: bool,

    /// Maximum seconds to wait for an ACK before timing out (default: `10.0`) (`NET-25`).
    #[arg(long)]
    pub ack_timeout: Option<f64>,

    /// ACK classification mode (`hl7`, `simple`, or `legacy`) (`NET-26`).
    #[arg(long, value_enum)]
    pub ack_mode: Option<CliAckMode>,

    /// Action on NAK or timeout (`resend`, `reset`, or `skip`) (`NET-28`).
    #[arg(long, value_enum)]
    pub nak_action: Option<CliNakAction>,

    /// Maximum send attempts per record (`NET-29`).
    #[arg(long)]
    pub max_tries: Option<u32>,

    /// Action when max tries is exceeded (`reset`, `skip`, or `stop`) (`NET-29`).
    #[arg(long, value_enum)]
    pub max_tries_action: Option<CliMaxTriesAction>,

    /// Rewrite outgoing `MSH-10` control ID with a fresh unique value (`NET-31`).
    #[arg(long, default_value_t = false)]
    pub rewrite_control_id: bool,

    /// Refresh outgoing `MSH-7` timestamp to current UTC time (`NET-32`).
    #[arg(long, default_value_t = false)]
    pub refresh_timestamp: bool,

    /// Export per-record send history log to a CSV file (`NET-33`).
    #[arg(long)]
    pub history_out: Option<PathBuf>,

    /// Enable TLS 1.2/1.3 transport (automatically enabled if any TLS cert/CA flag is passed) (`NET-34`, `D-23`).
    #[arg(long, default_value_t = false)]
    pub tls: bool,

    /// Override TLS SNI server name (defaults to `--host`) (`NET-34`).
    #[arg(long)]
    pub tls_sni: Option<String>,

    /// Custom CA certificate PEM bundle for server verification (`NET-34`).
    #[arg(long)]
    pub tls_ca: Option<PathBuf>,

    /// Client certificate chain PEM file for mutual TLS (mTLS) (`NET-34`).
    #[arg(long)]
    pub tls_client_cert: Option<PathBuf>,

    /// Client private key PEM file for mutual TLS (mTLS) (`NET-34`).
    #[arg(long)]
    pub tls_client_key: Option<PathBuf>,

    /// Explicit opt-in flag to accept self-signed or invalid server certificates for local testing (`NET-34`).
    #[arg(long, default_value_t = false)]
    pub tls_allow_invalid_certs: bool,
}

/// Resolve a [`SenderProfile`] from `--profile` (if provided) and apply CLI flag overrides.
pub fn build_sender_profile(args: &SendArgs) -> Result<SenderProfile, String> {
    let mut profile = match &args.profile {
        Some(name_or_path) => load_named_sender_profile(name_or_path)?,
        None => SenderProfile::default(),
    };

    if let Some(ref host) = args.host {
        profile.host = host.clone();
    }
    if let Some(port) = args.port {
        profile.port = port;
    }
    if let Some(ct) = args.connect_timeout {
        profile.connect_timeout_seconds = ct;
    }
    if let Some(dc) = args.delay_connect {
        profile.delay_after_connect_seconds = dc;
    }
    if let Some(db) = args.delay_between {
        profile.delay_between_records_seconds = db;
    }
    if args.no_wait_ack {
        profile.wait_for_ack = false;
    }
    if args.respect_ack_modes {
        profile.respect_ack_modes = true;
    }
    if let Some(at) = args.ack_timeout {
        profile.ack_wait_seconds = at;
    }
    if let Some(mode) = args.ack_mode {
        profile.ack_mode = match mode {
            CliAckMode::Hl7 => AckMode::Hl7,
            CliAckMode::Simple => AckMode::Simple {
                expected_bytes: vec![0x0B, 0x06, 0x1C, 0x0D],
            },
            CliAckMode::Legacy => AckMode::LegacySubstring {
                token: "MSA|AA".to_string(),
            },
        };
    }
    if let Some(nak) = args.nak_action {
        profile.nak_action = match nak {
            CliNakAction::Resend => NakAction::Resend,
            CliNakAction::Reset => NakAction::ResetAndResend,
            CliNakAction::Skip => NakAction::Skip,
        };
    }
    if let Some(mt) = args.max_tries {
        profile.max_tries = mt.max(1);
    }
    if let Some(mta) = args.max_tries_action {
        profile.max_tries_action = match mta {
            CliMaxTriesAction::Reset => MaxTriesAction::ResetAndResend,
            CliMaxTriesAction::Skip => MaxTriesAction::Skip,
            CliMaxTriesAction::Stop => MaxTriesAction::CloseAndStop,
        };
    }
    if args.rewrite_control_id {
        profile.rewrite_control_id = true;
    }
    if args.refresh_timestamp {
        profile.refresh_timestamp = true;
    }

    let any_tls_flag = args.tls
        || args.tls_sni.is_some()
        || args.tls_ca.is_some()
        || args.tls_client_cert.is_some()
        || args.tls_client_key.is_some()
        || args.tls_allow_invalid_certs;

    if any_tls_flag {
        profile.tls.enabled = true;
    }
    if let Some(ref sni) = args.tls_sni {
        profile.tls.sni_server_name = Some(sni.clone());
    }
    if let Some(ref ca) = args.tls_ca {
        profile.tls.custom_ca_path = Some(ca.clone());
    }
    if let Some(ref cert) = args.tls_client_cert {
        profile.tls.client_cert_path = Some(cert.clone());
    }
    if let Some(ref key) = args.tls_client_key {
        profile.tls.client_key_path = Some(key.clone());
    }
    if args.tls_allow_invalid_certs {
        profile.tls.allow_invalid_certificates = true;
    }

    if profile.tls.client_cert_path.is_some() ^ profile.tls.client_key_path.is_some() {
        return Err(
            "Both --tls-client-cert and --tls-client-key must be provided for mutual TLS."
                .to_string(),
        );
    }

    Ok(profile)
}

/// Parse `--scope <start..end>` into a [`SendScope`] (`NET-21`).
pub fn parse_send_scope(spec: Option<&str>) -> Result<SendScope, String> {
    let Some(raw) = spec else {
        return Ok(SendScope::All);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok(SendScope::All);
    }
    let (start_str, end_str) = if let Some(pair) = trimmed.split_once("..=") {
        pair
    } else if let Some(pair) = trimmed.split_once("..") {
        pair
    } else if let Some(pair) = trimmed.split_once('-') {
        pair
    } else {
        let single: usize = trimmed
            .parse()
            .map_err(|_| format!("Invalid --scope '{trimmed}': expected start..end"))?;
        return Ok(SendScope::Range {
            start: single,
            end: single,
        });
    };

    let start: usize = start_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid --scope start '{start_str}'"))?;
    let end: usize = end_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid --scope end '{end_str}'"))?;
    Ok(SendScope::Range { start, end })
}

/// Execute `pipe_wrench_cli send`, returning `Ok(0)` when all selected records are sent and
/// acknowledged, or `Err` / `Ok(1)` on connection/NAK failure.
pub async fn execute_send_command(args: &SendArgs) -> Result<i32, String> {
    let profile = build_sender_profile(args)?;
    let scope = parse_send_scope(args.scope.as_deref())?;

    let scanner_cfg = ScannerConfig {
        envelope: hl7_core::MllpEnvelope {
            start_block: profile.envelope.start_block,
            end_data: profile.envelope.end_data,
            end_block: profile.envelope.end_block,
        },
        recovery_mode: RecoveryMode::RecoverNonHl7,
        max_line_bytes: 64 * 1024 * 1024,
    };
    let cancel = AtomicBool::new(false);
    let mut load_out =
        load_files(&args.files, scanner_cfg, &cancel, |_| {}).map_err(|e| e.to_string())?;

    let filter_opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: args.invert_match,
    };
    if let Some(filter) = RecordFilter::from_cli_specs_with_options(
        args.filter.as_deref(),
        &args.filter_field,
        filter_opts,
    )? {
        load_out.records.retain(|r| filter.matches_record(r));
    }

    if load_out.records.is_empty() {
        return Err("No HL7 records found in input file(s).".to_string());
    }

    let indices = resolve_send_scope(&scope, load_out.records.len()).map_err(|e| e.to_string())?;
    let payloads: Vec<Vec<u8>> = indices
        .into_iter()
        .filter_map(|i| load_out.records.get(i).map(|r| r.bytes().to_vec()))
        .collect();

    let total_to_send = payloads.len();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let send_res = run_sender(profile.clone(), payloads, ev_tx, cmd_rx).await;

    let mut history: Vec<SendHistoryEntry> = Vec::new();
    let mut sent_count = 0usize;
    let mut skipped_count = 0usize;
    let mut nak_count = 0usize;

    while let Ok(ev) = ev_rx.try_recv() {
        match ev {
            SenderEvent::Progress {
                current,
                total,
                attempts,
                ..
            } => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Sending record {current}/{total} (attempt {attempts})..."
                );
            }
            SenderEvent::AckReceived {
                record_index,
                control_id,
                is_ack,
                peer_addr,
                ..
            } => {
                if !is_ack {
                    nak_count += 1;
                }
                let label = if is_ack { "ACK" } else { "NAK" };
                let _ = writeln!(
                    std::io::stderr(),
                    "Record #{} (MSH-10={control_id}) [{peer_addr}]: {label}",
                    record_index + 1
                );
            }
            SenderEvent::MalformedPacket {
                record_index,
                control_id,
                info,
            } => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Record #{} (MSH-10={control_id}) [{}]: MALFORMED — {} | preview: {}",
                    record_index + 1,
                    info.peer_addr,
                    info.reason,
                    info.raw_preview
                );
            }
            SenderEvent::History(entry) => {
                history.push(entry);
            }
            SenderEvent::StatusMessage(msg) => {
                let _ = writeln!(std::io::stderr(), "{msg}");
            }
            SenderEvent::Finished { sent, skipped } => {
                sent_count = sent;
                skipped_count = skipped;
            }
            _ => {}
        }
    }

    if let Some(ref csv_path) = args.history_out {
        write_history_csv(csv_path, &history)?;
    }

    let _ = writeln!(
        std::io::stderr(),
        "Send summary: {sent_count}/{total_to_send} sent, {skipped_count} skipped, {nak_count} NAKs"
    );

    if let Err(err) = send_res {
        return Err(err.to_string());
    }

    if sent_count == total_to_send && skipped_count == 0 {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn write_history_csv(path: &Path, history: &[SendHistoryEntry]) -> Result<(), String> {
    let mut out = String::from("record_index,control_id,timestamp,outcome,rtt_ms,attempt\r\n");
    for entry in history {
        out.push_str(&format!(
            "{},{},{},{},{},{}\r\n",
            entry.record_index + 1,
            entry.control_id,
            entry.timestamp,
            entry.outcome,
            entry.rtt_ms,
            entry.attempt
        ));
    }
    std::fs::write(path, out).map_err(|e| format!("Failed writing '{}': {e}", path.display()))
}

fn load_named_sender_profile(name_or_path: &str) -> Result<SenderProfile, String> {
    let direct = Path::new(name_or_path);
    if direct.is_file() {
        let body = std::fs::read_to_string(direct)
            .map_err(|e| format!("Failed reading profile '{}': {e}", direct.display()))?;
        return toml::from_str(&body)
            .map_err(|e| format!("Invalid sender profile TOML '{}': {e}", direct.display()));
    }
    if name_or_path.eq_ignore_ascii_case("default") {
        return Ok(SenderProfile::default());
    }
    if let Some(dirs) = ProjectDirs::from("io", "pipe_wrench", "pipe_wrench") {
        let candidate = dirs.config_dir().join(format!("{name_or_path}.toml"));
        if candidate.is_file() {
            let body = std::fs::read_to_string(&candidate)
                .map_err(|e| format!("Failed reading profile '{}': {e}", candidate.display()))?;
            return toml::from_str(&body).map_err(|e| {
                format!("Invalid sender profile TOML '{}': {e}", candidate.display())
            });
        }
    }
    Err(format!("Sender profile '{name_or_path}' not found."))
}
