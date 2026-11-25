//! Phase 17 integration tests covering granular Raw-Editor cursor to Segment Accordion
//! synchronization (`SEG-F[(R)][.C[.S]]`) across components, repetitions, subcomponents,
//! and custom `MSH-2` delimiters (`D-30`).

use hl7_core::FieldAddress;
use pipe_wrench::ui::detail_pane::resolve_raw_cursor_field_path;
use pipe_wrench::{AppSettings, SessionState};

#[test]
fn cursor_sync_resolves_component_repetition_subcomponent_and_custom_delimiters() {
    let settings = AppSettings::default();

    // 1. Standard delimiters: component (PID-3.4), repetition (PID-3(2).1), subcomponent (OBX-5.1.2)
    let raw_std = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ORU^R01|P17_01|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JOHN\r",
        "OBX|1|CE|CODE||A&B^DESC\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw_std, false, &settings);
    // Start with all segments collapsed to prove auto-expand works on cursor sync
    state.collapse_all_segments();
    assert!(state.expanded_segments.is_empty());

    // 1a. Click inside `HOSP` -> PID-3.4
    let hosp_offset = state.inline_raw_text.find("HOSP").unwrap() + 1;
    state.select_from_raw_offset(hosp_offset);
    let expected_pid3_4: FieldAddress = "PID-3.4".parse().unwrap();
    assert_eq!(state.selected_segment, Some(1));
    assert_eq!(state.selected_field_path, Some(expected_pid3_4));
    assert!(state.expanded_segments.contains(&1));
    assert!(state.scroll_accordion_to_selection);
    assert!(state.scroll_to_field_row);
    let row_idx = state.selected_field_row.expect("PID-3.4 row highlighted");
    assert_eq!(
        state.record_cache.as_ref().unwrap().field_rows[row_idx].address,
        Some(expected_pid3_4)
    );

    // Clear one-shot scroll flag (as the Segment Accordion does after scrolling), then verify
    // clicking directly inside the Segment Accordion does NOT re-trigger `scroll_accordion_to_selection`.
    state.scroll_accordion_to_selection = false;
    state.select_field_row(0);
    assert!(
        !state.scroll_accordion_to_selection,
        "manual accordion click must not trigger auto-scroll"
    );

    // 1b. Click inside `B` in `A&B^DESC` -> OBX-5.1.2
    state.collapse_all_segments();
    let sub_b_offset = state.inline_raw_text.find("A&B").unwrap() + 2;
    state.select_from_raw_offset(sub_b_offset);
    let expected_obx5_1_2: FieldAddress = "OBX-5.1.2".parse().unwrap();
    assert_eq!(state.selected_segment, Some(2));
    assert_eq!(state.selected_field_path, Some(expected_obx5_1_2));
    assert!(state.expanded_segments.contains(&2));
    let obx_row_idx = state.selected_field_row.expect("OBX-5.1.2 row highlighted");
    assert_eq!(
        state.record_cache.as_ref().unwrap().field_rows[obx_row_idx].address,
        Some(expected_obx5_1_2)
    );

    // 2. Repetition: `PID|1||ID1^^^HOSP~ID2^^^CLINIC` -> click inside `ID2` resolves `PID-3(2).1`
    let raw_rep = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ADT^A01|P17_02|P|2.5.1\r",
        "PID|1||ID1^^^HOSP~ID2^^^CLINIC||SMITH^JANE\r",
    );
    let mut rep_state = SessionState::new();
    rep_state.paste_from_text(raw_rep, false, &settings);
    rep_state.collapse_all_segments();

    let id2_offset = rep_state.inline_raw_text.find("ID2").unwrap() + 1;
    rep_state.select_from_raw_offset(id2_offset);
    let expected_rep2: FieldAddress = "PID-3(2).1".parse().unwrap();
    assert_eq!(rep_state.selected_segment, Some(1));
    assert_eq!(rep_state.selected_field_path, Some(expected_rep2));
    assert!(rep_state.expanded_segments.contains(&1));
    let rep_row_idx = rep_state
        .selected_field_row
        .expect("PID-3(2).1 row highlighted");
    assert_eq!(
        rep_state.record_cache.as_ref().unwrap().field_rows[rep_row_idx].address,
        Some(expected_rep2)
    );

    // 3. Custom component delimiter `:` (`MSH|:~\&|...`) -> `PID-3.4` using `:`
    let raw_custom = concat!(
        "MSH|:~\\&|LAB|FAC|EMR|FAC|20260115120000||ADT:A01|P17_03|P|2.5.1\r",
        "PID|1||SYNTH0001:::HOSP:MR||DOE:JOHN\r",
    );
    let mut custom_state = SessionState::new();
    custom_state.paste_from_text(raw_custom, false, &settings);
    custom_state.collapse_all_segments();

    let custom_hosp_offset = custom_state.inline_raw_text.find("HOSP").unwrap() + 1;
    let delims = custom_state.doc.records[0].delimiters();
    assert_eq!(delims.component, b':');
    assert_eq!(
        resolve_raw_cursor_field_path(&custom_state.inline_raw_text, custom_hosp_offset, delims),
        Some((1, expected_pid3_4))
    );

    custom_state.select_from_raw_offset(custom_hosp_offset);
    assert_eq!(custom_state.selected_segment, Some(1));
    assert_eq!(custom_state.selected_field_path, Some(expected_pid3_4));
    assert!(custom_state.expanded_segments.contains(&1));
    let custom_row_idx = custom_state
        .selected_field_row
        .expect("custom-delim PID-3.4 row highlighted");
    assert_eq!(
        custom_state.record_cache.as_ref().unwrap().field_rows[custom_row_idx].address,
        Some(expected_pid3_4)
    );
}

#[test]
fn inline_accordion_value_edit_stages_once_cancels_on_escape_and_commits_on_enter() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ADT^A01|INLINE1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JOHN\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // Select PID-3.4 (`HOSP`)
    let hosp_offset = state.inline_raw_text.find("HOSP").unwrap() + 1;
    state.select_from_raw_offset(hosp_offset);
    let pid3_4: FieldAddress = "PID-3.4".parse().unwrap();
    assert_eq!(state.selected_field_path, Some(pid3_4));

    // 1. Start inline edit -> stages existing value once and requests focus on first frame
    state.start_inline_field_edit(&settings).unwrap();
    assert_eq!(state.editing_field_path, Some(pid3_4));
    assert_eq!(state.editing_field_buffer, "HOSP");
    assert!(state.editing_field_just_opened);

    // Simulate multi-keystroke typing across frames: record and undo stack stay untouched
    state.editing_field_just_opened = false;
    state.editing_field_buffer.push_str("_TEMP");
    assert_eq!(state.editing_field_buffer, "HOSP_TEMP");
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"HOSP"[..])
    );
    assert!(!state.undo_stack.can_undo());

    // 2. Escape cancels cleanly without modifying the message
    state.cancel_inline_field_edit();
    assert!(state.editing_field_path.is_none());
    assert!(state.editing_field_buffer.is_empty());
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"HOSP"[..])
    );
    assert!(!state.undo_stack.can_undo());

    // 3. Committing without changes is a no-op (no empty undo entry)
    state.start_inline_field_edit(&settings).unwrap();
    assert!(!state.commit_inline_field_edit(&settings).unwrap());
    assert!(!state.undo_stack.can_undo());

    // 4. Re-enter inline edit, type new value, commit -> single atomic undoable edit
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_just_opened = false;
    state.editing_field_buffer = "MAIN_CLINIC".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert!(state.editing_field_path.is_none());
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"MAIN_CLINIC"[..])
    );
    assert!(state.undo_stack.can_undo());

    // Undo restores "HOSP", Redo reapplies "MAIN_CLINIC"
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"HOSP"[..])
    );
    state.redo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"MAIN_CLINIC"[..])
    );
}

#[test]
fn hierarchy_aware_inline_field_edit_allows_composite_values_and_rejects_disallowed_delimiters() {
    let settings = AppSettings {
        display_compound_fields: true,
        ..AppSettings::default()
    };

    let raw = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ADT^A01|HIER1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JOHN\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // 1. Edit whole field MSH-9 (`ADT^A01` -> `ADT^A04`) inline: succeeds and updates child MSH-9.2 row
    let msh9: FieldAddress = "MSH-9".parse().unwrap();
    let msh9_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh9))
        .unwrap()
        .row_index;
    state.select_field_row(msh9_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "ADT^A04".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert_eq!(
        state.doc.records[0].field_bytes(0, &msh9),
        Some(&b"ADT^A04"[..])
    );
    let msh9_2: FieldAddress = "MSH-9.2".parse().unwrap();
    let msh9_2_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh9_2))
        .expect("MSH-9.2 child row present");
    assert_eq!(msh9_2_row.display_value, "A04");

    // 2. Edit whole field PID-3 with repetitions & components (`ID1^^^HOSP~ID2^^^CLINIC`): succeeds and spawns repetition rows
    let pid3: FieldAddress = "PID-3".parse().unwrap();
    let pid3_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(pid3))
        .unwrap()
        .row_index;
    state.select_field_row(pid3_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "ID1^^^HOSP~ID2^^^CLINIC".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    let pid3_rep2_4: FieldAddress = "PID-3(2).4".parse().unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_rep2_4),
        Some(&b"CLINIC"[..])
    );
    assert!(state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .any(|f| f.address == Some(pid3_rep2_4) && f.display_value == "CLINIC"));

    // 3. Typing literal `|` into MSH-9 auto-escapes to `\F\` without shifting subsequent MSH fields
    let msh9_row2 = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh9))
        .unwrap()
        .row_index;
    state.select_field_row(msh9_row2);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "ADT|SAFE^A04".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert_eq!(
        state.doc.records[0].field_bytes(0, &msh9),
        Some(&b"ADT\\F\\SAFE^A04"[..])
    );

    // 4. Typing literal `^` into component MSH-9.1 auto-escapes to `\S\` without shifting neighbor components
    let msh9_1: FieldAddress = "MSH-9.1".parse().unwrap();
    let msh9_1_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh9_1))
        .unwrap()
        .row_index;
    state.select_field_row(msh9_1_row);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "ADT^SAFE".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert_eq!(
        state.doc.records[0].field_bytes(0, &msh9_1),
        Some(&b"ADT\\S\\SAFE"[..])
    );
}

#[test]
fn filter_by_path_opens_filter_bar_populates_path_and_value_and_applies() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG2|P|2.5.1\r",
        "PID|1||SYNTH0002^^^CLINIC WEST^MR||SMITH^JOHN||19750505|M\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG3|P|2.5.1\r",
        "PID|1||SYNTH0003^^^^MR||EMPTY^FAC||19900101|U\r",
    );
    state.paste_from_text(raw, false, &settings);

    // 1. Simple value: PID-3.4 = HOSP
    state.filter_help_open = true;
    state.open_filter_for_path("PID-3.4", "HOSP", &settings);
    assert!(state.filter_bar_open);
    assert!(!state.filter_help_open);
    assert!(state.focus_filter_input);
    assert_eq!(state.filter_query, "PID-3.4=HOSP");
    assert!(state.filter_error.is_none());
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[0][..]));

    // 2. Value containing whitespace is quoted and matches cleanly
    state.focus_filter_input = false;
    state.open_filter_for_path("PID-3.4", "CLINIC WEST", &settings);
    assert!(state.filter_bar_open);
    assert!(state.focus_filter_input);
    assert_eq!(state.filter_query, "PID-3.4=\"CLINIC WEST\"");
    assert!(state.filter_error.is_none());
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[1][..]));

    // 3. Empty value matches empty/missing field cleanly
    state.focus_filter_input = false;
    state.open_filter_for_path("PID-3.4", "", &settings);
    assert_eq!(state.filter_query, "PID-3.4=\"\"");
    assert!(state.filter_error.is_none());
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[2][..]));
}

#[test]
fn multi_filter_stacking_and_or_export_filtered_and_keep_only_filtered() {
    use pipe_wrench::FilterCombineMode;

    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||TESTPATIENT^ALPHA||19800101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A04|MSG2|P|2.5.1\r",
        "PID|1||SYNTH0002^^^HOSP^MR||TESTPATIENT^BETA||19800101|M\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG3|P|2.5.1\r",
        "PID|1||SYNTH0003^^^CLINIC^MR||OTHER^GAMMA||19850101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG4|P|2.5.1\r",
        "PID|1||SYNTH0004^^^HOSP^MR||TESTPATIENT^DELTA||19900101|M\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ORU^R01|MSG5|P|2.5.1\r",
        "PID|1||SYNTH0005^^^HOSP^MR||OTHER^EPSILON||19920101|F\r",
    );
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.doc.len(), 5);

    // 1. Start with single filter on PID-5.1=TESTPATIENT (matches 0, 1, 3)
    state.open_filter_for_path("PID-5.1", "TESTPATIENT", &settings);
    assert_eq!(
        state.filtered_record_indices.as_deref(),
        Some(&[0, 1, 3][..])
    );

    // 2. Stack a second filter MSH-9.2=A01 via Add Filter (promotes first query into chips, AND -> 0, 3)
    state.add_filter_for_path("MSH-9.2", "A01", &settings);
    assert_eq!(state.active_filters.len(), 2);
    assert_eq!(state.active_filters[0].query, "PID-5.1=TESTPATIENT");
    assert_eq!(state.active_filters[1].query, "MSH-9.2=A01");
    assert_eq!(state.filter_combine_mode, FilterCombineMode::And);
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[0, 3][..]));

    // 3. Toggle to OR -> matches 0, 1, 2, 3 (all TESTPATIENT or all A01)
    state.set_filter_combine_mode(FilterCombineMode::Or, &settings);
    assert_eq!(
        state.filtered_record_indices.as_deref(),
        Some(&[0, 1, 2, 3][..])
    );

    // 4. Switch back to AND, verify Export Filtered writes only the 2 matched records without mutating session
    state.set_filter_combine_mode(FilterCombineMode::And, &settings);
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[0, 3][..]));
    let export_path = std::env::temp_dir().join("pipe_wrench_test_export_filtered.hl7");
    let exported_count = state
        .export_filtered_to_path(export_path.clone(), &settings)
        .unwrap();
    assert_eq!(exported_count, 2);
    assert_eq!(state.doc.len(), 5);
    let exported_bytes = std::fs::read(&export_path).unwrap();
    let exported_str = String::from_utf8_lossy(&exported_bytes);
    assert!(exported_str.contains("MSG1"));
    assert!(exported_str.contains("MSG4"));
    assert!(!exported_str.contains("MSG2"));
    assert!(!exported_str.contains("MSG3"));
    assert!(!exported_str.contains("MSG5"));
    let _ = std::fs::remove_file(&export_path);

    // 5. Remove one chip (index 0: PID-5.1=TESTPATIENT) -> only MSH-9.2=A01 remains (matches 0, 2, 3)
    state.remove_filter_rule(0, &settings);
    assert_eq!(state.active_filters.len(), 1);
    assert_eq!(state.active_filters[0].query, "MSH-9.2=A01");
    assert_eq!(
        state.filtered_record_indices.as_deref(),
        Some(&[0, 2, 3][..])
    );

    // 6. Keep Only Filtered prunes unmatched (1, 4) from memory and updates total to 3 of 3
    let pruned = state.request_keep_only_filtered(&settings).unwrap();
    assert!(pruned);
    assert_eq!(state.doc.len(), 3);
    assert_eq!(
        state.filtered_record_indices.as_deref(),
        Some(&[0, 1, 2][..])
    );
}

#[test]
fn inline_accordion_edit_live_syncs_raw_editor_with_single_atomic_undo_and_clean_escape_revert() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JANE||19800101|F\r",
    );
    state.paste_from_text(raw, false, &settings);
    let baseline_raw = state.inline_raw_text.clone();
    assert!(!state.undo_stack.can_undo());

    // Locate PID-5.1 ("DOE")
    let pid5_1: FieldAddress = "PID-5.1".parse().unwrap();
    let row_idx = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(pid5_1))
        .unwrap()
        .row_index;
    state.select_field_row(row_idx);
    state.start_inline_field_edit(&settings).unwrap();

    // 1. Live keystrokes update Raw Message Editor immediately without pushing UndoStack entries
    state.editing_field_buffer = "DO".to_string();
    assert!(state.apply_live_inline_field_preview(&settings));
    assert!(state.inline_raw_text.contains("|DO^JANE|"));
    assert!(!state.undo_stack.can_undo());

    state.editing_field_buffer = "SMITH".to_string();
    assert!(state.apply_live_inline_field_preview(&settings));
    assert!(state.inline_raw_text.contains("|SMITH^JANE|"));
    assert!(!state.undo_stack.can_undo());

    // 2. Typing a disallowed delimiter (`^` in component PID-5.1) keeps last valid preview with no modal popup
    state.editing_field_buffer = "SMITH^BAD".to_string();
    assert!(!state.apply_live_inline_field_preview(&settings));
    assert!(state.modal_alert.is_none());
    assert!(state.inline_raw_text.contains("|SMITH^JANE|"));

    // 3. Pressing Escape reverts Raw Message Editor & record bytes back to exact baseline with 0 undo entries
    state.cancel_inline_field_edit();
    assert_eq!(state.inline_raw_text, baseline_raw);
    assert!(state.editing_field_path.is_none());
    assert!(!state.undo_stack.can_undo());

    // 4. Start another inline edit, type multiple keystrokes, then commit with Enter -> single atomic UndoStack entry
    state.select_field_row(row_idx);
    state.start_inline_field_edit(&settings).unwrap();
    for step in ["J", "JO", "JON", "JONES"] {
        state.editing_field_buffer = step.to_string();
        assert!(state.apply_live_inline_field_preview(&settings));
        assert!(state.inline_raw_text.contains(&format!("|{step}^JANE|")));
        assert!(!state.undo_stack.can_undo());
    }
    assert!(state.commit_inline_field_edit(&settings).unwrap());
    assert!(state.inline_raw_text.contains("|JONES^JANE|"));
    assert!(state.undo_stack.can_undo());

    // Single Ctrl+Z reverts the entire multi-keystroke edit back to "DOE"
    state.undo(&settings).unwrap();
    assert_eq!(state.inline_raw_text, baseline_raw);
    assert!(!state.undo_stack.can_undo());
}

#[test]
fn segment_accordion_multi_select_report_filter_and_copy_formatting() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||TESTPATIENT^ALPHA||19800101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A04|MSG2|P|2.5.1\r",
        "PID|1||SYNTH0002^^^CLINIC^MR||TESTPATIENT^BETA||19800101|M\r",
    );
    state.paste_from_text(raw, false, &settings);

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
    let row_msh9_2 = find_row(&state, "MSH-9.2");
    let row_pid3_4 = find_row(&state, "PID-3.4");
    let row_pid5_1 = find_row(&state, "PID-5.1");
    let row_pid5_2 = find_row(&state, "PID-5.2");

    // 1. Plain click selects single row and formats Copy Value / Path / Summary cleanly
    state.select_field_row_with_modifiers(row_pid5_1, false, false);
    assert_eq!(state.selected_field_rows.len(), 1);
    assert_eq!(state.format_accordion_copy_values(), "TESTPATIENT");
    assert_eq!(state.format_accordion_copy_paths(), "PID-5.1");
    assert_eq!(
        state.format_accordion_copy_summary(),
        "PID-5.1 (Family Name): TESTPATIENT"
    );

    // 2. Ctrl+Click toggles additional non-contiguous rows (MSH-9.2 and PID-3.4)
    state.select_field_row_with_modifiers(row_msh9_2, true, false);
    state.select_field_row_with_modifiers(row_pid3_4, true, false);
    assert_eq!(state.selected_field_rows.len(), 3);
    assert_eq!(
        state.format_accordion_copy_values(),
        "A01\nHOSP\nTESTPATIENT"
    );
    assert_eq!(
        state.format_accordion_copy_paths(),
        "MSH-9.2\nPID-3.4\nPID-5.1"
    );
    assert_eq!(
        state.format_accordion_copy_summary(),
        "MSH-9.2 (Trigger Event): A01\nPID-3.4 (Assigning Authority): HOSP\nPID-5.1 (Family Name): TESTPATIENT"
    );

    // 3. Multi-field Filter by Selected Values stacks all 3 rules into active_filters
    state.filter_by_selected_accordion_rows(false, &settings);
    assert!(state.filter_bar_open);
    assert_eq!(state.active_filters.len(), 3);
    assert_eq!(state.active_filters[0].query, "MSH-9.2=A01");
    assert_eq!(state.active_filters[1].query, "PID-3.4=HOSP");
    assert_eq!(state.active_filters[2].query, "PID-5.1=TESTPATIENT");
    assert_eq!(state.filtered_record_indices.as_deref(), Some(&[0][..]));

    // 4. Multi-field Build Report pre-populates Report Builder with selected paths in display order
    state.open_report_from_selected_accordion_fields();
    assert!(state.report_window.open);
    let col_paths: Vec<String> = state
        .report_window
        .definition
        .columns
        .iter()
        .map(|c| c.address.to_string())
        .collect();
    assert_eq!(col_paths, vec!["MSH-9.2", "PID-3.4", "PID-5.1"]);

    // 5. Shift+Click selects contiguous visible range between anchor (PID-5.1) and PID-5.2
    state.select_field_row_with_modifiers(row_pid5_1, false, false);
    state.select_field_row_with_modifiers(row_pid5_2, false, true);
    assert!(state.selected_field_rows.contains(&row_pid5_1));
    assert!(state.selected_field_rows.contains(&row_pid5_2));
    assert_eq!(state.selected_field_rows.len(), 2);
}

#[test]
fn show_empty_toggle_reveals_empty_dictionary_fields_and_inline_edit_pads_delimiters() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JANE\r",
        "PV1|1|I|WARD1^101^A\r",
    );
    state.paste_from_text(raw, false, &settings);

    let msh8: FieldAddress = "MSH-8".parse().unwrap();
    let pv1_19: FieldAddress = "PV1-19".parse().unwrap();

    // By default (show_empty_fields == false), empty MSH-8 and trailing PV1-19 are hidden
    assert!(!state.show_empty_fields);
    assert!(!state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .any(|f| f.address == Some(msh8)));
    assert!(!state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .any(|f| f.address == Some(pv1_19)));

    // Enabling Show Empty reveals both MSH-8 (Security) and trailing PV1-19 (Visit Number)
    state.set_show_empty_fields(true, &settings);
    assert!(state.show_empty_fields);
    let msh8_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(msh8))
        .expect("MSH-8 should be revealed when Show Empty is active");
    assert_eq!(msh8_row.description.as_deref(), Some("Security"));
    assert!(msh8_row.display_value.is_empty());
    assert!(msh8_row.is_editable);

    let pv1_19_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .find(|f| f.address == Some(pv1_19))
        .expect("PV1-19 should be revealed when Show Empty is active");
    assert_eq!(pv1_19_row.description.as_deref(), Some("Visit Number"));
    let pv1_19_row_idx = pv1_19_row.row_index;

    // Inline editing the revealed trailing empty field PV1-19 pads intermediate `|` delimiters cleanly
    state.select_field_row(pv1_19_row_idx);
    state.start_inline_field_edit(&settings).unwrap();
    state.editing_field_buffer = "VISIT9999".to_string();
    assert!(state.commit_inline_field_edit(&settings).unwrap());

    assert_eq!(
        state.doc.records[0].field_bytes(2, &pv1_19),
        Some(&b"VISIT9999"[..])
    );
    assert!(state.inline_raw_text.contains("VISIT9999"));
}
