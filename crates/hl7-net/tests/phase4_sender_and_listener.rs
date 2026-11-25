//! Phase 4 acceptance and retry state machine tests.
//!
//! Covers: NET-10..NET-34, D-18, D-19, D-23, NFR-3..NFR-5.

use tokio::sync::mpsc;

use hl7_net::listener::{run_listener_server, ListenerCommand, ListenerEvent};
use hl7_net::profile::{
    ListenerAckSetting, ListenerProfile, ListenerTlsConfig, MaxTriesAction, NakAction,
    SenderProfile, SenderTlsConfig,
};
use hl7_net::sender::{
    prepare_outgoing_record, run_sender, NextAction, SenderCommand, SenderEvent,
    SenderStateMachine, MSG_SERVER_ABRUPTLY_STOPPED, MSG_SERVER_NOT_ACKING,
};

// NET-28, NET-29, D-18: Table-driven tests covering all 9 cells of NakAction × MaxTriesAction
#[test]
fn retry_state_machine_matrix_9_cells() {
    let nak_actions = [
        NakAction::Resend,
        NakAction::ResetAndResend,
        NakAction::Skip,
    ];
    let max_actions = [
        MaxTriesAction::ResetAndResend,
        MaxTriesAction::Skip,
        MaxTriesAction::CloseAndStop,
    ];

    for nak in nak_actions {
        for max in max_actions {
            let profile = SenderProfile {
                max_tries: 2,
                nak_action: nak,
                max_tries_action: max,
                ..SenderProfile::default()
            };

            let mut sm = SenderStateMachine::new(profile, 1);
            sm.on_send_attempt();

            // First NAK (attempts = 1 < max_tries = 2)
            let action1 = sm.on_nak_or_timeout();

            match nak {
                NakAction::Skip => {
                    // NakAction::Skip skips immediately without reaching max_tries (D-18)
                    assert_eq!(action1, NextAction::Done);
                    assert_eq!(sm.skipped_count(), 1);
                }
                NakAction::Resend => {
                    assert_eq!(action1, NextAction::ResendRecord);
                    sm.on_send_attempt(); // attempts = 2 >= max_tries = 2
                    let action2 = sm.on_nak_or_timeout();
                    match max {
                        MaxTriesAction::ResetAndResend => {
                            assert_eq!(action2, NextAction::ResetConnectionAndResend);
                            assert_eq!(sm.reset_count(), 1);
                            assert_eq!(sm.attempts(), 0); // restarted per NET-29
                        }
                        MaxTriesAction::Skip => {
                            assert_eq!(action2, NextAction::Done);
                            assert_eq!(sm.skipped_count(), 1);
                        }
                        MaxTriesAction::CloseAndStop => {
                            assert!(matches!(action2, NextAction::CloseAndStop(_)));
                        }
                    }
                }
                NakAction::ResetAndResend => {
                    assert_eq!(action1, NextAction::ResetConnectionAndResend);
                    assert_eq!(sm.reset_count(), 1);
                    sm.on_send_attempt(); // attempts = 2 >= max_tries = 2
                    let action2 = sm.on_nak_or_timeout();
                    match max {
                        MaxTriesAction::ResetAndResend => {
                            assert_eq!(action2, NextAction::ResetConnectionAndResend);
                            assert_eq!(sm.reset_count(), 2);
                            assert_eq!(sm.attempts(), 0);
                        }
                        MaxTriesAction::Skip => {
                            assert_eq!(action2, NextAction::Done);
                            assert_eq!(sm.skipped_count(), 1);
                        }
                        MaxTriesAction::CloseAndStop => {
                            assert!(matches!(action2, NextAction::CloseAndStop(_)));
                        }
                    }
                }
            }
        }
    }
}

// NET-31, NET-32: Outgoing copy modifications (MSH-10 rewrite and MSH-7 refresh)
#[test]
fn prepare_outgoing_record_rewrites_msh10_and_msh7() {
    let orig = b"MSH|^~\\&|SND|FAC|RCV|FAC|20000101000000||ADT^A01|ORIG100|P|2.5\rPID|1||P01";

    // 1. Rewrite control ID only
    let (mod1, ctrl_id1) = prepare_outgoing_record(orig, true, false);
    assert_ne!(ctrl_id1, "ORIG100");
    assert!(ctrl_id1.starts_with("ACK"));
    assert!(String::from_utf8_lossy(&mod1).contains(&ctrl_id1));
    assert!(String::from_utf8_lossy(&mod1).contains("20000101000000"));

    // 2. Refresh timestamp only
    let (mod2, ctrl_id2) = prepare_outgoing_record(orig, false, true);
    assert_eq!(ctrl_id2, "ORIG100");
    assert!(!String::from_utf8_lossy(&mod2).contains("20000101000000"));

    // 3. Both
    let (mod3, ctrl_id3) = prepare_outgoing_record(orig, true, true);
    assert_ne!(ctrl_id3, "ORIG100");
    assert!(!String::from_utf8_lossy(&mod3).contains("20000101000000"));
}

// NET-10..NET-16, D-19: In-process multi-connection listener concurrency test
#[tokio::test]
async fn multi_connection_listener_receives_and_acks() {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    // Use ephemeral port 0 or high port
    let profile = ListenerProfile {
        name: "Test Listener".to_string(),
        bind_address: "127.0.0.1".to_string(),
        port: 19876,
        send_ack: true,
        ack_setting: ListenerAckSetting::GeneratedHl7 {
            code: "AA".to_string(),
        },
        ..ListenerProfile::default()
    };

    let server_handle = tokio::spawn(run_listener_server(profile, event_tx, cmd_rx));

    // Wait for Started event
    let start_event = event_rx.recv().await.unwrap();
    assert!(matches!(
        start_event,
        ListenerEvent::Started { port: 19876 }
    ));

    // Connect 2 concurrent clients
    let c1 = tokio::spawn(async {
        use futures_util::{SinkExt, StreamExt};
        use tokio_util::codec::Framed;
        let stream = tokio::net::TcpStream::connect("127.0.0.1:19876")
            .await
            .unwrap();
        let mut framed = Framed::new(stream, hl7_net::envelope::MllpCodec::default());
        framed
            .send(&b"MSH|^~\\&|S1|F|R|F|20260101||ADT^A01|C1_MSG|P|2.5\r"[..])
            .await
            .unwrap();
        let ack = framed.next().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&ack.payload).contains("MSA|AA|C1_MSG"));
    });

    let c2 = tokio::spawn(async {
        use futures_util::{SinkExt, StreamExt};
        use tokio_util::codec::Framed;
        let stream = tokio::net::TcpStream::connect("127.0.0.1:19876")
            .await
            .unwrap();
        let mut framed = Framed::new(stream, hl7_net::envelope::MllpCodec::default());
        framed
            .send(&b"MSH|^~\\&|S2|F|R|F|20260101||ADT^A01|C2_MSG|P|2.5\r"[..])
            .await
            .unwrap();
        let ack = framed.next().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&ack.payload).contains("MSA|AA|C2_MSG"));
    });

    c1.await.unwrap();
    c2.await.unwrap();

    // Stop server
    cmd_tx.send(ListenerCommand::Stop).unwrap();
    let _ = server_handle.await.unwrap();
}

// NET-17, NET-34, D-23: In-process TLS loopback integration test with generated certs
#[tokio::test]
async fn tls_loopback_listener_and_sender_integration() {
    // Generate self-signed cert using rcgen
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    let temp_dir = tempfile::tempdir().unwrap();
    let cert_path = temp_dir.path().join("server.crt");
    let key_path = temp_dir.path().join("server.key");

    std::fs::write(&cert_path, cert_pem).unwrap();
    std::fs::write(&key_path, key_pem).unwrap();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let profile = ListenerProfile {
        name: "TLS Listener".to_string(),
        bind_address: "127.0.0.1".to_string(),
        port: 19877,
        send_ack: true,
        tls: ListenerTlsConfig {
            enabled: true,
            cert_chain_path: Some(cert_path.clone()),
            private_key_path: Some(key_path),
            client_ca_path: None,
        },
        ..ListenerProfile::default()
    };

    let server_handle = tokio::spawn(run_listener_server(profile, event_tx, cmd_rx));
    let _ = event_rx.recv().await.unwrap(); // Started

    // Client connects using SenderTlsConfig with allow_invalid_certificates = true
    let sender_tls_config = SenderTlsConfig {
        enabled: true,
        sni_server_name: Some("localhost".to_string()),
        custom_ca_path: None,
        allow_invalid_certificates: true,
        client_cert_path: None,
        client_key_path: None,
    };

    let connector = hl7_net::tls::build_tls_connector(&sender_tls_config).unwrap();
    let tcp = tokio::net::TcpStream::connect("127.0.0.1:19877")
        .await
        .unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();

    use futures_util::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;
    let mut framed = Framed::new(tls_stream, hl7_net::envelope::MllpCodec::default());
    framed
        .send(&b"MSH|^~\\&|TLS_CLIENT|FAC|TLS_SERVER|FAC|20260101||ADT^A01|TLS_MSG1|P|2.5\r"[..])
        .await
        .unwrap();

    let ack = framed.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&ack.payload).contains("MSA|AA|TLS_MSG1"));

    cmd_tx.send(ListenerCommand::Stop).unwrap();
    let _ = server_handle.await.unwrap();
}

// NET-20..NET-33: Full async run_sender against run_listener_server (ACK, NAK/CloseAndStop, Stop, Abrupt Close)
#[tokio::test]
async fn async_run_sender_end_to_end_ack_nak_stop_and_abrupt_close() {
    // 1. Happy path: 2 records sent to listener with Generated AA ACK
    let (l_ev_tx, mut l_ev_rx) = mpsc::unbounded_channel();
    let (l_cmd_tx, l_cmd_rx) = mpsc::unbounded_channel();
    let l_prof = ListenerProfile {
        port: 19881,
        send_ack: true,
        ack_setting: ListenerAckSetting::GeneratedHl7 {
            code: "AA".to_string(),
        },
        ..ListenerProfile::default()
    };
    let server = tokio::spawn(run_listener_server(l_prof, l_ev_tx, l_cmd_rx));
    let _ = l_ev_rx.recv().await.unwrap();

    let (s_ev_tx, mut s_ev_rx) = mpsc::unbounded_channel();
    let (_s_cmd_tx, s_cmd_rx) = mpsc::unbounded_channel();
    let s_prof = SenderProfile {
        port: 19881,
        delay_after_connect_seconds: 0.01,
        delay_between_records_seconds: 0.01,
        rewrite_control_id: true,
        ..SenderProfile::default()
    };
    let records = vec![
        b"MSH|^~\\&|S|F|R|F|20260101||ADT^A01|M1|P|2.5\rPID|1||P1".to_vec(),
        b"MSH|^~\\&|S|F|R|F|20260101||ADT^A01|M2|P|2.5\rPID|1||P2".to_vec(),
    ];
    run_sender(s_prof, records.clone(), s_ev_tx, s_cmd_rx)
        .await
        .unwrap();

    let mut acks = 0;
    let mut finished_sent = 0;
    while let Ok(ev) = s_ev_rx.try_recv() {
        match ev {
            SenderEvent::AckReceived { is_ack: true, .. } => acks += 1,
            SenderEvent::Finished { sent, .. } => finished_sent = sent,
            _ => {}
        }
    }
    assert_eq!(acks, 2);
    assert_eq!(finished_sent, 2);
    let _ = l_cmd_tx.send(ListenerCommand::Stop);
    let _ = server.await;

    // 2. Immediate Stop while waiting for ACK (NET-22, NET-25)
    let silent_listener = tokio::net::TcpListener::bind("127.0.0.1:19882")
        .await
        .unwrap();
    let accept_task = tokio::spawn(async move {
        let (_sock, _) = silent_listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });

    let (s_ev_tx2, _s_ev_rx2) = mpsc::unbounded_channel();
    let (s_cmd_tx2, s_cmd_rx2) = mpsc::unbounded_channel();
    let s_prof2 = SenderProfile {
        port: 19882,
        ack_wait_seconds: 10.0,
        ..SenderProfile::default()
    };
    let sender_task = tokio::spawn(run_sender(s_prof2, records.clone(), s_ev_tx2, s_cmd_rx2));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    s_cmd_tx2.send(SenderCommand::Stop).unwrap();
    let res = tokio::time::timeout(std::time::Duration::from_millis(500), sender_task)
        .await
        .expect("Stop must cancel wait_for_ack immediately");
    assert!(res.unwrap().is_ok());
    accept_task.abort();

    // 3. Abrupt close by server (NET-30)
    let drop_listener = tokio::net::TcpListener::bind("127.0.0.1:19883")
        .await
        .unwrap();
    tokio::spawn(async move {
        let (sock, _) = drop_listener.accept().await.unwrap();
        drop(sock);
    });
    let (s_ev_tx3, mut s_ev_rx3) = mpsc::unbounded_channel();
    let (_s_cmd_tx3, s_cmd_rx3) = mpsc::unbounded_channel();
    let s_prof3 = SenderProfile {
        port: 19883,
        ..SenderProfile::default()
    };
    let err = run_sender(s_prof3, records.clone(), s_ev_tx3, s_cmd_rx3)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(MSG_SERVER_ABRUPTLY_STOPPED));
    let mut saw_abrupt_msg = false;
    while let Ok(ev) = s_ev_rx3.try_recv() {
        if let SenderEvent::StatusMessage(m) = ev {
            if m == MSG_SERVER_ABRUPTLY_STOPPED {
                saw_abrupt_msg = true;
            }
        }
    }
    assert!(saw_abrupt_msg);

    // 4. Max tries exceeded -> CloseAndStop "Remote host is not acknowledging records." (NET-29)
    let (l_ev_tx4, mut l_ev_rx4) = mpsc::unbounded_channel();
    let (l_cmd_tx4, l_cmd_rx4) = mpsc::unbounded_channel();
    let l_prof4 = ListenerProfile {
        port: 19884,
        send_ack: true,
        ack_setting: ListenerAckSetting::GeneratedHl7 {
            code: "AR".to_string(),
        },
        ..ListenerProfile::default()
    };
    let server4 = tokio::spawn(run_listener_server(l_prof4, l_ev_tx4, l_cmd_rx4));
    let _ = l_ev_rx4.recv().await.unwrap();

    let (s_ev_tx4, _s_ev_rx4) = mpsc::unbounded_channel();
    let (_s_cmd_tx4, s_cmd_rx4) = mpsc::unbounded_channel();
    let s_prof4 = SenderProfile {
        port: 19884,
        max_tries: 2,
        nak_action: NakAction::Resend,
        max_tries_action: MaxTriesAction::CloseAndStop,
        ..SenderProfile::default()
    };
    let err4 = run_sender(s_prof4, records, s_ev_tx4, s_cmd_rx4)
        .await
        .unwrap_err();
    assert!(err4.to_string().contains(MSG_SERVER_NOT_ACKING));
    let _ = l_cmd_tx4.send(ListenerCommand::Stop);
    let _ = server4.await;
}

#[tokio::test]
async fn mllp_listener_and_sender_log_peer_addr_and_malformed_packets() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    // --- Part A: Listener receiving (a) HTTP garbage, (b) truncated 0x0B frame, (c) unframed raw HL7 ---
    let (l_ev_tx, mut l_ev_rx) = mpsc::unbounded_channel();
    let (l_cmd_tx, l_cmd_rx) = mpsc::unbounded_channel();
    let l_prof = ListenerProfile {
        port: 19891,
        send_ack: false,
        ..ListenerProfile::default()
    };
    let server = tokio::spawn(run_listener_server(l_prof, l_ev_tx, l_cmd_rx));
    let _ = l_ev_rx.recv().await.unwrap(); // Started

    let payloads: &[(&[u8], &str, &str)] = &[
        (
            b"HTTP/1.1 400 Bad Request\r\n",
            "Missing MLLP start byte",
            "HTTP/1.1 400 Bad Request<CR><LF>",
        ),
        (
            b"\x0BMSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|TRUNC1|P|2.5.1\r",
            "Incomplete MLLP frame",
            "<VT>MSH|^~\\&|",
        ),
        (
            b"MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|UNFRAMED1|P|2.5.1\r",
            "Unframed raw HL7",
            "MSH|^~\\&|",
        ),
    ];

    for (raw, expected_reason_sub, expected_preview_sub) in payloads {
        let mut client = TcpStream::connect("127.0.0.1:19891").await.unwrap();
        let local_port = client.local_addr().unwrap().port();
        client.write_all(raw).await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        let mut found_malformed = None;
        for _ in 0..8 {
            if let Ok(Some(ListenerEvent::MalformedPacket { info, .. })) =
                tokio::time::timeout(std::time::Duration::from_secs(2), l_ev_rx.recv()).await
            {
                found_malformed = Some(info);
                break;
            }
        }
        let info = found_malformed.expect("expected ListenerEvent::MalformedPacket");
        assert_eq!(info.peer_addr.ip().to_string(), "127.0.0.1");
        assert_eq!(info.peer_addr.port(), local_port);
        assert!(
            info.reason.contains(expected_reason_sub),
            "unexpected reason: {}",
            info.reason
        );
        assert!(
            info.raw_preview.contains(expected_preview_sub),
            "unexpected preview: {}",
            info.raw_preview
        );
    }

    let _ = l_cmd_tx.send(ListenerCommand::Stop);
    let _ = server.await;

    // --- Part B: Sender receiving (a) HTTP garbage before EOF, (b) truncated 0x0B before timeout, (c) unframed HL7 before EOF ---
    let outbound = vec![b"MSH|^~\\&|A|B|C|D|20260115120000||ADT^A01|M1|P|2.5.1\r".to_vec()];

    let sender_cases: &[(&[u8], bool, &str, &str)] = &[
        (
            b"HTTP/1.1 400 Bad Request\r\n",
            true, // close immediately -> EOF
            "Missing MLLP start byte",
            "HTTP/1.1 400 Bad Request<CR><LF>",
        ),
        (
            b"\x0BMSH|^~\\&|SRV|FAC|CLI|FAC|20260115120000||ACK|M1|P|2.5.1\r",
            false, // keep open -> timeout with buffered bytes
            "Incomplete MLLP frame",
            "<VT>MSH|^~\\&|",
        ),
        (
            b"MSH|^~\\&|SRV|FAC|CLI|FAC|20260115120000||ACK|M1|P|2.5.1\r",
            true, // close immediately -> EOF
            "Unframed raw HL7",
            "MSH|^~\\&|",
        ),
    ];

    for (raw_resp, close_immediately, expected_reason_sub, expected_preview_sub) in sender_cases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let raw_owned = raw_resp.to_vec();
        let close_imm = *close_immediately;

        let mock_srv = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut tmp = [0u8; 512];
            let _ = sock.read(&mut tmp).await;
            let _ = sock.write_all(&raw_owned).await;
            if close_imm {
                let _ = sock.shutdown().await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
        });

        let (s_ev_tx, mut s_ev_rx) = mpsc::unbounded_channel();
        let (_s_cmd_tx, s_cmd_rx) = mpsc::unbounded_channel();
        let s_prof = SenderProfile {
            host: "127.0.0.1".to_string(),
            port,
            ack_wait_seconds: 0.15,
            max_tries: 1,
            nak_action: NakAction::Skip,
            ..SenderProfile::default()
        };
        let _ = run_sender(s_prof, outbound.clone(), s_ev_tx, s_cmd_rx).await;
        let _ = mock_srv.await;

        let mut malformed = None;
        while let Ok(ev) = s_ev_rx.try_recv() {
            if let SenderEvent::MalformedPacket { info, .. } = ev {
                malformed = Some(info);
                break;
            }
        }
        let info = malformed.expect("expected SenderEvent::MalformedPacket");
        assert_eq!(info.peer_addr.ip().to_string(), "127.0.0.1");
        assert_eq!(info.peer_addr.port(), port);
        assert!(
            info.reason.contains(expected_reason_sub),
            "unexpected sender reason: {}",
            info.reason
        );
        assert!(
            info.raw_preview.contains(expected_preview_sub),
            "unexpected sender preview: {}",
            info.raw_preview
        );
    }
}

#[tokio::test]
async fn enhanced_mode_msh15_msh16_opt_in_and_always_on_msa2_mismatch_warning() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let ne_msg = b"MSH|^~\\&|SND_APP|SND_FAC|RCV_APP|RCV_FAC|20261123120000||ADT^A01|NE_CTRL_1|P|2.5.1|||NE|NE\rPID|1||P100\r".to_vec();

    // 1. Default (`respect_ack_modes = false`): MSH-15=NE|MSH-16=NE still receives Auto-ACK
    //    with swapped MSH-3/4 <-> MSH-5/6 and MSA-2 == inbound MSH-10.
    {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (l_ev_tx, mut l_ev_rx) = mpsc::unbounded_channel();
        let (l_cmd_tx, l_cmd_rx) = mpsc::unbounded_channel();
        let l_prof = ListenerProfile {
            bind_address: "127.0.0.1".to_string(),
            port,
            send_ack: true,
            respect_ack_modes: false,
            ..ListenerProfile::default()
        };
        let server = tokio::spawn(run_listener_server(l_prof, l_ev_tx, l_cmd_rx));
        loop {
            if let Some(ListenerEvent::Started { .. }) = l_ev_rx.recv().await {
                break;
            }
        }

        let (s_ev_tx, mut s_ev_rx) = mpsc::unbounded_channel();
        let (_s_cmd_tx, s_cmd_rx) = mpsc::unbounded_channel();
        let s_prof = SenderProfile {
            host: "127.0.0.1".to_string(),
            port,
            wait_for_ack: true,
            respect_ack_modes: false,
            ack_wait_seconds: 2.0,
            ..SenderProfile::default()
        };
        run_sender(s_prof, vec![ne_msg.clone()], s_ev_tx, s_cmd_rx)
            .await
            .unwrap();

        let mut received_ack: Option<Vec<u8>> = None;
        while let Ok(ev) = s_ev_rx.try_recv() {
            if let SenderEvent::AckReceived {
                response, is_ack, ..
            } = ev
            {
                assert!(is_ack, "default mode must receive valid Auto-ACK");
                received_ack = Some(response);
            }
        }
        let ack_bytes = received_ack.expect("expected Auto-ACK when respect_ack_modes=false");
        let ack_str = String::from_utf8_lossy(&ack_bytes);
        assert!(
            ack_str.starts_with("MSH|^~\\&|RCV_APP|RCV_FAC|SND_APP|SND_FAC|"),
            "Auto-ACK must swap MSH-3/4 <-> MSH-5/6: {ack_str}"
        );
        assert!(
            ack_str.contains("MSA|AA|NE_CTRL_1"),
            "Auto-ACK must echo inbound MSH-10 into MSA-2: {ack_str}"
        );

        let _ = l_cmd_tx.send(ListenerCommand::Stop);
        let _ = server.await;
    }

    // 2. Opt-in (`respect_ack_modes = true`): sender skips ACK wait without timing out,
    //    and listener suppresses Auto-ACK with warning log.
    {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (l_ev_tx, mut l_ev_rx) = mpsc::unbounded_channel();
        let (l_cmd_tx, l_cmd_rx) = mpsc::unbounded_channel();
        let l_prof = ListenerProfile {
            bind_address: "127.0.0.1".to_string(),
            port,
            send_ack: true,
            respect_ack_modes: true,
            ..ListenerProfile::default()
        };
        let server = tokio::spawn(run_listener_server(l_prof, l_ev_tx, l_cmd_rx));
        loop {
            if let Some(ListenerEvent::Started { .. }) = l_ev_rx.recv().await {
                break;
            }
        }

        let (s_ev_tx, mut s_ev_rx) = mpsc::unbounded_channel();
        let (_s_cmd_tx, s_cmd_rx) = mpsc::unbounded_channel();
        let s_prof = SenderProfile {
            host: "127.0.0.1".to_string(),
            port,
            wait_for_ack: true,
            respect_ack_modes: true,
            ack_wait_seconds: 2.0,
            ..SenderProfile::default()
        };
        run_sender(s_prof, vec![ne_msg.clone()], s_ev_tx, s_cmd_rx)
            .await
            .unwrap();

        let mut saw_ne_outcome = false;
        while let Ok(ev) = s_ev_rx.try_recv() {
            if let SenderEvent::History(h) = ev {
                if h.outcome.contains("No ACK expected") && h.outcome.contains("MSH-15/16=NE") {
                    saw_ne_outcome = true;
                }
            }
        }
        assert!(
            saw_ne_outcome,
            "sender must record 'Sent (No ACK expected — MSH-15/16=NE)' when respect_ack_modes=true"
        );

        let mut saw_suppressed = false;
        for _ in 0..8 {
            if let Ok(Some(ListenerEvent::WarningMessage(msg))) =
                tokio::time::timeout(std::time::Duration::from_millis(500), l_ev_rx.recv()).await
            {
                if msg.contains("ACK suppressed per MSH-15/16=NE") {
                    saw_suppressed = true;
                    break;
                }
            }
        }
        assert!(
            saw_suppressed,
            "listener must emit 'ACK suppressed per MSH-15/16=NE' when respect_ack_modes=true"
        );

        let _ = l_cmd_tx.send(ListenerCommand::Stop);
        let _ = server.await;
    }

    // 3. Always-on MSA-2 mismatch warning on sender
    {
        let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = mock_listener.local_addr().unwrap().port();
        let mock_srv = tokio::spawn(async move {
            let (mut sock, _) = mock_listener.accept().await.unwrap();
            let mut tmp = [0u8; 512];
            let _ = sock.read(&mut tmp).await;
            let wrong_ack = b"\x0BMSH|^~\\&|RCV|FAC|SND|FAC|20261123||ACK|A1|P|2.5.1\rMSA|AA|WRONG_CTRL_ID\r\x1C\r";
            let _ = sock.write_all(wrong_ack).await;
            let _ = sock.shutdown().await;
        });

        let (s_ev_tx, mut s_ev_rx) = mpsc::unbounded_channel();
        let (_s_cmd_tx, s_cmd_rx) = mpsc::unbounded_channel();
        let s_prof = SenderProfile {
            host: "127.0.0.1".to_string(),
            port,
            wait_for_ack: true,
            respect_ack_modes: false,
            ack_wait_seconds: 1.0,
            max_tries: 1,
            nak_action: NakAction::Skip,
            ..SenderProfile::default()
        };
        let _ = run_sender(s_prof, vec![ne_msg], s_ev_tx, s_cmd_rx).await;
        let _ = mock_srv.await;

        let mut saw_mismatch_warning = false;
        while let Ok(ev) = s_ev_rx.try_recv() {
            if let SenderEvent::StatusMessage(msg) = ev {
                if msg.contains("MSA-2 mismatch") && msg.contains("WRONG_CTRL_ID") {
                    saw_mismatch_warning = true;
                }
            }
        }
        assert!(
            saw_mismatch_warning,
            "sender must emit a non-fatal MSA-2 mismatch warning when MSA-2 != outbound MSH-10"
        );
    }
}
