//! Implementation of the `pipe_wrench_cli listen` subcommand (`NET-10..NET-17`, `D-19`, `D-23`).

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;
use directories::ProjectDirs;
use hl7_net::{
    run_listener_server, ListenerAckSetting, ListenerCommand, ListenerEvent, ListenerProfile,
};
use tokio::sync::mpsc;

/// Arguments for `pipe_wrench_cli listen` (`NET-10..NET-17`, `D-23`).
#[derive(Debug, Clone, Parser)]
pub struct ListenArgs {
    /// Load a saved `ListenerProfile` by name or path from the platform config directory,
    /// overridden by any explicit CLI flags (`NET-10`).
    #[arg(long)]
    pub profile: Option<String>,

    /// Local IP address or hostname to bind (default: `127.0.0.1`) (`NET-10`).
    #[arg(long)]
    pub bind: Option<String>,

    /// TCP port to listen on (default: `7772`) (`NET-10`).
    #[arg(long)]
    pub port: Option<u16>,

    /// ACK mode (`generated`, `simple`, `file:<path>`, or `off`) (`NET-15`).
    #[arg(long)]
    pub ack: Option<String>,

    /// Opt-in HL7 v2 Enhanced Mode: suppress Auto-ACK when inbound MSH-15/16=NE.
    #[arg(long, default_value_t = false)]
    pub respect_ack_modes: bool,

    /// Generated HL7 ACK `MSA-1` code (`AA`, `AE`, `AR`, `CA`, `CE`, or `CR`; default `AA`) (`NET-16`).
    #[arg(long)]
    pub ack_code: Option<String>,

    /// Space-separated hex bytes for `--ack simple` (default: `0b 06 1c 0d`) (`NET-15`).
    #[arg(long)]
    pub ack_hex: Option<String>,

    /// Append received HL7 records to a file on disk (`NET-10`, `NET-13`).
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Enable TLS 1.2/1.3 listener (`NET-17`, `D-23`).
    #[arg(long, default_value_t = false)]
    pub tls: bool,

    /// Server certificate chain PEM file (required when `--tls` is enabled) (`NET-17`).
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Server private key PEM file (required when `--tls` is enabled) (`NET-17`).
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// Optional client CA PEM bundle for mutual TLS (mTLS) client verification (`NET-17`).
    #[arg(long)]
    pub tls_client_ca: Option<PathBuf>,
}

/// Resolve a [`ListenerProfile`] from `--profile` (if provided) and apply CLI flag overrides.
pub fn build_listener_profile(args: &ListenArgs) -> Result<ListenerProfile, String> {
    let mut profile = match &args.profile {
        Some(name_or_path) => load_named_listener_profile(name_or_path)?,
        None => ListenerProfile::default(),
    };

    if let Some(ref bind) = args.bind {
        profile.bind_address = bind.clone();
    }
    if let Some(port) = args.port {
        profile.port = port;
    }
    if let Some(ref out) = args.out {
        profile.log_to_disk_path = Some(out.clone());
    }
    if args.respect_ack_modes {
        profile.respect_ack_modes = true;
    }

    let ack_code = args
        .ack_code
        .as_deref()
        .unwrap_or("AA")
        .trim()
        .to_ascii_uppercase();
    if !matches!(ack_code.as_str(), "AA" | "AE" | "AR" | "CA" | "CE" | "CR") {
        return Err(format!(
            "Invalid --ack-code '{ack_code}': expected AA, AE, AR, CA, CE, or CR."
        ));
    }

    if let Some(ref ack_spec) = args.ack {
        let trimmed = ack_spec.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower == "off" || lower == "none" {
            profile.send_ack = false;
        } else if lower == "generated" || lower == "hl7" {
            profile.send_ack = true;
            profile.ack_setting = ListenerAckSetting::GeneratedHl7 { code: ack_code };
        } else if lower == "simple" {
            profile.send_ack = true;
            let hex = args
                .ack_hex
                .clone()
                .unwrap_or_else(|| "0b 06 1c 0d".to_string());
            profile.ack_setting = ListenerAckSetting::SimpleHex(hex);
        } else if let Some(path_str) = trimmed
            .strip_prefix("file:")
            .or_else(|| trimmed.strip_prefix("FILE:"))
        {
            profile.send_ack = true;
            profile.ack_setting = ListenerAckSetting::FromFile(PathBuf::from(path_str.trim()));
        } else {
            return Err(format!(
                "Invalid --ack '{trimmed}': expected generated, simple, file:<path>, or off."
            ));
        }
    } else if args.ack_code.is_some() {
        profile.send_ack = true;
        profile.ack_setting = ListenerAckSetting::GeneratedHl7 { code: ack_code };
    } else if let Some(ref hex) = args.ack_hex {
        profile.send_ack = true;
        profile.ack_setting = ListenerAckSetting::SimpleHex(hex.clone());
    }

    let any_tls_flag = args.tls
        || args.tls_cert.is_some()
        || args.tls_key.is_some()
        || args.tls_client_ca.is_some();

    if any_tls_flag {
        profile.tls.enabled = true;
    }
    if let Some(ref cert) = args.tls_cert {
        profile.tls.cert_chain_path = Some(cert.clone());
    }
    if let Some(ref key) = args.tls_key {
        profile.tls.private_key_path = Some(key.clone());
    }
    if let Some(ref ca) = args.tls_client_ca {
        profile.tls.client_ca_path = Some(ca.clone());
    }

    if profile.tls.enabled
        && (profile.tls.cert_chain_path.is_none() || profile.tls.private_key_path.is_none())
    {
        return Err(
            "Both --tls-cert and --tls-key are required when listener TLS is enabled.".to_string(),
        );
    }

    Ok(profile)
}

/// Run the headless MLLP listener until `Ctrl+C` is received.
pub async fn execute_listen_command(args: &ListenArgs) -> Result<(), String> {
    let profile = build_listener_profile(args)?;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let server_handle = tokio::spawn(run_listener_server(profile, ev_tx, cmd_rx));

    let log_task = tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            match ev {
                ListenerEvent::Started { port } => {
                    let _ = writeln!(std::io::stderr(), "Listening on port {port}...");
                }
                ListenerEvent::ClientConnected { addr, conn_id } => {
                    let _ = writeln!(std::io::stderr(), "Client #{conn_id} connected from {addr}");
                }
                ListenerEvent::ClientDisconnected {
                    addr,
                    conn_id,
                    messages,
                } => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Client #{conn_id} ({addr}) disconnected ({messages} message(s))"
                    );
                }
                ListenerEvent::RecordReceived { record, peer, .. } => {
                    let summary = record.summary();
                    let _ = writeln!(
                        std::io::stderr(),
                        "Received {} (MSH-10={}) from {peer}",
                        summary.message_type,
                        summary.control_id
                    );
                }
                ListenerEvent::MalformedPacket { conn_id, info } => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Malformed packet on conn #{conn_id} from {}: {} | preview: {}",
                        info.peer_addr,
                        info.reason,
                        info.raw_preview
                    );
                }
                ListenerEvent::ClientError { conn_id, error } => {
                    let _ = writeln!(std::io::stderr(), "Connection #{conn_id} error: {error}");
                }
                ListenerEvent::WarningMessage(msg) => {
                    let _ = writeln!(std::io::stderr(), "Warning: {msg}");
                }
                ListenerEvent::FatalError(err) => {
                    let _ = writeln!(std::io::stderr(), "Fatal error: {err}");
                }
                ListenerEvent::Stopped => {
                    let _ = writeln!(std::io::stderr(), "Listener stopped.");
                    break;
                }
            }
        }
    });

    tokio::select! {
        res = server_handle => {
            let _ = log_task.await;
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("Listener task failed: {e}")),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            let _ = cmd_tx.send(ListenerCommand::Stop);
            let _ = log_task.await;
            Ok(())
        }
    }
}

fn load_named_listener_profile(name_or_path: &str) -> Result<ListenerProfile, String> {
    let direct = Path::new(name_or_path);
    if direct.is_file() {
        let body = std::fs::read_to_string(direct)
            .map_err(|e| format!("Failed reading profile '{}': {e}", direct.display()))?;
        return toml::from_str(&body)
            .map_err(|e| format!("Invalid listener profile TOML '{}': {e}", direct.display()));
    }
    if name_or_path.eq_ignore_ascii_case("default") {
        return Ok(ListenerProfile::default());
    }
    if let Some(dirs) = ProjectDirs::from("io", "pipe_wrench", "pipe_wrench") {
        let candidate = dirs.config_dir().join(format!("{name_or_path}.toml"));
        if candidate.is_file() {
            let body = std::fs::read_to_string(&candidate)
                .map_err(|e| format!("Failed reading profile '{}': {e}", candidate.display()))?;
            return toml::from_str(&body).map_err(|e| {
                format!(
                    "Invalid listener profile TOML '{}': {e}",
                    candidate.display()
                )
            });
        }
    }
    Err(format!("Listener profile '{name_or_path}' not found."))
}
