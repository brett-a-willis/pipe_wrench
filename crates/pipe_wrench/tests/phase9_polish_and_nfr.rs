//! Phase 9 tests covering canonical HL7 v2 path notation (`SEGMENT-FIELD(.COMPONENT)(.SUBCOMPONENT)`),
//! Preferences file filter management (`FO-1`, `FO-2`), zero-record external editor fallback state
//! (`FO-12`), and NFR-1 large-file loading & selection responsiveness.

use std::io::BufReader;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use hl7_core::{
    load_from_bytes, load_records_from_reader, FieldAddress, FieldDisplayOptions, RecoveryMode,
    ScannerConfig,
};
use pipe_wrench::{AppSettings, DataSource, SessionState};

// §0, §5.2, §12.2, D-8: Canonical HL7 v2 path notation and default first-repetition resolution
#[test]
fn canonical_hl7_path_notation_examples_and_default_repetition() {
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01^ADT_A01|MSG100|P|2.5.1\r",
        "PID|1||ID1^^^AUTH1&OID1&ISO~ID2^^^AUTH2&OID2&ISO||DOE^JOHN~ROE^JANE\r",
        "OBX|1|ST|CODE||VAL1~VAL2~VAL3\r",
    );
    let out = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
    let rec = &out.records[0];

    // MSH-9.1 -> "ADT"
    let msh9_1: FieldAddress = "MSH-9.1".parse().unwrap();
    assert_eq!(msh9_1.to_string(), "MSH-9.1");
    assert_eq!(rec.field_bytes(0, &msh9_1), Some(&b"ADT"[..]));

    // PID-3.1 (no repetition specified -> assumes 1st repetition "ID1", D-8)
    let pid3_1: FieldAddress = "PID-3.1".parse().unwrap();
    assert_eq!(pid3_1.to_string(), "PID-3.1");
    assert_eq!(rec.field_bytes(1, &pid3_1), Some(&b"ID1"[..]));

    // PID-3.4.1 (no repetition -> 1st repetition, component 4, subcomponent 1 -> "AUTH1")
    let pid3_4_1: FieldAddress = "PID-3.4.1".parse().unwrap();
    assert_eq!(pid3_4_1.to_string(), "PID-3.4.1");
    assert_eq!(rec.field_bytes(1, &pid3_4_1), Some(&b"AUTH1"[..]));

    // PID-3(1) and PID-3(2).1 and PID-3(2).4.1
    let pid3_rep1: FieldAddress = "PID-3(1)".parse().unwrap();
    assert_eq!(pid3_rep1.to_string(), "PID-3(1)");
    assert_eq!(
        rec.field_bytes(1, &pid3_rep1),
        Some(&b"ID1^^^AUTH1&OID1&ISO"[..])
    );

    let pid3_rep2_c1: FieldAddress = "PID-3(2).1".parse().unwrap();
    assert_eq!(pid3_rep2_c1.to_string(), "PID-3(2).1");
    assert_eq!(rec.field_bytes(1, &pid3_rep2_c1), Some(&b"ID2"[..]));

    let pid3_rep2_c4_s1: FieldAddress = "PID-3(2).4.1".parse().unwrap();
    assert_eq!(pid3_rep2_c4_s1.to_string(), "PID-3(2).4.1");
    assert_eq!(rec.field_bytes(1, &pid3_rep2_c4_s1), Some(&b"AUTH2"[..]));

    // OBX-5 and OBX-5(3)
    let obx5: FieldAddress = "OBX-5".parse().unwrap();
    assert_eq!(obx5.to_string(), "OBX-5");
    assert_eq!(rec.field_bytes(2, &obx5), Some(&b"VAL1~VAL2~VAL3"[..]));

    let obx5_3: FieldAddress = "OBX-5(3)".parse().unwrap();
    assert_eq!(obx5_3.to_string(), "OBX-5(3)");
    assert_eq!(rec.field_bytes(2, &obx5_3), Some(&b"VAL3"[..]));

    // Verify field_entries generates canonical dot notation
    let entries = rec.field_entries(1, FieldDisplayOptions::default());
    let formatted: Vec<String> = entries
        .iter()
        .filter_map(|e| e.address.map(|a| a.to_string()))
        .collect();
    assert!(formatted.contains(&"PID-3(1).1".to_string()));
    assert!(formatted.contains(&"PID-3(1).4.1".to_string()));
    assert!(formatted.contains(&"PID-3(2).1".to_string()));
    assert!(formatted.contains(&"PID-3(2).4.1".to_string()));
    assert!(formatted.contains(&"PID-5(1).1".to_string()));
    assert!(formatted.contains(&"PID-5(2).1".to_string()));
}

// FO-1, FO-2, G-6: AppSettings filter pattern list management
#[test]
fn app_settings_filter_pattern_management_unlimited_and_rejects_duplicates() {
    let mut settings = AppSettings::default();
    assert_eq!(
        settings.file_filter_patterns,
        vec!["*.*", "*.dat", "*.txt", "*.hl7"]
    );
    assert!(settings.add_filter_pattern("*.HL7").is_err());
    assert!(settings.add_filter_pattern("   ").is_err());

    for i in 0..12 {
        settings.add_filter_pattern(format!("*.custom{i}")).unwrap();
    }
    assert_eq!(settings.file_filter_patterns.len(), 16);
    assert!(settings.remove_filter_pattern(0));
    assert_eq!(settings.file_filter_patterns.len(), 15);
}

// NFR-1: Load 50,000 messages in under 3 seconds and update selection panes in under 50 ms
#[test]
fn nfr1_load_50k_messages_under_3s_and_select_under_50ms() {
    let msg = b"MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG00001|P|2.5.1\rPID|1||PAT0001^^^HOSP^MR||TESTPATIENT^ALPHA||19800101|U\rPV1|1|I|WARD1^101^A\r";
    let mut buf = Vec::with_capacity(msg.len() * 50_000);
    for _ in 0..50_000 {
        buf.extend_from_slice(msg);
    }

    let cancel = AtomicBool::new(false);
    let config = ScannerConfig {
        recovery_mode: RecoveryMode::Strict,
        ..ScannerConfig::default()
    };

    let load_start = Instant::now();
    let out = load_records_from_reader(BufReader::new(buf.as_slice()), config, &cancel).unwrap();
    let load_elapsed = load_start.elapsed();
    assert_eq!(out.records.len(), 50_000);
    assert!(
        load_elapsed.as_secs_f64() < 3.0,
        "loading 50,000 messages took {:.2}s (NFR-1 target < 3.0s)",
        load_elapsed.as_secs_f64()
    );

    let settings = AppSettings::default();
    let mut state = SessionState::new();
    state.set_records(out.records, DataSource::Clipboard, &settings);

    let sel_start = Instant::now();
    state.select_record(25_000, false, &settings);
    let sel_elapsed = sel_start.elapsed();
    assert!(
        sel_elapsed.as_millis() < 50,
        "selecting record 25,000 took {} ms (NFR-1 target < 50 ms)",
        sel_elapsed.as_millis()
    );
}

// SCAN-13, SCAN-15: GUI DirectoryScanWindowState sorting and selected file handoff
#[test]
fn gui_directory_scan_window_sorting_and_open_selected_handoff() {
    use pipe_wrench::ui::scan_window::ScanSortColumn;
    use std::path::Path;

    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let scan_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/scan");

    state.scan_window.dir_input = scan_dir.display().to_string();
    let cfg = state.scan_window.build_config(&settings).unwrap();
    let cancel = AtomicBool::new(false);
    let rep = hl7_core::scan_directory(&cfg, &cancel, |_| {}).unwrap();
    state.scan_window.report = Some(rep);

    // Select first two files (`a.txt` and `arch.zip!e.txt`) and verify filesystem path extraction (SCAN-15)
    state.scan_window.selected_rows.insert(0);
    state.scan_window.selected_rows.insert(1);
    let fs_paths = state.scan_window.selected_filesystem_paths();
    assert_eq!(fs_paths.len(), 2);

    // Toggle sort by MSH count (SCAN-13, SORT-3)
    state.scan_window.sort_by_column(ScanSortColumn::Msh);
    state.scan_window.sort_by_column(ScanSortColumn::Msh); // Descending
    let top_msh = state.scan_window.report.as_ref().unwrap().files[0].msh_count;
    assert_eq!(top_msh, 4); // sub/c.hl7 has 4 MSH messages
}

// D-26: Post-Phase 10 UI & feature refinements:
// - Unified detail view without synthetic marker rows
// - Multi-record clipboard copy
// - Non-destructive Record List Filter (plain text & field path)
// - Jump-to-Next Search (F3 / Shift+F3) with wrap & segment accordion expansion
// - Dark editor dynamic delimiter syntax highlighting spans
// - In-place raw editor commit onto UndoStack
// - Docked TCP/IP panel toggling
#[test]
fn post_phase10_ui_refinements_filter_search_copy_syntax_and_dock() {
    use pipe_wrench::ui::detail_pane::{tokenize_hl7_syntax, SyntaxSpanKind};
    use pipe_wrench::ui::net_panels::TcpIpDockTab;

    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|SND1|FAC1|RCV1|FAC1|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||12345^^^HOSP&OID&ISO||DOE^JOHN~ALIAS^JOHNNY\r",
        "OBX|1|NM|GLU||105|mg/dL\r",
        "MSH|^~\\&|SND2|FAC2|RCV2|FAC2|20260115120001||ORU^R01|MSG2|P|2.5.1\r",
        "PID|1||67890^^^HOSP&OID&ISO||SMITH^ALICE\r",
        "OBX|1|TX|NOTE||Critical value\\E\\alert\r",
    );
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.doc.len(), 2);

    // 1. Verify no synthetic marker rows exist in field_rows
    let cache = state.record_cache.as_ref().unwrap();
    assert!(cache.field_rows.iter().all(|r| !r.is_parsed_marker));

    // 2. Multi-record clipboard copy (Shift+Click range 0..=1 -> formats both records)
    state.select_record_with_modifiers(0, false, false, &settings);
    state.select_record_with_modifiers(1, false, true, &settings);
    assert_eq!(state.selected_records.len(), 2);
    let copied = state.format_selected_records_for_clipboard(&settings);
    assert!(copied.contains("MSG1"));
    assert!(copied.contains("MSG2"));
    assert!(copied.contains("DOE^JOHN"));
    assert!(copied.contains("SMITH^ALICE"));

    // 3. Non-destructive Record List Filter (field path & plain text)
    state.filter_query = "MSH-9.1=ORU".to_string();
    state.apply_filter(&settings);
    assert_eq!(state.filtered_record_indices, Some(vec![1]));
    assert_eq!(state.doc.len(), 2); // underlying Document untouched
    assert!(!state.undo_stack.is_dirty());

    state.filter_query = "PID-5.1 contains DOE".to_string();
    state.apply_filter(&settings);
    assert_eq!(state.filtered_record_indices, Some(vec![0]));

    state.clear_filter(&settings);
    assert!(state.filtered_record_indices.is_none());

    // 4. Jump-to-Next Search (F3 / Shift+F3) across (record, segment, field) with wrap
    state.collapse_all_segments();
    state.search_query = "Critical".to_string();
    let hit1 = state.jump_search(true, &settings).expect("match found");
    assert_eq!(hit1.record_index, 1);
    assert_eq!(hit1.segment_index, 2);
    assert_eq!(hit1.field_address, Some("OBX-5".parse().unwrap()));
    assert_eq!(state.selected_record, Some(1));
    assert!(state.expanded_segments.contains(&2));

    // Next again wraps around
    let hit_wrap = state.jump_search(true, &settings).expect("wrapped match");
    assert_eq!(hit_wrap, hit1);
    assert!(state.search_status.contains("wrapped"));

    // 5. Dark editor syntax highlighting spans for segment tags and all 5 active delimiters
    let sample_text = "MSH|^~\\&|A^B~C\\E\\D&E\nPID|1";
    let spans = tokenize_hl7_syntax(sample_text, hl7_core::Delimiters::STANDARD, Some(0));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::SegmentTag && &sample_text[s.range.clone()] == "MSH"));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::FieldDelimiter));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::ComponentDelimiter));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::RepetitionDelimiter));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::EscapeDelimiter));
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::SubcomponentDelimiter));
    assert!(spans.iter().any(|s| s.on_active_line));

    // 6. In-place raw editor commits via EditRecordCmd onto UndoStack
    state.select_record(0, false, &settings);
    state.inline_raw_text.push_str("\nNTE|1||Inline raw note");
    state.commit_inline_raw_edit(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 4);
    assert!(state.undo_stack.is_dirty());
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 3);

    // 7. Docked TCP/IP panel toggling
    assert!(!state.network.docked_open);
    state.network.toggle_dock();
    assert!(state.network.docked_open);
    state.network.open_dock_tab(TcpIpDockTab::ServerListener);
    assert_eq!(state.network.active_tab, TcpIpDockTab::ServerListener);
    state.network.toggle_dock();
    assert!(!state.network.docked_open);
}

// D-26: Dual Dark/Light theme design system, pill badges, record navigation bar (|<, <, >, >|),
// clickable Rec # / Total jump-to-record control, and upgraded GUI Search (field restriction,
// regex, match counter, and field hit breakdown).
#[test]
fn dual_dark_light_theme_record_navigation_jump_and_upgraded_search() {
    use pipe_wrench::ui::detail_pane::SyntaxSpanKind;
    use pipe_wrench::ui::theme::{
        ack_code_pill_colors, build_visuals, message_type_pill_colors, segment_tag_pill_colors,
        EditorPalette, ThemePalette,
    };

    // 1. Dual Dark & Light theme palettes and EditorPalette (Dark vs Light)
    let dark_vis = build_visuals(true);
    let light_vis = build_visuals(false);
    let dark_theme = ThemePalette::for_visuals(&dark_vis);
    let light_theme = ThemePalette::for_visuals(&light_vis);
    assert!(dark_theme.is_dark);
    assert!(!light_theme.is_dark);
    assert_eq!(
        dark_theme.base_canvas,
        egui::Color32::from_rgb(0x14, 0x15, 0x19)
    );
    assert_eq!(
        light_theme.base_canvas,
        egui::Color32::from_rgb(0xF6, 0xF8, 0xFA)
    );
    assert_ne!(dark_theme.accent, light_theme.accent);

    let dark_ed = EditorPalette::for_visuals(&dark_vis);
    let light_ed = EditorPalette::for_visuals(&light_vis);
    assert_eq!(
        dark_ed.background,
        egui::Color32::from_rgb(0x1E, 0x1E, 0x1E)
    );
    assert_eq!(
        light_ed.background,
        egui::Color32::from_rgb(0xFF, 0xFF, 0xFF)
    );
    assert_ne!(dark_ed.text_fg, light_ed.text_fg);
    assert_ne!(
        SyntaxSpanKind::SegmentTag.color_in(&dark_ed),
        SyntaxSpanKind::SegmentTag.color_in(&light_ed)
    );
    assert_ne!(
        SyntaxSpanKind::FieldDelimiter.color_in(&dark_ed),
        SyntaxSpanKind::FieldDelimiter.color_in(&light_ed)
    );

    // 2. Pill badge helpers return distinct dark vs light colors
    let adt_dark = message_type_pill_colors("ADT^A01", true);
    let adt_light = message_type_pill_colors("ADT^A01", false);
    assert_ne!(adt_dark.bg, adt_light.bg);
    assert_ne!(adt_dark.fg, adt_light.fg);
    assert_ne!(
        segment_tag_pill_colors("OBX", true).bg,
        segment_tag_pill_colors("OBX", false).bg
    );
    assert_ne!(
        ack_code_pill_colors("AA", true).bg,
        ack_code_pill_colors("AR", true).bg
    );

    // 3. Record navigation (|<, <, >, >|) and clickable Rec # / Total jump control
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|S|F|R|F|20260115120000||ADT^A01|M1|P|2.5.1\rPID|1||P1||ALPHA^ONE\rOBX|1|NM|GLU||95\r",
        "MSH|^~\\&|S|F|R|F|20260115120001||ORU^R01|M2|P|2.5.1\rPID|1||P2||BETA^TWO\rOBX|1|NM|GLU||210\r",
        "MSH|^~\\&|S|F|R|F|20260115120002||ADT^A08|M3|P|2.5.1\rPID|1||P3||GAMMA^THREE\rOBX|1|NM|GLU||250\rNTE|1||GLU high\r",
    );
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.format_record_position_label(), "Msg 1 / 3");

    state.navigate_next_record(&settings);
    assert_eq!(state.selected_record, Some(1));
    assert_eq!(state.format_record_position_label(), "Msg 2 / 3");

    state.navigate_last_record(&settings);
    assert_eq!(state.selected_record, Some(2));
    assert_eq!(state.format_record_position_label(), "Msg 3 / 3");

    state.navigate_prev_record(&settings);
    assert_eq!(state.selected_record, Some(1));

    state.navigate_first_record(&settings);
    assert_eq!(state.selected_record, Some(0));

    // Inline Rec # / Total numeric jump input
    state.begin_record_jump();
    assert!(state.record_jump_active);
    assert_eq!(state.record_jump_input, "1");
    state.record_jump_input = "3".to_string();
    state.commit_record_jump_input(&settings);
    assert!(!state.record_jump_active);
    assert_eq!(state.selected_record, Some(2));

    // Navigation & Jump when a Record List Filter is active (OBX-5>200 matches records #2 and #3)
    state.filter_query = "OBX-5>200".to_string();
    state.apply_filter(&settings);
    assert_eq!(state.filtered_record_indices, Some(vec![1, 2]));
    state.navigate_first_record(&settings);
    assert_eq!(state.selected_record, Some(1));
    assert_eq!(state.format_record_position_label(), "Msg 1 / 2 (3 total)");
    state.jump_to_visible_record_number(2, &settings);
    assert_eq!(state.selected_record, Some(2));
    state.clear_filter(&settings);

    // 4. Upgraded GUI Search: regex, field restriction, match counter, and field hit breakdown
    state.search_query = "GLU".to_string();
    state.search_use_regex = false;
    let first_hit = state.jump_search(true, &settings).unwrap();
    assert_eq!(first_hit.record_index, 0);
    assert_eq!(state.search_matches.len(), 4); // 3 in OBX-3 + 1 in NTE-3
    assert_eq!(state.search_status, "Match 1 of 4");
    assert_eq!(state.search_field_breakdown.len(), 2);
    assert_eq!(state.search_field_breakdown[0].field_path, "OBX-3");
    assert_eq!(state.search_field_breakdown[0].count, 3);
    assert_eq!(state.search_field_breakdown[1].field_path, "NTE-3");
    assert_eq!(state.search_field_breakdown[1].count, 1);

    // Clicking "NTE-3" in Hit Breakdown restricts search to NTE-3 and jumps to record #3
    let restricted_hit = state
        .restrict_search_to_field_path("NTE-3", &settings)
        .unwrap();
    assert_eq!(restricted_hit.record_index, 2);
    assert_eq!(state.search_matches.len(), 1);
    assert_eq!(state.search_status, "Match 1 of 1");
}
