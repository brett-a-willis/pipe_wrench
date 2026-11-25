//! Phase 23 integration tests covering:
//! 1. Strict TCP/IP (MLLP) input validation across Sender & Listener ports, timeouts, delays,
//!    retry counts, send ranges, host/bind addresses, and headless UI rendering.
//! 2. Comprehensive copy/paste edge cases: CRLF/LF normalization, multi-message & FHS/BHS/BTS/FTS
//!    batch paste, MLLP framing byte stripping (`\x0B...\x1C\x0D`), custom non-standard MSH
//!    delimiters, invalid/empty clipboard rejection, and multi-selection copy consistency
//!    (including empty fields and HL7 escape sequences).

use hl7_core::{Delimiters, FieldAddress, RecordKind};
use pipe_wrench::ui::net_panels::{NetworkPanelState, SendScopeUiMode, TcpIpDockTab};
use pipe_wrench::{AppSettings, PipeWrenchApp, SessionState};

fn make_raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    }
}

#[test]
fn tcpip_sender_and_listener_strict_input_validation_and_ui_render() {
    let mut net = NetworkPanelState::new();

    // Defaults are valid
    assert!(net.validate_sender_inputs().is_valid());
    assert!(net.validate_listener_inputs().is_valid());

    // 1. Sender & Listener Port validation (digits-only, 1..=65535)
    for bad_port in ["", "0", "65536", "70000", "abc", "-1", "77.72", " 7772a"] {
        net.sender_port_input = bad_port.to_string();
        assert!(
            net.validate_sender_inputs().port.is_some(),
            "expected sender port error for '{bad_port}'"
        );
        net.listener_port_input = bad_port.to_string();
        assert!(
            net.validate_listener_inputs().port.is_some(),
            "expected listener port error for '{bad_port}'"
        );
    }
    for good_port in ["1", "7772", "65535"] {
        net.sender_port_input = good_port.to_string();
        assert!(
            net.validate_sender_inputs().port.is_none(),
            "expected valid sender port for '{good_port}'"
        );
        net.listener_port_input = good_port.to_string();
        assert!(
            net.validate_listener_inputs().port.is_none(),
            "expected valid listener port for '{good_port}'"
        );
    }

    // 2. Host / Bind address validation (trimmed, non-empty)
    net.sender_profile.host = "   ".to_string();
    assert!(net.validate_sender_inputs().host.is_some());
    assert!(net.start_sender(&[], &Default::default(), None).is_err());
    net.sender_profile.host = "  127.0.0.1  ".to_string();
    assert!(net.validate_sender_inputs().host.is_none());

    net.listener_profile.bind_address = "   ".to_string();
    assert!(net.validate_listener_inputs().bind_address.is_some());
    assert!(net.request_start_listener(true, None).is_err());
    net.listener_profile.bind_address = " 127.0.0.1 ".to_string();
    assert!(net.validate_listener_inputs().bind_address.is_none());

    // 3. Timeouts, delays, max tries, and range validation
    net.sender_connect_timeout_input = "0".to_string();
    assert!(net.validate_sender_inputs().connect_timeout.is_some());
    net.sender_connect_timeout_input = "abc".to_string();
    assert!(net.validate_sender_inputs().connect_timeout.is_some());
    net.sender_connect_timeout_input = "5.5".to_string();
    assert!(net.validate_sender_inputs().connect_timeout.is_none());

    net.sender_delay_connect_input = "-1".to_string();
    assert!(net.validate_sender_inputs().delay_after_connect.is_some());
    net.sender_delay_connect_input = "0".to_string();
    assert!(net.validate_sender_inputs().delay_after_connect.is_none());

    net.sender_delay_between_input = "nope".to_string();
    assert!(net.validate_sender_inputs().delay_between_records.is_some());
    net.sender_delay_between_input = "0.25".to_string();
    assert!(net.validate_sender_inputs().delay_between_records.is_none());

    net.sender_profile.wait_for_ack = true;
    net.sender_ack_wait_input = "0".to_string();
    assert!(net.validate_sender_inputs().ack_wait.is_some());
    net.sender_ack_wait_input = "30".to_string();
    assert!(net.validate_sender_inputs().ack_wait.is_none());

    net.sender_max_tries_input = "0".to_string();
    assert!(net.validate_sender_inputs().max_tries.is_some());
    net.sender_max_tries_input = "101".to_string();
    assert!(net.validate_sender_inputs().max_tries.is_some());
    net.sender_max_tries_input = "3".to_string();
    assert!(net.validate_sender_inputs().max_tries.is_none());

    net.send_scope_mode = SendScopeUiMode::Range;
    net.range_start_input = "5".to_string();
    net.range_end_input = "2".to_string();
    assert!(net.validate_sender_inputs().range.is_some());
    net.range_start_input = "1".to_string();
    net.range_end_input = "5".to_string();
    assert!(net.validate_sender_inputs().range.is_none());
    assert!(net.validate_sender_inputs().is_valid());

    // 4. Headless UI render with invalid Sender & Listener fields (error borders + disabled buttons)
    let mut app = PipeWrenchApp::new();
    app.state.network.open_dock_tab(TcpIpDockTab::ClientSender);
    app.state.network.sender_profile.host.clear();
    app.state.network.sender_port_input = "99999".to_string();
    app.state.network.sender_connect_timeout_input = "0".to_string();
    let ctx = egui::Context::default();
    let _ = ctx.run(make_raw_input(), |ctx| {
        app.render_frame(ctx);
    });

    app.state
        .network
        .open_dock_tab(TcpIpDockTab::ServerListener);
    app.state.network.listener_profile.bind_address.clear();
    app.state.network.listener_port_input = "0".to_string();
    let _ = ctx.run(make_raw_input(), |ctx| {
        app.render_frame(ctx);
    });
}

#[test]
fn copy_paste_edge_cases_line_endings_batches_mllp_custom_delims_invalid_and_multi_copy() {
    let settings = AppSettings::default();

    // 1. Line-ending normalization on paste: CRLF (\r\n) and LF (\n)
    for (label, payload) in [
        (
            "CRLF",
            "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ADT^A01|M1|P|2.5.1\r\nPID|1||1001^^^HOSP^MR||DOE^JANE\r\nPV1|1|I\r\n",
        ),
        (
            "LF",
            "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ADT^A01|M2|P|2.5.1\nPID|1||1002^^^HOSP^MR||SMITH^JOHN\nPV1|1|O\n",
        ),
    ] {
        let mut state = SessionState::new();
        let pasted = state.paste_records_undoable(payload, &settings).unwrap();
        assert_eq!(pasted, 1, "case {label}");
        assert_eq!(state.doc.len(), 1, "case {label}");
        assert_eq!(state.doc.records[0].segments().len(), 3, "case {label}");
        assert_eq!(state.doc.records[0].segment_bytes(1).unwrap()[0..3], *b"PID");
        assert_eq!(state.doc.records[0].segment_bytes(2).unwrap()[0..3], *b"PV1");
    }

    // 2. Multi-message back-to-back MSH paste AND FHS/BHS/BTS/FTS batch-wrapped paste
    {
        let mut state = SessionState::new();
        let multi_msh = concat!(
            "MSH|^~\\&|A|F|R|F|20261123010000||ADT^A01|ID1|P|2.5.1\r\n",
            "PID|1||P1||ALPHA^ONE\r\n",
            "MSH|^~\\&|A|F|R|F|20261123020000||ORU^R01|ID2|P|2.5.1\r\n",
            "PID|1||P2||BETA^TWO\r\n",
            "MSH|^~\\&|A|F|R|F|20261123030000||ORM^O01|ID3|P|2.5.1\r\n",
            "PID|1||P3||GAMMA^THREE\r\n",
        );
        let pasted = state.paste_records_undoable(multi_msh, &settings).unwrap();
        assert_eq!(pasted, 3);
        assert_eq!(state.summaries[0].control_id, "ID1");
        assert_eq!(state.summaries[1].control_id, "ID2");
        assert_eq!(state.summaries[2].control_id, "ID3");

        let mut batch_state = SessionState::new();
        let batch_payload = concat!(
            "FHS|^~\\&|SND|FAC|RCV|FAC|20261123||FILE1||FCID1\r",
            "BHS|^~\\&|SND|FAC|RCV|FAC|20261123||BATCH1||BCID1\r",
            "MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A01|BM1|P|2.5.1\r",
            "PID|1||BP1||BATCH^ONE\r",
            "MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A08|BM2|P|2.5.1\r",
            "PID|1||BP2||BATCH^TWO\r",
            "BTS|2\r",
            "FTS|1\r",
        );
        let batch_count = batch_state
            .paste_records_undoable(batch_payload, &settings)
            .unwrap();
        assert_eq!(batch_count, 6);
        assert_eq!(batch_state.doc.records[0].kind(), RecordKind::Fhs);
        assert_eq!(batch_state.doc.records[1].kind(), RecordKind::Bhs);
        assert_eq!(batch_state.doc.records[2].kind(), RecordKind::Msh);
        assert_eq!(batch_state.doc.records[3].kind(), RecordKind::Msh);
        assert_eq!(batch_state.doc.records[4].kind(), RecordKind::Bts);
        assert_eq!(batch_state.doc.records[5].kind(), RecordKind::Fts);
    }

    // 3. MLLP framing byte stripping on paste (\x0B ... \x1C\x0D)
    {
        let mut state = SessionState::new();
        let mllp_framed = concat!(
            "\x0bMSH|^~\\&|MLLP|FAC|RCV|FAC|20261123||ADT^A01|MLLP1|P|2.5.1\r",
            "PID|1||M100||FRAMED^ONE\r\x1c\r",
            "\x0bMSH|^~\\&|MLLP|FAC|RCV|FAC|20261123||ORU^R01|MLLP2|P|2.5.1\r",
            "PID|1||M200||FRAMED^TWO\r\x1c\r",
        );
        let count = state
            .paste_records_undoable(mllp_framed, &settings)
            .unwrap();
        assert_eq!(count, 2);
        for rec in &state.doc.records {
            assert!(!rec.bytes().contains(&0x0B));
            assert!(!rec.bytes().contains(&0x1C));
            assert!(rec.bytes().starts_with(b"MSH|"));
        }
        assert_eq!(state.summaries[0].control_id, "MLLP1");
        assert_eq!(state.summaries[1].control_id, "MLLP2");
    }

    // 4. Non-standard MSH delimiters on paste (MSH:^~\&:...)
    {
        let mut state = SessionState::new();
        let custom_delim_msg = concat!(
            "MSH:^~\\&:CUSTAPP:CUSTFAC:RCV:FAC:20261123::ADT^A01:CUST1:P:2.5.1\r\n",
            "PID:1::C123^^^HOSP^MR::CUSTOM^DELIM\r\n",
        );
        let count = state
            .paste_records_undoable(custom_delim_msg, &settings)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            state.doc.records[0].delimiters(),
            Delimiters::from_five_bytes(*b":^~\\&").unwrap()
        );
        let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
        assert_eq!(
            state.doc.records[0].field_bytes(1, &pid5_1),
            Some(b"CUSTOM".as_slice())
        );
    }

    // 5. Invalid / empty / whitespace-only clipboard handling
    {
        let mut state = SessionState::new();
        for bad in ["", "   \r\n\t  ", "not an hl7 message at all"] {
            let err = state.paste_records_undoable(bad, &settings).unwrap_err();
            assert!(!err.is_empty());
            assert_eq!(state.doc.len(), 0);
            assert!(!state.status_message.is_empty());
        }
    }

    // 6. Multi-selection Copy consistency (Messages list + Segment Accordion with empty & escaped values)
    {
        let mut state = SessionState::new();
        let raw = concat!(
            "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ADT^A01|CPY1|P|2.5.1\r",
            "PID|1||ID1^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|TX|NOTE||Line 1\\.br\\Line 2 \\F\\ pipe \\S\\ caret\r",
            "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ORU^R01|CPY2|P|2.5.1\r",
            "PID|1||ID2^^^HOSP^MR||SMITH^JOHN||19750505|M\r",
        );
        state.paste_from_text(raw, false, &settings);

        // Multi-select both messages in Messages list and verify ordered round-trip copy
        state.select_record_with_modifiers(0, false, false, &settings);
        state.select_record_with_modifiers(1, false, true, &settings);
        let copied_msgs = state.format_selected_records_for_clipboard(&settings);
        let idx1 = copied_msgs.find("CPY1").unwrap();
        let idx2 = copied_msgs.find("CPY2").unwrap();
        assert!(idx1 < idx2, "copied messages must preserve display order");

        let mut rt_state = SessionState::new();
        assert_eq!(
            rt_state
                .paste_records_undoable(&copied_msgs, &settings)
                .unwrap(),
            2
        );
        assert_eq!(rt_state.summaries[0].control_id, "CPY1");
        assert_eq!(rt_state.summaries[1].control_id, "CPY2");

        // Segment Accordion multi-select Copy with an empty field (Show Empty) AND an escaped value
        state.select_record(0, false, &settings);
        state.set_show_empty_fields(true, &settings);

        let find_row = |s: &SessionState, path: &str| -> usize {
            let addr: FieldAddress = path.parse().unwrap();
            s.record_cache
                .as_ref()
                .unwrap()
                .field_rows
                .iter()
                .find(|f| f.address == Some(addr))
                .unwrap()
                .row_index
        };
        let row_empty_pid2 = find_row(&state, "PID-2");
        let row_pid5_1 = find_row(&state, "PID-5.1");
        let row_obx5 = find_row(&state, "OBX-5");

        state.select_field_row_with_modifiers(row_empty_pid2, false, false);
        state.select_field_row_with_modifiers(row_pid5_1, true, false);
        state.select_field_row_with_modifiers(row_obx5, true, false);

        assert_eq!(state.format_accordion_copy_paths(), "PID-2\nPID-5.1\nOBX-5");
        assert_eq!(
            state.format_accordion_copy_values(),
            "\nDOE\nLine 1\\.br\\Line 2 \\F\\ pipe \\S\\ caret"
        );
        assert_eq!(
            state.format_accordion_copy_summary(),
            "PID-2 (Patient ID): \nPID-5.1 (Family Name): DOE\nOBX-5 (Observation Value): Line 1\\.br\\Line 2 \\F\\ pipe \\S\\ caret"
        );
    }
}

#[test]
fn hardened_clipboard_paste_and_binary_file_ingestion_guardrails() {
    let settings = AppSettings::default();

    // 1. Random Word/prose text, JSON, XML, and HTML rejected cleanly with user notice & zero records
    for prose in [
        "Meeting notes for Tuesday: review Q4 HL7 interface roadmap and MSH migration plan.",
        "{\"message\": \"not hl7\"}",
        "<html><body><h1>Status Report</h1></body></html>",
        "MSH without a valid single-char separator",
    ] {
        let mut state = SessionState::new();
        let err = state.paste_records_undoable(prose, &settings).unwrap_err();
        assert!(
            err.contains("No valid HL7 messages found"),
            "unexpected error for prose '{prose}': {err}"
        );
        assert_eq!(state.doc.len(), 0);
        assert!(!state.undo_stack.can_undo());
        assert_eq!(state.status_message, err);
    }

    // 2. Simulated non-text/image clipboard error (`ContentNotAvailable`) handled gracefully
    {
        let mut state = SessionState::new();
        let err = state
            .paste_from_clipboard_result(Err("ContentNotAvailable"), false, &settings)
            .unwrap_err();
        assert!(
            err.contains("Clipboard does not contain text") && err.contains("MSH, FHS, or BHS"),
            "unexpected clipboard error notice: {err}"
        );
        assert_eq!(state.doc.len(), 0);
        assert_eq!(state.status_message, err);
    }

    // 3. Binary / .docx magic-byte payloads (`PK\x03\x04...`, `%PDF-`, embedded `\0`) rejected in paste & file load
    {
        let mut state = SessionState::new();
        for bin_str in [
            "PK\x03\x04\x14\x00\x06\x00word/document.xml",
            "%PDF-1.7\n1 0 obj",
            "MSH|^~\\&|APP\0|FAC|RCV|FAC|20261123||ADT^A01|M1|P|2.5.1\r",
        ] {
            let err = state
                .paste_records_undoable(bin_str, &settings)
                .unwrap_err();
            assert!(
                err.contains("Unsupported binary file or non-HL7 content"),
                "unexpected binary paste notice: {err}"
            );
            assert_eq!(state.doc.len(), 0);
        }

        let dir = tempfile::tempdir().unwrap();
        let docx_path = dir.path().join("notes.docx");
        std::fs::write(&docx_path, b"PK\x03\x04\x14\x00\x06\x00[Content_Types].xml").unwrap();
        let docx_err = state
            .load_files_sync(
                std::slice::from_ref(&docx_path),
                hl7_core::RecoveryMode::Strict,
                &settings,
            )
            .unwrap_err();
        assert!(
            docx_err.contains("Unsupported binary file or non-HL7 content"),
            "unexpected docx load error: {docx_err}"
        );
        assert_eq!(state.doc.len(), 0);

        let nul_bin_path = dir.path().join("corrupt.dat");
        std::fs::write(&nul_bin_path, b"\x00\x01\x02\x03BINARY_DATA\x00").unwrap();
        let bin_err = state
            .load_files_sync(
                std::slice::from_ref(&nul_bin_path),
                hl7_core::RecoveryMode::RecoverNonHl7,
                &settings,
            )
            .unwrap_err();
        assert!(
            bin_err.contains("Unsupported binary file or non-HL7 content"),
            "unexpected binary file load error: {bin_err}"
        );
        assert_eq!(state.doc.len(), 0);
    }

    // 4. Genuine HL7 text prefixed with UTF-8 BOM (\u{FEFF}) or non-breaking spaces (\u{00A0}) parses cleanly
    {
        let mut state = SessionState::new();
        let word_copied = concat!(
            "\u{FEFF}\u{00A0}  MSH|^~\\&|WORD|FAC|RCV|FAC|20261123||ADT^A01|BOM1|P|2.5.1\r\n",
            "\u{00A0}PID|1||W123^^^HOSP^MR||WORD^PATIENT\r\n",
        );
        let pasted = state
            .paste_records_undoable(word_copied, &settings)
            .unwrap();
        assert_eq!(pasted, 1);
        assert_eq!(state.doc.len(), 1);
        assert_eq!(state.summaries[0].control_id, "BOM1");
        let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
        assert_eq!(
            state.doc.records[0].field_bytes(1, &pid5_1),
            Some(b"WORD".as_slice())
        );
    }
}

#[test]
fn utf8_bom_and_windows1252_legacy_file_loading_with_fallback_notice() {
    let settings = AppSettings::default();
    let dir = tempfile::tempdir().unwrap();

    // 1. File with leading UTF-8 BOM (0xEF 0xBB 0xBF) loads cleanly in Strict mode
    let bom_path = dir.path().join("bom_message.hl7");
    let mut bom_bytes = vec![0xEF, 0xBB, 0xBF];
    bom_bytes.extend_from_slice(
        b"MSH|^~\\&|APP|FAC|RCV|FAC|20261123||ADT^A01|BOMFILE1|P|2.5.1\rPID|1||P1||DOE^JANE\r",
    );
    std::fs::write(&bom_path, &bom_bytes).unwrap();

    let mut state = SessionState::new();
    let loaded = state
        .load_files_sync(
            std::slice::from_ref(&bom_path),
            hl7_core::RecoveryMode::Strict,
            &settings,
        )
        .unwrap();
    assert_eq!(loaded, 1);
    assert_eq!(state.summaries[0].control_id, "BOMFILE1");

    // 2. Legacy Windows-1252 / Latin-1 file (non-UTF-8 accented bytes: MÜLLER = 0xDC, JOSÉ = 0xC9)
    // loads cleanly without failing UTF-8 decoding and surfaces a non-blocking fallback notice
    let latin1_path = dir.path().join("legacy_latin1.log");
    let mut latin1_bytes = Vec::new();
    latin1_bytes.extend_from_slice(
        b"MSH|^~\\&|LEGACY|FAC|RCV|FAC|20261123||ADT^A01|LATIN1|P|2.5.1\rPID|1||P2||M",
    );
    latin1_bytes.push(0xDC); // Ü in Windows-1252 / ISO-8859-1
    latin1_bytes.extend_from_slice(b"LLER^JOS");
    latin1_bytes.push(0xC9); // É in Windows-1252 / ISO-8859-1
    latin1_bytes.push(b'\r');
    std::fs::write(&latin1_path, &latin1_bytes).unwrap();

    let mut state2 = SessionState::new();
    let loaded2 = state2
        .load_files_sync(
            std::slice::from_ref(&latin1_path),
            hl7_core::RecoveryMode::Strict,
            &settings,
        )
        .unwrap();
    assert_eq!(loaded2, 1);
    assert!(
        state2.status_message.contains("Windows-1252/Latin-1"),
        "expected fallback decoding notice, got: {}",
        state2.status_message
    );
    let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
    let decoded_family = state2.doc.records[0]
        .charset()
        .decode_display(state2.doc.records[0].field_bytes(1, &pid5_1).unwrap())
        .into_owned();
    assert_eq!(decoded_family, "MÜLLER");
}

#[test]
fn msh8_empty_population_short_msh2_and_inline_delimiter_auto_escaping() {
    let settings = AppSettings::default();

    // 1. Short MSH-2 (< 4 encoding chars, e.g. `MSH|^~|...`) detects delimiters cleanly
    let short_delims =
        Delimiters::from_header_bytes(b"MSH|^~|APP|FAC|RCV|FAC|20261123||ADT^A01|S1|P|2.5.1")
            .expect("short MSH-2 should parse");
    assert_eq!(short_delims.field, b'|');
    assert_eq!(short_delims.component, b'^');
    assert_eq!(short_delims.repetition, b'~');

    // 2. Populating empty MSH-8 (Security) via Show Empty does NOT shift MSH-9, MSH-10, or MSH-12
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|DFAC|20261123120000||ADT^A01|CTRL99|P|2.5.1\r",
        "PID|1||1001^^^HOSP^MR||DOE^JANE\r",
        "NTE|1|L|Initial note|EXTRA\r",
    );
    state.paste_from_text(raw, false, &settings);
    state.set_show_empty_fields(true, &settings);

    let msh8: FieldAddress = "MSH-8".parse().unwrap();
    let msh8_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh8))
        .expect("MSH-8 visible under Show Empty")
        .row_index;
    state.select_field_row(msh8_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "SEC_TOKEN".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());

    assert_eq!(
        state.doc.records[0].field_bytes(0, &msh8),
        Some(b"SEC_TOKEN".as_slice())
    );
    assert_eq!(
        state.doc.records[0].field_bytes(0, &"MSH-9".parse().unwrap()),
        Some(b"ADT^A01".as_slice())
    );
    assert_eq!(
        state.doc.records[0].field_bytes(0, &"MSH-10".parse().unwrap()),
        Some(b"CTRL99".as_slice())
    );
    assert_eq!(
        state.doc.records[0].field_bytes(0, &"MSH-12".parse().unwrap()),
        Some(b"2.5.1".as_slice())
    );

    // 3. Editing NTE-3 with literal `|` and PID-5.1 with literal `^` auto-escapes without shifting neighbors
    let nte3: FieldAddress = "NTE-3".parse().unwrap();
    let nte3_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(nte3))
        .unwrap()
        .row_index;
    state.select_field_row(nte3_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "Left | Right".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert_eq!(
        state.doc.records[0].field_bytes(2, &nte3),
        Some(b"Left \\F\\ Right".as_slice())
    );
    assert_eq!(
        state.doc.records[0].field_bytes(2, &"NTE-4".parse().unwrap()),
        Some(b"EXTRA".as_slice()),
        "NTE-4 must not shift after editing NTE-3 with literal '|'"
    );

    let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
    let pid5_1_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(pid5_1))
        .unwrap()
        .row_index;
    state.select_field_row(pid5_1_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "DOE^JR".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid5_1),
        Some(b"DOE\\S\\JR".as_slice())
    );
    assert_eq!(
        state.doc.records[0].field_bytes(1, &"PID-5.2".parse().unwrap()),
        Some(b"JANE".as_slice()),
        "PID-5.2 must not shift after editing PID-5.1 with literal '^'"
    );
}

#[test]
fn multi_chunk_mllp_frame_reassembly_handles_large_base64_obx5_payload() {
    use bytes::BytesMut;
    use hl7_net::{MllpCodec, MllpEnvelope};
    use tokio_util::codec::Decoder;

    let mut codec = MllpCodec::new(MllpEnvelope::DEFAULT, 1024 * 1024);
    // Simulate a large 32 KiB base64-encoded PDF inside OBX-5 split across 137-byte TCP chunks
    let base64_blob = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NTY3ODkw".repeat(750);
    let msg = format!(
        "MSH|^~\\&|LAB|FAC|EHR|FAC|20261123||ORU^R01|BIG1|P|2.5.1\rOBX|1|ED|PDF||^Application^PDF^Base64^{base64_blob}\r"
    );
    let mut framed = Vec::with_capacity(msg.len() + 3);
    framed.push(0x0B);
    framed.extend_from_slice(msg.as_bytes());
    framed.extend_from_slice(&[0x1C, 0x0D]);

    let mut buf = BytesMut::new();
    let mut decoded_frames = Vec::new();
    for chunk in framed.chunks(137) {
        buf.extend_from_slice(chunk);
        while let Some(frame) = codec.decode(&mut buf).unwrap() {
            decoded_frames.push(frame);
        }
    }
    assert_eq!(decoded_frames.len(), 1);
    assert_eq!(decoded_frames[0].payload, msg.as_bytes());
}

#[test]
fn gui_phi_scrub_in_place_undo_and_export_synthetic_are_referentially_consistent() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|EHR|MAIN|LAB|MAIN|20261123080000||ADT^A01|MSG100|P|2.5.1\r",
        "PID|1||MRN998877^^^HOSP^MR||VALDEZ^ELENA^M||19820419|F|||742 EVERGREEN TER^APT 4B^SPRINGFIELD^IL^62704^USA||(312)555-8291|||S||ACC445566|123-45-6789\r",
        "PV1|1|I|ICU^101^A||||||||||||||||VIS778899\r",
        "NTE|1|L|Patient ELENA VALDEZ (MRN998877) admitted for acute asthma\r",
        "MSH|^~\\&|LAB|MAIN|EHR|MAIN|20261123093000||ORU^R01|MSG101|P|2.5.1\r",
        "PID|1||MRN998877^^^HOSP^MR||VALDEZ^ELENA^M||19820419|F|||742 EVERGREEN TER^APT 4B^SPRINGFIELD^IL^62704^USA||(312)555-8291|||S||ACC445566|123-45-6789\r",
        "PV1|1|I|ICU^101^A||||||||||||||||VIS778899\r",
        "OBX|1|TX|IMP||Chest X-ray for VALDEZ, ELENA (visit VIS778899): clear lung fields\r",
    );
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.doc.len(), 2);
    let orig_0 = state.doc.records[0].bytes().to_vec();
    let orig_1 = state.doc.records[1].bytes().to_vec();

    // 1. Export Synthetic to disk must NOT mutate the loaded session
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("synthetic.hl7");
    let (exported, used_seed) = state
        .export_synthetic_to_path(out_path.clone(), Some("gui-seed-42"), &settings)
        .unwrap();
    assert_eq!(exported, 2);
    assert_eq!(used_seed, "gui-seed-42");
    assert_eq!(state.doc.records[0].bytes(), orig_0.as_slice());
    assert_eq!(state.doc.records[1].bytes(), orig_1.as_slice());
    assert!(!state.undo_stack.can_undo());

    let exported_text = std::fs::read_to_string(&out_path).unwrap();
    assert!(!exported_text.contains("VALDEZ"));
    assert!(!exported_text.contains("ELENA"));
    assert!(!exported_text.contains("MRN998877"));
    assert!(!exported_text.contains("VIS778899"));
    assert!(exported_text.contains("acute asthma"));
    assert!(exported_text.contains("clear lung fields"));

    // 2. Scrub in-place with the exact same seed -> matches exported output and is cleanly undoable
    let (modified, seed_in_place) = state
        .scrub_loaded_records_undoable(Some("gui-seed-42"), &settings)
        .unwrap();
    assert_eq!(modified, 2);
    assert_eq!(seed_in_place, "gui-seed-42");
    assert!(state.undo_stack.can_undo());

    let pid3_1: FieldAddress = "PID-3.1".parse().unwrap();
    let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
    let pv1_19: FieldAddress = "PV1-19".parse().unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1),
        state.doc.records[1].field_bytes(1, &pid3_1),
        "ADT and ORU must map to identical synthetic MRN"
    );
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid5_1),
        state.doc.records[1].field_bytes(1, &pid5_1),
        "ADT and ORU must map to identical synthetic family name"
    );
    assert_eq!(
        state.doc.records[0].field_bytes(2, &pv1_19),
        state.doc.records[1].field_bytes(2, &pv1_19),
        "ADT and ORU must map to identical synthetic visit ID"
    );

    // 3. Single Ctrl+Z undo restores both original messages byte-for-byte
    assert!(state.undo(&settings).unwrap().is_some());
    assert_eq!(state.doc.records[0].bytes(), orig_0.as_slice());
    assert_eq!(state.doc.records[1].bytes(), orig_1.as_slice());
}

#[test]
fn gui_segment_accordion_detects_and_extracts_ed_base64_attachment() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    // Base64 for "%PDF-1.4\nSynthetic Report\n%%EOF"
    let pdf_b64 = "JVBERi0xLjQKU3ludGhldGljIFJlcG9ydAolJUVPRg==";
    let raw = format!(
        "MSH|^~\\&|LAB|MAIN|EHR|MAIN|20261123||ORU^R01|MSG0001|P|2.5.1\rOBX|1|ED|PDF||^Application^PDF^Base64^{pdf_b64}\r"
    );
    state.paste_from_text(&raw, false, &settings);
    let cache = state.record_cache.as_ref().unwrap();
    let att_row = cache
        .field_rows
        .iter()
        .find(|r| r.attachment.is_some())
        .expect("expected Base64 PDF attachment row in Segment Accordion");
    let summary = att_row.attachment.as_ref().unwrap();
    assert_eq!(summary.extension, "pdf");
    assert_eq!(summary.label, "PDF");
    assert!(summary.badge_text().starts_with("[Base64 PDF"));

    let row_idx = att_row.row_index;
    let (decoded, suggested) = state.extract_attachment_for_field_row(row_idx).unwrap();
    assert_eq!(suggested, "MSG0001_OBX-5.pdf");
    assert_eq!(decoded.bytes, b"%PDF-1.4\nSynthetic Report\n%%EOF");

    let dir = tempfile::tempdir().unwrap();
    let out_file = dir.path().join(&suggested);
    let written = state
        .save_attachment_for_row_to_path(row_idx, out_file.clone())
        .unwrap();
    assert_eq!(written, decoded.bytes.len());
    assert_eq!(std::fs::read(&out_file).unwrap(), decoded.bytes);
}

#[test]
fn gui_phi_scrub_modals_render_mandatory_best_effort_disclaimer_banner() {
    use pipe_wrench::app::PipeWrenchApp;
    use pipe_wrench::state::{ScrubPhiDialog, ScrubPhiMode};

    fn collect_shape_text(shapes: &[egui::epaint::ClippedShape], out: &mut String) {
        for clipped in shapes {
            collect_inner_shape_text(&clipped.shape, out);
        }
    }

    fn collect_inner_shape_text(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(ts) => {
                out.push_str(ts.galley.text());
                out.push('\n');
            }
            egui::epaint::Shape::Vec(vec) => {
                for s in vec {
                    collect_inner_shape_text(s, out);
                }
            }
            _ => {}
        }
    }

    for mode in [ScrubPhiMode::InPlaceLoaded, ScrubPhiMode::ExportSynthetic] {
        let mut app = PipeWrenchApp::new();
        app.state.scrub_phi_dialog = Some(ScrubPhiDialog::new(mode));
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut rendered_text = String::new();
        for _ in 0..2 {
            let full = ctx.run(raw_input.clone(), |ctx| {
                app.render_frame(ctx);
            });
            rendered_text.clear();
            collect_shape_text(&full.shapes, &mut rendered_text);
        }
        assert!(
            rendered_text.contains("Best-Effort De-Identification Only"),
            "modal {mode:?} must display best-effort warning heading, got:\n{rendered_text}"
        );
        assert!(
            rendered_text.contains("CANNOT be 100% guaranteed"),
            "modal {mode:?} must display non-guarantee warning body, got:\n{rendered_text}"
        );
        let expected_cta = match mode {
            ScrubPhiMode::InPlaceLoaded => "Scrub Loaded Messages",
            ScrubPhiMode::ExportSynthetic => "Export Synthetic...",
        };
        assert!(
            rendered_text.contains(expected_cta),
            "modal {mode:?} must render primary CTA button '{expected_cta}'"
        );
    }
}

#[test]
fn floating_find_replace_widget_collapse_expand_toggles_and_match_navigation() {
    use pipe_wrench::app::PipeWrenchApp;
    use pipe_wrench::state::FindReplaceTab;

    fn collect_shape_text(shapes: &[egui::epaint::ClippedShape], out: &mut String) {
        for clipped in shapes {
            collect_inner(&clipped.shape, out);
        }
    }
    fn collect_inner(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(ts) => {
                out.push_str(ts.galley.text());
                out.push('\n');
            }
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_inner(s, out);
                }
            }
            _ => {}
        }
    }

    let mut app = PipeWrenchApp::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ADT^A01|F1|P|2.5.1\r",
        "PID|1||MRN101^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20261123||ORU^R01|F2|P|2.5.1\r",
        "PID|1||MRN102^^^HOSP^MR||DOE^JOHN||19750505|M\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };

    let render_text = |app: &mut PipeWrenchApp| -> String {
        let mut out = String::new();
        for _ in 0..2 {
            let full = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
            out.clear();
            collect_shape_text(&full.shapes, &mut out);
        }
        out
    };

    // 1. Ctrl+F opens collapsed (Find-only: Aa / ab / .* inline toggles, no Replace All)
    app.state.open_find_panel();
    assert_eq!(app.state.find_replace_tab, FindReplaceTab::Find);
    app.state.search_query = "DOE".to_string();
    let _ = app.state.jump_search(true, &app.settings);

    let find_text = render_text(&mut app);
    assert!(
        find_text.contains("Aa") && find_text.contains("ab") && find_text.contains(".*"),
        "expected inline Aa/ab/.* toggles, got:\n{find_text}"
    );
    assert!(
        find_text.contains("1 of 2"),
        "expected live match counter '1 of 2', got:\n{find_text}"
    );
    assert!(
        !find_text.contains("Replace All"),
        "collapsed Find widget must not show Replace All"
    );

    // 2. Pressing Enter cycles forward (`1 of 2` -> `2 of 2`), Shift+Enter cycles back (`2 of 2` -> `1 of 2`)
    let mut enter_input = raw_input.clone();
    enter_input.events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    let full_after_enter = ctx.run(enter_input, |ctx| app.render_frame(ctx));
    let mut after_enter_text = String::new();
    collect_shape_text(&full_after_enter.shapes, &mut after_enter_text);
    assert_eq!(
        app.state.search_match_cursor,
        Some(1),
        "Enter must advance to match index 1"
    );

    let mut shift_enter_input = raw_input.clone();
    shift_enter_input.modifiers = egui::Modifiers::SHIFT;
    shift_enter_input.events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    let _ = ctx.run(shift_enter_input, |ctx| app.render_frame(ctx));
    assert_eq!(
        app.state.search_match_cursor,
        Some(0),
        "Shift+Enter must jump back to match index 0"
    );

    // 3. Ctrl+H expands Replace row (`Replace All` visible)
    let _ = app.state.jump_search(true, &app.settings);
    app.state.open_replace_panel();
    assert_eq!(app.state.find_replace_tab, FindReplaceTab::Replace);
    let rep_text = render_text(&mut app);
    assert!(
        rep_text.contains("2 of 2"),
        "expected '2 of 2' after Find Next"
    );
    assert!(
        rep_text.contains("Replace ($1, $2)") && rep_text.contains("All"),
        "expected expanded Replace row with hint and scope selector, got:\n{rep_text}"
    );

    // Toggle back to collapsed Find-only state
    app.state.find_replace_tab = FindReplaceTab::Find;
    let collapsed_text = render_text(&mut app);
    assert!(
        !collapsed_text.contains("Replace ($1, $2)"),
        "expected Replace row hidden when collapsed"
    );

    // 4. No-match query displays `No results`
    app.state.search_query = "ZZZZ_NO_MATCH".to_string();
    app.state.last_searched_query.clear();
    let _ = app.state.count_find_matches();
    let none_text = render_text(&mut app);
    assert!(
        none_text.contains("No results"),
        "expected 'No results', got:\n{none_text}"
    );
}

#[test]
fn segment_tree_unique_values_filter_report_and_winmerge_compare_modal() {
    use pipe_wrench::app::PipeWrenchApp;
    use pipe_wrench::state::UniqueValuesSortColumn;

    fn collect_shape_text(shapes: &[egui::epaint::ClippedShape], out: &mut String) {
        for clipped in shapes {
            collect_inner(&clipped.shape, out);
        }
    }
    fn collect_inner(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(ts) => {
                out.push_str(ts.galley.text());
                out.push('\n');
            }
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_inner(s, out);
                }
            }
            _ => {}
        }
    }

    let mut app = PipeWrenchApp::new();
    let raw = concat!(
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123080000||ADT^A01|CMP1|P|2.5.1\r",
        "PID|1||MRN1001^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "PV1|1|I|ICU^101^A\r",
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123090000||ADT^A01|CMP2|P|2.5.1\r",
        "PID|1||MRN1009^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "PV1|1|O|CLINIC^2^B\r",
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123100000||ORU^R01|CMP3|P|2.5.1\r",
        "PID|1||MRN2001^^^HOSP^MR||SMITH^JOHN||19750505|M\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);
    assert_eq!(app.state.doc.len(), 3);

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let render_text = |app: &mut PipeWrenchApp| -> String {
        let mut out = String::new();
        for _ in 0..2 {
            let full = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
            out.clear();
            collect_shape_text(&full.shapes, &mut out);
        }
        out
    };

    // 1. Show Unique Values (PV1-2): 1x "I", 1x "O", 1x (Empty / Missing)
    app.state.open_unique_values_for_path("PV1-2");
    {
        let uv = app.state.unique_values_modal.as_ref().unwrap();
        assert_eq!(uv.path, "PV1-2");
        assert_eq!(uv.total_messages, 3);
        assert_eq!(uv.rows.len(), 3);
        assert!(uv.rows.iter().any(|r| r.is_empty_missing && r.count == 1));
    }
    let uv_tsv = app.state.format_unique_values_table();
    assert!(uv_tsv.contains("(Empty / Missing)"));
    app.state.sort_unique_values(UniqueValuesSortColumn::Value);
    let uv_rendered = render_text(&mut app);
    assert!(uv_rendered.contains("Unique Values"));
    assert!(uv_rendered.contains("Copy Values Table"));
    assert!(uv_rendered.contains("(Empty / Missing)"));
    app.state.unique_values_modal = None;

    // 2. Context-menu Filter (`!=` exclude, `exists` presence) and Add to Report
    app.state
        .exclude_filter_for_path("PV1-2", "I", &app.settings);
    assert_eq!(
        app.state.filtered_record_indices.as_ref().map(Vec::len),
        Some(2)
    );
    app.state.clear_filter(&app.settings);
    app.state.filter_present_for_path("PV1-2", &app.settings);
    assert_eq!(
        app.state.filtered_record_indices.as_ref().map(Vec::len),
        Some(2)
    );
    app.state.clear_filter(&app.settings);
    app.state.add_path_to_report_and_open("PID-3.1");
    assert!(app.state.report_window.open);
    app.state.report_window.open = false;

    // 3. Multi-select 2 messages, open WinMerge-style Compare modal, and step Next/Prev Diff
    app.state
        .select_record_with_modifiers(0, false, false, &app.settings);
    app.state
        .select_record_with_modifiers(1, true, false, &app.settings);
    assert_eq!(app.state.selected_records.len(), 2);
    app.state.open_compare_selected_messages();
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(
            cmp.diff.total_diff_rows >= 2,
            "expected PID-3.1 & PV1 diffs"
        );
        assert_eq!(cmp.active_diff_cursor, Some(0));
    }
    let cmp_text_1 = render_text(&mut app);
    assert!(cmp_text_1.contains("Compare HL7 Messages"));
    assert!(cmp_text_1.contains("Prev Diff") && cmp_text_1.contains("Next Diff"));
    assert!(cmp_text_1.contains("Diff 1 of"));
    assert!(cmp_text_1.contains("PID-3.1"));

    // Step forward with Next Diff -> Diff 2 of N, then Prev Diff -> Diff 1 of N
    app.state.compare_next_diff();
    assert_eq!(
        app.state.compare_modal.as_ref().unwrap().active_diff_cursor,
        Some(1)
    );
    let cmp_text_2 = render_text(&mut app);
    assert!(cmp_text_2.contains("Diff 2 of"));

    app.state.compare_prev_diff();
    assert_eq!(
        app.state.compare_modal.as_ref().unwrap().active_diff_cursor,
        Some(0)
    );

    // 4. Top Compare button & View -> Compare Messages enablement: enabled ONLY for 1..=5 selected
    assert!(!pipe_wrench::ui::record_list::can_compare_selected_records(
        0
    ));
    for count in 1..=5 {
        assert!(
            pipe_wrench::ui::record_list::can_compare_selected_records(count),
            "expected Compare enabled for {count} selected messages"
        );
    }
    assert!(!pipe_wrench::ui::record_list::can_compare_selected_records(
        6
    ));
    // Verify top toolbar places Compare right next to Open / Save and renders cleanly when disabled (0 selected)
    app.state.compare_modal = None;
    app.state.selected_records.clear();
    let toolbar_text = render_text(&mut app);
    let save_pos = toolbar_text.find("Save").expect("Save button in toolbar");
    let compare_pos = toolbar_text
        .find("Compare")
        .expect("Compare button in toolbar");
    let undo_pos = toolbar_text.find("Undo").expect("Undo button in toolbar");
    assert!(
        save_pos < compare_pos && compare_pos < undo_pos,
        "expected top Compare button between Save and Undo"
    );

    // 5. Multi-message Compare (3 selected messages: #1 as Baseline vs #2 and #3) + Make Baseline promotion
    app.state.compare_modal = None;
    app.state
        .select_record_with_modifiers(0, false, false, &app.settings);
    app.state
        .select_record_with_modifiers(1, true, false, &app.settings);
    app.state
        .select_record_with_modifiers(2, true, false, &app.settings);
    assert_eq!(app.state.selected_records.len(), 3);
    app.state.open_compare_selected_messages();
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(cmp.is_multi_target());
        assert_eq!(cmp.targets.len(), 2);
        assert!(cmp.left_raw.contains("MRN1001"));
        assert!(cmp.targets[0].raw.contains("MRN1009"));
        assert!(cmp.targets[1].raw.contains("MRN2001"));
        assert!(cmp.active_total_diff_rows() >= 2);
    }
    let multi_text = render_text(&mut app);
    assert!(multi_text.contains("BASELINE"));
    assert!(multi_text.contains("Make Baseline"));
    assert!(multi_text.contains("PID-3.1"));

    // Promote target #2 (MRN2001) to Baseline and verify Baseline swaps and re-diffs
    app.state.compare_promote_target_to_baseline(1);
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(cmp.left_raw.contains("MRN2001"));
        assert!(cmp.targets[1].raw.contains("MRN1001"));
        assert!(cmp.active_total_diff_rows() >= 2);
    }
    let _ = render_text(&mut app);

    // Removing a target column drops back cleanly to 2-message Compare mode
    app.state.compare_remove_target(1);
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(!cmp.is_multi_target());
        assert_eq!(cmp.targets.len(), 1);
    }
    let _ = render_text(&mut app);

    // Selecting 1 message and opening Compare displays the interactive Baseline Input Zone
    // (paste box + clipboard CTA + loaded-message picker + current message preview) rather than an all-green diff
    app.state.compare_modal = None;
    app.state
        .select_record_with_modifiers(0, false, false, &app.settings);
    assert_eq!(app.state.selected_records.len(), 1);
    app.state.open_compare_selected_messages();
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(cmp.left_raw.is_empty());
        assert!(cmp.editing_baseline);
        assert!(cmp.right_raw.contains("MRN1001"));
    }
    let baseline_prompt_text = render_text(&mut app);
    assert!(
        baseline_prompt_text.contains("Provide Baseline Message"),
        "expected Baseline Input Zone heading, got:\n{baseline_prompt_text}"
    );
    assert!(baseline_prompt_text.contains("Paste from Clipboard"));
    assert!(baseline_prompt_text.contains("Or pick another loaded message..."));
    assert!(baseline_prompt_text.contains("Apply / Compare"));
    assert!(baseline_prompt_text.contains("Current Message (Loaded)"));
    assert!(
        !baseline_prompt_text.contains("Differences Only"),
        "empty-baseline setup must not render the all-green diff toolbar/table yet"
    );

    // Picking another loaded message transitions immediately into the live WinMerge diff view
    // with `Edit Baseline` available in the toolbar
    app.state.compare_select_loaded_baseline(1);
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(!cmp.editing_baseline);
        assert!(cmp.left_raw.contains("MRN1009"));
        assert!(cmp.diff.total_diff_rows >= 2);
    }
    let active_diff_text = render_text(&mut app);
    assert!(active_diff_text.contains("Edit Baseline"));
    assert!(active_diff_text.contains("Paste Baseline"));
    assert!(active_diff_text.contains("PID-3.1"));

    // Clicking Edit Baseline reopens the interactive Baseline Input Zone pre-populated with Left raw text
    app.state.compare_begin_edit_baseline();
    let reedit_text = render_text(&mut app);
    assert!(reedit_text.contains("Provide Baseline Message"));
    assert!(reedit_text.contains("Cancel"));
    // Pasting a new baseline transitions immediately back into the live diff view
    app.state.compare_paste_baseline(
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261123080000||ADT^A01|CMP_BASE|P|2.5.1\rPID|1||MRN9999^^^HOSP^MR||DOE^JANE||19800101|F\r",
    );
    let pasted_diff_text = render_text(&mut app);
    assert!(pasted_diff_text.contains("Edit Baseline"));
    assert!(pasted_diff_text.contains("MRN9999"));
}

#[test]
fn ui_polish_bounded_unique_values_window_and_filter_apply_clear() {
    use pipe_wrench::app::PipeWrenchApp;

    let mut app = PipeWrenchApp::new();
    let mut raw = String::new();
    for i in 1..=250 {
        raw.push_str(&format!(
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124080000||ADT^A01|UV{i:04}|P|2.5.1\rPID|1||MRN{i:04}^^^HOSP^MR||PAT{i:04}^TEST||19800101|F\r"
        ));
    }
    app.state.paste_from_text(&raw, false, &app.settings);
    assert_eq!(app.state.doc.len(), 250);

    // 1. Unique Values modal with 250 distinct values must stay bounded within viewport height
    app.state.open_unique_values_for_path("PID-3.1");
    assert_eq!(
        app.state.unique_values_modal.as_ref().unwrap().rows.len(),
        250
    );

    let screen_h = 720.0_f32;
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, screen_h),
        )),
        ..Default::default()
    };
    for _ in 0..3 {
        let _ = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
    }
    let uv_id = egui::Id::new("unique_values_distribution_modal");
    let uv_rect = ctx
        .memory(|m| m.area_rect(uv_id))
        .expect("Unique Values window area_rect should be recorded");
    assert!(
        uv_rect.height() <= screen_h - 60.0,
        "Unique Values window height ({}) exceeded viewport budget (screen_h={screen_h})",
        uv_rect.height()
    );
    app.state.unique_values_modal = None;

    // 2. Filter bar defaults to `*` (Entire Message), supports full-message filtering + negation,
    //    and resets back to `*` on Clear
    assert_eq!(
        app.state.filter_query,
        pipe_wrench::state::DEFAULT_FILTER_PATH
    );
    app.state.filter_bar_open = true;

    // Filter with default `*` target against a value in PID
    app.state.filter_builder_value = "MRN0042".to_string();
    app.state.apply_filter(&app.settings);
    assert_eq!(
        app.state.filtered_record_indices.as_ref().map(Vec::len),
        Some(1)
    );
    let _ = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));

    // Negation with `*` (`!=`) excludes MRN0042 -> 249 messages match
    app.state.filter_builder_op = pipe_wrench::state::FilterBuilderOp::NotEquals;
    app.state.apply_filter(&app.settings);
    assert_eq!(
        app.state.filtered_record_indices.as_ref().map(Vec::len),
        Some(249)
    );

    // Right-click field filter sets a specific path, then Clear resets back to `*`
    app.state
        .open_filter_for_path("PID-3.1", "MRN0042", &app.settings);
    assert!(app.state.filter_query.contains("PID-3.1"));
    assert_eq!(
        app.state.filtered_record_indices.as_ref().map(Vec::len),
        Some(1)
    );

    app.state.clear_filter(&app.settings);
    assert!(app.state.filtered_record_indices.is_none());
    assert_eq!(
        app.state.filter_query,
        pipe_wrench::state::DEFAULT_FILTER_PATH
    );
    let _ = ctx.run(raw_input, |ctx| app.render_frame(ctx));
}

#[test]
fn memory_guardrail_modal_safe_subset_load_all_cancel_and_status_bar_ram() {
    use hl7_core::{MemoryBudget, RecoveryMode};
    use pipe_wrench::app::PipeWrenchApp;

    fn collect_shape_text(shapes: &[egui::epaint::ClippedShape], out: &mut String) {
        for clipped in shapes {
            collect_inner(&clipped.shape, out);
        }
    }
    fn collect_inner(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(ts) => {
                out.push_str(ts.galley.text());
                out.push('\n');
            }
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_inner(s, out);
                }
            }
            _ => {}
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversized_sim.hl7");
    let mut raw = String::new();
    for i in 1..=20 {
        raw.push_str(&format!(
            "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124080000||ADT^A01|MEM{i:04}|P|2.5.1\rPID|1||MRN{i:04}^^^HOSP^MR||PAT{i:04}^TEST||19800101|F\r"
        ));
    }
    std::fs::write(&path, raw.as_bytes()).unwrap();
    let file_bytes = raw.len() as u64;

    // Simulate a tight budget that allows ~5 messages (25% of total heap footprint)
    let tight_budget = MemoryBudget {
        available_ram_bytes: file_bytes * 6,
        safe_load_budget_bytes: (file_bytes * 6) / 4,
        process_rss_bytes: 64 * 1024 * 1024,
        ast_expansion_factor: 6.0,
    };
    assert!(tight_budget.exceeds_budget(file_bytes));
    let expected_subset = tight_budget.estimate_safe_message_limit(file_bytes, 20);
    assert!((1..20).contains(&expected_subset));

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };

    let wait_for_load = |app: &mut PipeWrenchApp| {
        for _ in 0..200 {
            app.state.poll_background_load(&app.settings);
            if app.state.active_load.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.state.active_load.is_none(), "background load timed out");
    };

    // 1. Trigger Guardrail modal and verify Cancel leaves workspace untouched
    let mut app = PipeWrenchApp::new();
    app.state.request_open_files_with_guardrail(
        vec![path.clone()],
        RecoveryMode::Strict,
        None,
        false,
        &app.settings,
        ctx.clone(),
        Some(tight_budget),
    );
    assert!(app.state.memory_guardrail_dialog.is_some());
    assert!(app.state.doc.is_empty());

    let mut rendered = String::new();
    for _ in 0..2 {
        let full = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
        rendered.clear();
        collect_shape_text(&full.shapes, &mut rendered);
    }
    assert!(rendered.contains("Large File Memory Guardrail"));
    assert!(rendered.contains(&format!(
        "Load Safe Subset (First {expected_subset} Messages)"
    )));
    assert!(rendered.contains("Load All Anyway"));
    assert!(rendered.contains("RAM:"));

    app.state.cancel_memory_guardrail();
    assert!(app.state.memory_guardrail_dialog.is_none());
    assert!(app.state.doc.is_empty());

    // 2. Trigger Guardrail modal and choose `Load Safe Subset (First N Messages)`
    app.state.request_open_files_with_guardrail(
        vec![path.clone()],
        RecoveryMode::Strict,
        None,
        false,
        &app.settings,
        ctx.clone(),
        Some(tight_budget),
    );
    assert!(app.state.memory_guardrail_dialog.is_some());
    app.state
        .confirm_memory_guardrail_safe_subset(&app.settings, ctx.clone());
    wait_for_load(&mut app);
    assert_eq!(app.state.doc.len(), expected_subset);
    assert!(
        app.state.status_message.contains(&format!(
            "Loaded first {expected_subset} of 20 messages to protect system RAM"
        )),
        "unexpected status message: {}",
        app.state.status_message
    );

    // 3. Trigger Guardrail modal and choose `Load All Anyway` -> loads all 20 messages
    app.state.request_open_files_with_guardrail(
        vec![path],
        RecoveryMode::Strict,
        None,
        false,
        &app.settings,
        ctx.clone(),
        Some(tight_budget),
    );
    assert!(app.state.memory_guardrail_dialog.is_some());
    app.state
        .confirm_memory_guardrail_load_all(&app.settings, ctx.clone());
    wait_for_load(&mut app);
    assert_eq!(app.state.doc.len(), 20);
}

#[test]
fn scrub_phi_custom_paths_pill_grid_remove_and_execute_scrub() {
    use hl7_core::{CustomPhiMode, CustomPhiRule};
    use pipe_wrench::app::PipeWrenchApp;
    use pipe_wrench::state::{ScrubPhiDialog, ScrubPhiMode};

    fn collect_shape_text(shapes: &[egui::epaint::ClippedShape], out: &mut String) {
        for clipped in shapes {
            collect_inner(&clipped.shape, out);
        }
    }
    fn collect_inner(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(ts) => {
                out.push_str(ts.galley.text());
                out.push('\n');
            }
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_inner(s, out);
                }
            }
            _ => {}
        }
    }

    let mut app = PipeWrenchApp::new();
    app.settings.clear_custom_phi_rules();
    let raw = concat!(
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124090000||ORU^R01|ZTEST1|P|2.5.1\r",
        "PID|1||MRN7711^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "OBX|1|TX|NOTE||Sensitive free text note\r",
        "ZPI|1|KEEP_CODE|SITE_TOKEN_8899\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);

    // Z* quick-pick detects ZPI fields from the selected message
    let z_paths = app.state.detected_custom_z_paths_in_selected();
    assert!(z_paths.contains(&"ZPI-3".to_string()));

    // Add two custom PHI path pills (ZPI-3 Redact and OBX-5 Mask)
    app.settings
        .add_custom_phi_rule(CustomPhiRule {
            path: "ZPI-3".to_string(),
            mode: CustomPhiMode::Redact,
        })
        .unwrap();
    app.settings
        .add_custom_phi_rule(CustomPhiRule {
            path: "OBX-5".to_string(),
            mode: CustomPhiMode::Mask,
        })
        .unwrap();
    assert_eq!(app.settings.custom_phi_rules.len(), 2);

    // Open Scrub PHI modal and verify pills, mode tags, live count, and Clear Custom render
    app.state.scrub_phi_dialog = Some(ScrubPhiDialog::new(ScrubPhiMode::InPlaceLoaded));
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let mut rendered = String::new();
    for _ in 0..2 {
        let full = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
        rendered.clear();
        collect_shape_text(&full.shapes, &mut rendered);
    }
    assert!(rendered.contains("Additional Custom PHI Fields"));
    assert!(rendered.contains("ZPI-3"));
    assert!(rendered.contains("RED"));
    assert!(rendered.contains("OBX-5"));
    assert!(rendered.contains("MSK"));
    assert!(rendered.contains("2 matching value(s)"));
    assert!(rendered.contains("Clear Custom"));

    // Remove OBX-5 pill and verify only ZPI-3 remains
    app.settings.remove_custom_phi_rule(1);
    assert_eq!(app.settings.custom_phi_rules.len(), 1);
    assert_eq!(app.settings.custom_phi_rules[0].path, "ZPI-3");

    // Execute Scrub PHI and verify ZPI-3 is redacted while ZPI-2 is preserved
    let (modified, _) = app
        .state
        .scrub_loaded_records_undoable(Some("ui-test-seed"), &app.settings)
        .unwrap();
    assert_eq!(modified, 1);
    let scrubbed_text = String::from_utf8_lossy(app.state.doc.records[0].bytes());
    assert!(!scrubbed_text.contains("SITE_TOKEN_8899"));
    assert!(scrubbed_text.contains("ZPI|1|KEEP_CODE|[REDACTED]"));
}

#[test]
fn cross_feature_hardening_scrub_undo_compare_stale_delete_invalid_regex_and_alt_delimiters() {
    use hl7_core::{CustomPhiMode, CustomPhiRule, DiffOptions, MemoryBudget};
    use pipe_wrench::app::PipeWrenchApp;

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };

    // 1. Cross-feature workflow: custom PHI pill + Scrub PHI + Compare before/after + Undo restores exact bytes
    let mut app = PipeWrenchApp::new();
    app.settings.clear_custom_phi_rules();
    app.settings.prompt_before_deleting = false;
    let raw = concat!(
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124080000||ORU^R01|H1|P|2.5.1\r",
        "PID|1||MRN101^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "ZPI|1|KEEP|SECRET_SITE_ID_42\r",
        "MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261124090000||ORU^R01|H2|P|2.5.1\r",
        "PID|1||MRN202^^^HOSP^MR||SMITH^JOHN||19750505|M\r",
        "ZPI|1|KEEP|SECRET_SITE_ID_99\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);
    let orig_msg0 = String::from_utf8_lossy(app.state.doc.records[0].bytes()).into_owned();

    app.settings
        .add_custom_phi_rule(CustomPhiRule {
            path: "ZPI-3".to_string(),
            mode: CustomPhiMode::Redact,
        })
        .unwrap();
    let (modified, _) = app
        .state
        .scrub_loaded_records_undoable(Some("hardening-seed"), &app.settings)
        .unwrap();
    assert_eq!(modified, 2);
    let scrubbed_msg0 = String::from_utf8_lossy(app.state.doc.records[0].bytes()).into_owned();
    assert!(!scrubbed_msg0.contains("SECRET_SITE_ID_42"));
    assert!(scrubbed_msg0.contains("[REDACTED]"));

    // Diff before vs after shows custom ZPI-3 and PID-3.1 changes cleanly
    let diff = hl7_core::diff_hl7_messages(&orig_msg0, &scrubbed_msg0, DiffOptions::default());
    assert!(diff.total_diff_rows >= 2);

    // Undo restores exact original bytes
    let _ = app.state.undo(&app.settings);
    assert_eq!(
        String::from_utf8_lossy(app.state.doc.records[0].bytes()),
        orig_msg0
    );

    // 2. Deleting ALL loaded messages while Compare and Unique Values are open never panics
    app.state.open_unique_values_for_path("PID-3.1");
    app.state
        .select_record_with_modifiers(0, false, false, &app.settings);
    app.state
        .select_record_with_modifiers(1, true, false, &app.settings);
    app.state.open_compare_selected_messages();
    assert!(app.state.compare_modal.is_some());

    // Delete all messages from the main workspace while Compare + Unique Values remain open
    app.state
        .request_delete_records(vec![0, 1], &app.settings)
        .unwrap();
    assert!(app.state.doc.is_empty());
    // Stale baseline picker call + 0-diff navigation + rendering must not panic
    app.state.compare_select_loaded_baseline(99);
    app.state.compare_next_diff();
    app.state.compare_prev_diff();
    for _ in 0..2 {
        let _ = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
    }
    app.state.compare_modal = None;
    app.state.unique_values_modal = None;

    // 3. Invalid regex in Filter and Find/Replace produces inline error without panicking
    app.state.paste_from_text(raw, false, &app.settings);
    app.state.filter_bar_open = true;
    app.state.filter_query = "*".to_string();
    app.state.filter_builder_op = pipe_wrench::state::FilterBuilderOp::Regex;
    app.state.filter_builder_value = "[unclosed(".to_string();
    app.state.apply_filter(&app.settings);
    assert!(app.state.filter_error.is_some());

    app.state.open_find_panel();
    app.state.search_use_regex = true;
    app.state.search_query = "[unclosed(".to_string();
    assert!(app.state.count_find_matches().is_err());
    let _ = ctx.run(raw_input, |ctx| app.render_frame(ctx));

    // 4. Non-standard MSH-1/MSH-2 delimiters (`*` and `@~\&`) across Compare & Custom PHI scrubbing
    let alt1 = "MSH*@~\\&*EHR*HOSP*LAB*HOSP*20261124080000**ORU@R01*ALT1*P*2.5.1\rPID*1**MRN8801@@@HOSP@MR**DOE@JANE**19800101*F\rZPI*1*KEEP*ALT_SECRET_77\r";
    let alt2 = "MSH*@~\\&*EHR*HOSP*LAB*HOSP*20261124090000**ORU@R01*ALT2*P*2.5.1\rPID*1**MRN8802@@@HOSP@MR**DOE@JANE**19800101*F\rZPI*1*KEEP*ALT_SECRET_77\r";
    let alt_diff = hl7_core::diff_hl7_messages(alt1, alt2, DiffOptions::default());
    assert_eq!(alt_diff.total_diff_rows, 1);
    assert!(alt_diff
        .blocks
        .iter()
        .flat_map(|b| &b.rows)
        .any(|r| r.path == "PID-3.1" && r.kind == hl7_core::DiffChangeKind::Modified));
    // Identical messages -> 0 diffs
    let zero_diff = hl7_core::diff_hl7_messages(alt1, alt1, DiffOptions::default());
    assert_eq!(zero_diff.total_diff_rows, 0);

    let alt_recs = hl7_core::load_from_bytes(alt1.as_bytes(), hl7_core::ScannerConfig::default())
        .unwrap()
        .records;
    let alt_scrubbed = hl7_core::PhiScrubber::new("alt-seed")
        .with_extra_paths(vec![CustomPhiRule {
            path: "ZPI-3".to_string(),
            mode: CustomPhiMode::Redact,
        }])
        .scrub_records(&alt_recs);
    let alt_out = String::from_utf8_lossy(alt_scrubbed[0].bytes());
    assert!(!alt_out.contains("ALT_SECRET_77"));
    assert!(alt_out.contains("ZPI*1*KEEP*[REDACTED]"));

    // 5. 0-byte file handling in MemoryBudget & estimate_file_set_metrics
    let dir = tempfile::tempdir().unwrap();
    let empty_file = dir.path().join("empty.hl7");
    std::fs::write(&empty_file, b"").unwrap();
    let (bytes, count, exact) =
        hl7_core::estimate_file_set_metrics(&[empty_file], hl7_core::ScannerConfig::default());
    assert_eq!((bytes, count, exact), (0, 0, true));
    let budget = MemoryBudget::from_available_bytes(8 * 1024 * 1024 * 1024);
    assert!(!budget.exceeds_budget(0));
    assert_eq!(budget.estimate_safe_message_limit(0, 0), 0);
}

#[test]
fn multi_column_compare_five_messages_with_long_values_wraps_and_renders_cleanly() {
    use pipe_wrench::app::PipeWrenchApp;

    let mut app = PipeWrenchApp::new();
    let long_mrn = "MRN_EXTENDED_ASSIGNING_AUTHORITY_IDENTIFIER_0000000000000000001";
    let long_note = "UNBROKEN_CLINICAL_OBSERVATION_STRING_WITH_ZERO_SPACES_TO_VERIFY_WRAP_AND_CELL_CLIPPING_INSIDE_MULTI_COLUMN_COMPARE_TABLE_0123456789";
    let mut raw = String::new();
    for i in 1..=5 {
        raw.push_str(&format!(
            "MSH|^~\\&|EHR_LONG_APP_{i}|FACILITY_MAIN_CAMPUS|LAB_SYS|LAB_FAC|20261124080{i}00||ORU^R01|LONG_CTRL_ID_{i:04}|P|2.5.1\r\
             PID|1||{long_mrn}_{i}^^^SYNTH_HOSP_LONG_AUTHORITY^MR||DOE^JANE||19800101|F\r\
             OBX|1|TX|NOTE||{long_note}_VARIANT_{i}\r"
        ));
    }
    app.state.paste_from_text(&raw, false, &app.settings);
    assert_eq!(app.state.doc.len(), 5);

    // Select all 5 messages and open multi-column Compare
    app.state
        .select_record_with_modifiers(0, false, false, &app.settings);
    for i in 1..5 {
        app.state
            .select_record_with_modifiers(i, true, false, &app.settings);
    }
    assert_eq!(app.state.selected_records.len(), 5);
    app.state.open_compare_selected_messages();
    {
        let cmp = app.state.compare_modal.as_ref().unwrap();
        assert!(cmp.is_multi_target());
        assert_eq!(cmp.targets.len(), 4);
        assert!(cmp.active_total_diff_rows() >= 2);
    }

    // Render at both normal and narrow viewport widths to verify horizontal scrolling + wrapping without panicking
    let ctx = egui::Context::default();
    for width in [1280.0_f32, 760.0_f32] {
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(width, 720.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = ctx.run(raw_input.clone(), |ctx| app.render_frame(ctx));
        }
    }
}
