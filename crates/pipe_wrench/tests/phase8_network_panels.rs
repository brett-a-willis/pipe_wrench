//! Phase 8 tests covering MLLP Sender and Listener UI panels, profile TOML persistence,
//! envelope byte input parsing (Hex/Dec/Oct + Defaults), non-loopback bind confirmation,
//! live TCP/IP record receive into `SessionState` (`"TCP/IP Listener"`, `FS-2`),
//! and Received ACK/NAK log + send history CSV export (`NET-1`, `NET-10..NET-34`).

use std::time::{Duration, Instant};

use hl7_core::AckMode;
use hl7_net::{ListenerAckSetting, ListenerProfile, MaxTriesAction, NakAction, SenderProfile};
use pipe_wrench::config::{
    load_listener_profile_file, load_sender_profile_file, save_listener_profile_in,
    save_sender_profile_in,
};
use pipe_wrench::ui::net_panels::{EnvelopeInputState, NetworkPanelState, SendScopeUiMode};
use pipe_wrench::{AppSettings, DataSource, SessionState};
use tempfile::TempDir;

// NET-1, NET-10, NET-20, G-6: Envelope input formats (Hex/Dec/Oct + Defaults) and profile TOML persistence
#[test]
fn envelope_input_parsing_and_profile_toml_persistence() {
    let mut env_ui = EnvelopeInputState {
        sb_input: "11".to_string(),   // decimal 11 = 0x0B
        ed_input: "0o34".to_string(), // octal 34 = 28 = 0x1C
        eb_input: "0x0D".to_string(), // hex 0x0D
    };
    let parsed = env_ui.parse_envelope().unwrap();
    assert_eq!(parsed.start_block, 0x0B);
    assert_eq!(parsed.end_data, 0x1C);
    assert_eq!(parsed.end_block, 0x0D);

    env_ui.sb_input = "0x01".to_string();
    env_ui.reset_defaults();
    assert_eq!(env_ui.parse_envelope().unwrap().start_block, 0x0B);

    let dir = TempDir::new().unwrap();

    // Sender profile round-trip (including AckMode, NakAction, MaxTriesAction, TLS)
    let mut s_prof = SenderProfile {
        name: "Prod Outbound".to_string(),
        port: 9100,
        ack_mode: AckMode::LegacySubstring {
            token: "MSA|AA".to_string(),
        },
        nak_action: NakAction::ResetAndResend,
        max_tries_action: MaxTriesAction::Skip,
        ..SenderProfile::default()
    };
    s_prof.tls.enabled = true;
    s_prof.tls.sni_server_name = Some("hl7.hospital.local".to_string());

    let s_path = save_sender_profile_in(&s_prof, dir.path()).unwrap();
    let s_loaded = load_sender_profile_file(&s_path).unwrap();
    assert_eq!(s_loaded, s_prof);

    // Listener profile round-trip
    let l_prof = ListenerProfile {
        name: "Inbound ADT".to_string(),
        port: 9200,
        ack_setting: ListenerAckSetting::GeneratedHl7 {
            code: "AA".to_string(),
        },
        ..ListenerProfile::default()
    };
    let l_path = save_listener_profile_in(&l_prof, dir.path()).unwrap();
    let l_loaded = load_listener_profile_file(&l_path).unwrap();
    assert_eq!(l_loaded, l_prof);
}

// NET-21: Non-loopback listener bind requires explicit confirmation; send scope validation
#[test]
fn non_loopback_bind_requires_confirmation_and_send_scope_validation() {
    let mut net = NetworkPanelState::new();
    net.listener_profile.bind_address = "0.0.0.0".to_string();
    net.listener_profile.port = 19890;

    // Without force_non_loopback, returns Ok(false) and sets pending_non_loopback_confirm
    let started = net.request_start_listener(false, None).unwrap();
    assert!(!started);
    assert!(net.pending_non_loopback_confirm);

    // Invalid 1-based range (start > end) rejected (NET-21)
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    state.paste_from_text(
        "MSH|^~\\&|S|F|R|F|20260101||ADT^A01|M1|P|2.5\rPID|1||P1\r",
        false,
        &settings,
    );
    state.network.send_scope_mode = SendScopeUiMode::Range;
    state.network.range_start = 3;
    state.network.range_end = 1;
    let err = state
        .network
        .start_sender(&state.doc.records, &state.selected_records, None)
        .unwrap_err();
    assert!(err.contains("Invalid send range"));
}

// NET-10..NET-16, NET-20..NET-33, FS-2: End-to-end UI Sender -> UI Listener -> SessionState
#[test]
fn end_to_end_panel_sender_to_listener_appends_records_and_logs_acks_and_history() {
    let settings = AppSettings::default();

    // Receiver session (Listener on loopback:19891)
    let mut recv_session = SessionState::new();
    recv_session.network.listener_profile.bind_address = "127.0.0.1".to_string();
    recv_session.network.listener_profile.port = 19891;
    let started = recv_session
        .network
        .request_start_listener(false, None)
        .unwrap();
    assert!(started);

    // Wait for listener Started event
    let deadline = Instant::now() + Duration::from_secs(2);
    while !recv_session.network.listener_running && Instant::now() < deadline {
        recv_session.poll_network(&settings);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(recv_session.network.listener_running);

    // Sender session with 2 records
    let mut send_session = SessionState::new();
    send_session.paste_from_text(
        concat!(
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|NET_M1|P|2.5.1\rPID|1||PAT1\r",
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ADT^A08|NET_M2|P|2.5.1\rPID|1||PAT2\r",
        ),
        false,
        &settings,
    );
    send_session.network.sender_profile.host = "127.0.0.1".to_string();
    send_session.network.sender_profile.port = 19891;
    send_session.network.modal_ack_popups = true;

    let doc_clone = send_session.doc.records.clone();
    let sel_clone = send_session.selected_records.clone();
    send_session
        .network
        .start_sender(&doc_clone, &sel_clone, None)
        .unwrap();

    // Poll both sessions until 2 records arrive and 2 ACKs are logged
    let deadline = Instant::now() + Duration::from_secs(3);
    while (recv_session.doc.len() < 2 || send_session.network.ack_log.len() < 2)
        && Instant::now() < deadline
    {
        recv_session.poll_network(&settings);
        send_session.poll_network(&settings);
        std::thread::sleep(Duration::from_millis(10));
    }

    // Verify receiver session (NET-12, NET-13, FS-2)
    assert_eq!(recv_session.doc.len(), 2);
    assert_eq!(recv_session.source, DataSource::TcpIp);
    assert_eq!(recv_session.source.display_label(), "TCP/IP Listener");
    // FS-2: TcpIp source has no single_file_path so Save Now falls back to Save As
    assert!(recv_session.source.single_file_path().is_none());
    assert_eq!(recv_session.network.total_received_count, 2);
    assert_eq!(recv_session.network.connections.len(), 1);
    assert_eq!(recv_session.network.connections[0].messages_received, 2);

    // Verify sender session ACK viewer and send history (NET-27, NET-33)
    assert_eq!(send_session.network.ack_log.len(), 2);
    assert_eq!(send_session.network.ack_log[0].heading, "Received ACK");
    assert!(send_session.network.modal_ack_alert.is_some());
    assert_eq!(send_session.network.send_history.len(), 2);
    let csv = send_session.network.format_history_csv();
    assert!(csv.contains("NET_M1") && csv.contains("NET_M2") && csv.contains(",ACK,"));

    recv_session.network.stop_listener();
}

#[test]
fn ack_log_ring_buffer_caps_memory_and_preserves_total_counters() {
    use pipe_wrench::ui::net_panels::{AckViewerEntry, NetworkPanelState, MAX_ACK_LOG_ENTRIES};

    let mut net = NetworkPanelState::new();
    assert_eq!(net.ack_log_capacity, MAX_ACK_LOG_ENTRIES);

    let overflow = 250usize;
    let total = MAX_ACK_LOG_ENTRIES + overflow;
    for i in 0..total {
        let is_malformed = i % 10 == 0;
        net.push_ack_log_entry(AckViewerEntry {
            heading: if is_malformed {
                "Malformed / Unexpected MLLP Response".to_string()
            } else {
                "Received ACK".to_string()
            },
            record_index: i,
            control_id: format!("CTL_{i}"),
            timestamp: "20260115120000".to_string(),
            peer_addr: Some("127.0.0.1:2575".to_string()),
            payload_text: if is_malformed {
                "HTTP/1.1 400 Bad Request<CR><LF>".to_string()
            } else {
                format!("MSH|^~\\&|||||20260115120000||ACK|CTL_{i}|P|2.5.1\nMSA|AA|CTL_{i}")
            },
            is_ack: !is_malformed,
            is_malformed,
            failure_reason: is_malformed.then(|| "Missing MLLP start byte 0x0B".to_string()),
        });
    }

    assert_eq!(net.ack_log.len(), MAX_ACK_LOG_ENTRIES);
    assert_eq!(net.total_ack_responses, total as u64);
    assert_eq!(net.ack_log_dropped_oldest, overflow as u64);
    assert_eq!(net.total_malformed, (total / 10) as u64);
    assert_eq!(net.total_acked, (total - total / 10) as u64);
    assert_eq!(net.ack_log.front().unwrap().record_index, overflow);
    assert_eq!(net.ack_log.back().unwrap().record_index, total - 1);
}
