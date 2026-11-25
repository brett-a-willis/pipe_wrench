//! Phase 13 integration tests covering Find & Replace (`Ctrl+F` / `Ctrl+H`)
//! with granular HL7 v2 field/component/subcomponent targeting (`PID-3.1.4`, `PID-3(2).4`, `OBX-5`),
//! `Wrap around`, `Match case`, `Match whole value / word`, `Regex` capture-group replacement (`$1`),
//! single `Replace` + advance, scoped `Replace All` (`CurrentRecord` / `FilteredRecords` / `AllRecords`),
//! and full `UndoStack` undo/redo.

use hl7_core::FieldAddress;
use pipe_wrench::{AppSettings, FindReplaceScope, FindReplaceTab, SessionState};

#[test]
fn find_replace_tabs_wrap_around_match_case_and_regex_capture_groups() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|HOSP|RCV|FAC|20260115120000||ADT^A01|MSG01|P|2.5.1\r",
        "PID|1||PAT-1001^^^HOSP^MR||DOE^JOHN\r",
        "OBX|1|ST|NOTE||alpha beta\r",
        "MSH|^~\\&|SND|HOSP|RCV|FAC|20260115120001||ADT^A08|MSG02|P|2.5.1\r",
        "PID|1||PAT-2002^^^HOSP^MR||SMITH^JANE\r",
        "OBX|1|ST|NOTE||ALPHA gamma\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // 1. Ctrl+F opens Find tab, Ctrl+H opens Replace tab, Escape closes panel
    state.open_find_panel();
    assert!(state.search_bar_open);
    assert_eq!(state.find_replace_tab, FindReplaceTab::Find);
    assert!(state.focus_search_input);

    state.open_replace_panel();
    assert!(state.search_bar_open);
    assert_eq!(state.find_replace_tab, FindReplaceTab::Replace);

    state.close_find_replace_panel();
    assert!(!state.search_bar_open);

    // 2. Match case toggle + Wrap around toggle
    state.open_find_panel();
    state.search_query = "alpha".to_string();
    state.search_case_sensitive = false;
    state.search_wrap_around = false;

    let hit1 = state.jump_search(true, &settings).unwrap();
    assert_eq!(hit1.record_index, 0);
    let hit2 = state.jump_search(true, &settings).unwrap();
    assert_eq!(hit2.record_index, 1);
    // With wrap_around = false, advancing past the last match stops instead of wrapping
    assert!(state.jump_search(true, &settings).is_none());
    assert!(state.search_status.contains("wrap off"));

    // Enabling wrap_around wraps back to the first match with "(wrapped)" status
    state.search_wrap_around = true;
    let wrapped = state.jump_search(true, &settings).unwrap();
    assert_eq!(wrapped.record_index, 0);
    assert!(state.search_status.contains("wrapped"));

    // Case-sensitive search only matches lowercase "alpha" in record 0
    state.search_case_sensitive = true;
    let (count, recs) = state.count_find_matches().unwrap();
    assert_eq!((count, recs), (1, 1));

    // 3. Regex capture-group replacement ($1) across PID-3.1
    state.open_replace_panel();
    state.search_query = "PAT-(\\d+)".to_string();
    state.replace_with_input = "MRN-$1".to_string();
    state.search_field_input = "PID-3.1".to_string();
    state.search_use_regex = true;
    state.search_case_sensitive = true;
    state.find_replace_scope = FindReplaceScope::AllRecords;

    let summary = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(summary.occurrences, 2);
    assert_eq!(summary.records_modified, 2);
    assert_eq!(
        state.replace_status_banner,
        "Replaced 2 occurrences across 2 records"
    );

    let pid3_1: FieldAddress = "PID-3.1".parse().unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1),
        Some(&b"MRN-1001"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &pid3_1),
        Some(&b"MRN-2002"[..])
    );

    // Single Undo reverts both records atomically; single Redo reapplies both
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1),
        Some(&b"PAT-1001"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &pid3_1),
        Some(&b"PAT-2002"[..])
    );

    state.redo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1),
        Some(&b"MRN-1001"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &pid3_1),
        Some(&b"MRN-2002"[..])
    );
}

#[test]
fn granular_field_component_subcomponent_targeting_isolates_modifications() {
    let settings = AppSettings::default();
    // Notice "MAIN" appears in MSH-4, PID-3.1.1, PID-3.1.4, PID-3(2).4, and OBX-5!
    let raw = concat!(
        "MSH|^~\\&|SND|MAIN|RCV|FAC|20260115120000||ADT^A01|MSG01|P|2.5.1\r",
        "PID|1||MAIN&S2&S3&MAIN^C2^C3^AUTH1~ID2^C2^C3^MAIN||DOE^JOHN\r",
        "OBX|1|ST|CODE||MAIN\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // 1. Target strictly subcomponent PID-3.1.4: replace "MAIN" with "SUB4"
    state.open_replace_panel();
    state.search_query = "MAIN".to_string();
    state.replace_with_input = "SUB4".to_string();
    state.search_field_input = "PID-3.1.4".to_string();
    state.search_use_regex = false;
    state.search_case_sensitive = true;
    state.find_replace_scope = FindReplaceScope::AllRecords;

    let summary = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(summary.occurrences, 1);
    assert_eq!(summary.records_modified, 1);

    let msh4: FieldAddress = "MSH-4".parse().unwrap();
    let pid3_1_1: FieldAddress = "PID-3.1.1".parse().unwrap();
    let pid3_1_4: FieldAddress = "PID-3.1.4".parse().unwrap();
    let pid3_rep2_4: FieldAddress = "PID-3(2).4".parse().unwrap();
    let obx5: FieldAddress = "OBX-5".parse().unwrap();

    // Only PID-3.1.4 changed; MSH-4, PID-3.1.1, PID-3(2).4, and OBX-5 are untouched!
    assert_eq!(
        state.doc.records[0].field_bytes(0, &msh4),
        Some(&b"MAIN"[..])
    );
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1_1),
        Some(&b"MAIN"[..])
    );
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1_4),
        Some(&b"SUB4"[..])
    );
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_rep2_4),
        Some(&b"MAIN"[..])
    );
    assert_eq!(
        state.doc.records[0].field_bytes(2, &obx5),
        Some(&b"MAIN"[..])
    );

    // 2. Target strictly PID-3(2).4 with empty "Find what" to bulk-set its entire value
    state.search_query.clear();
    state.replace_with_input = "AUTH2".to_string();
    state.search_field_input = "PID-3(2).4".to_string();
    let summary2 = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(summary2.occurrences, 1);
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_rep2_4),
        Some(&b"AUTH2"[..])
    );
    // PID-3.1.4 still "SUB4", OBX-5 still "MAIN"
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1_4),
        Some(&b"SUB4"[..])
    );
    assert_eq!(
        state.doc.records[0].field_bytes(2, &obx5),
        Some(&b"MAIN"[..])
    );

    // Undo both steps cleanly
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_rep2_4),
        Some(&b"MAIN"[..])
    );
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_1_4),
        Some(&b"MAIN"[..])
    );
}

#[test]
fn single_replace_advances_and_scoped_replace_all_respects_current_filtered_and_all() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\rOBX|1|ST|NOTE||OLD\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ORU^R01|MSG2|P|2.5.1\rOBX|1|ST|NOTE||OLD\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120002||ORU^R01|MSG3|P|2.5.1\rOBX|1|ST|NOTE||OLD\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);
    let obx5: FieldAddress = "OBX-5".parse().unwrap();

    // 1. Single Replace replaces active match in Record 0 and advances to Record 1
    state.open_replace_panel();
    state.search_query = "OLD".to_string();
    state.replace_with_input = "STEP1".to_string();
    state.search_field_input = "OBX-5".to_string();

    let next_after_replace = state.replace_current_and_advance(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &obx5),
        Some(&b"STEP1"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );
    assert_eq!(next_after_replace.unwrap().record_index, 1);

    // Undo single Replace restores Record 0 back to "OLD"
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );

    // 2. Replace All with CurrentRecord scope only touches selected record (Record 0)
    state.select_record(0, false, &settings);
    state.replace_with_input = "CUR_ONLY".to_string();
    state.find_replace_scope = FindReplaceScope::CurrentRecord;
    let s_cur = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(s_cur.occurrences, 1);
    assert_eq!(s_cur.records_modified, 1);
    assert_eq!(
        state.doc.records[0].field_bytes(1, &obx5),
        Some(&b"CUR_ONLY"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );
    state.undo(&settings).unwrap();

    // 3. Replace All with FilteredRecords scope only touches filtered ORU records (Records 1 & 2)
    state.filter_query = "MSH-9.1=ORU".to_string();
    state.apply_filter(&settings);
    state.replace_with_input = "ORU_NEW".to_string();
    state.find_replace_scope = FindReplaceScope::FilteredRecords;
    let s_filt = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(s_filt.occurrences, 2);
    assert_eq!(s_filt.records_modified, 2);
    assert_eq!(
        state.doc.records[0].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );
    assert_eq!(
        state.doc.records[1].field_bytes(1, &obx5),
        Some(&b"ORU_NEW"[..])
    );
    assert_eq!(
        state.doc.records[2].field_bytes(1, &obx5),
        Some(&b"ORU_NEW"[..])
    );

    // Undo reverts both filtered records in one step
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[1].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );
    assert_eq!(
        state.doc.records[2].field_bytes(1, &obx5),
        Some(&b"OLD"[..])
    );
}

#[test]
fn find_and_replace_empty_target_path_populates_missing_component_and_undoes() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||12345||DOE^JOHN\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120100||ADT^A04|MSG2|P|2.5.1\r",
        "PID|1||67890^^^EXISTING||SMITH^JANE\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // Empty Find what + Target Path PID-3.4 locates only record 0 (missing PID-3.4)
    state.open_replace_panel();
    state.search_query.clear();
    state.search_field_input = "PID-3.4".to_string();
    state.search_match_whole = true;
    state.replace_with_input = "HOSP".to_string();
    state.find_replace_scope = FindReplaceScope::AllRecords;

    let (count, recs) = state.count_find_matches().unwrap();
    assert_eq!((count, recs), (1, 1));

    let hit = state.jump_search(true, &settings).unwrap();
    assert_eq!(hit.record_index, 0);
    assert_eq!(hit.segment_index, 1);

    // Replace All populates missing PID-3.4 with HOSP while preserving EXISTING on record 1
    let summary = state.replace_all_in_scope(&settings).unwrap();
    assert_eq!(summary.occurrences, 1);
    assert_eq!(summary.records_modified, 1);

    let pid3_4: FieldAddress = "PID-3.4".parse().unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3_4),
        Some(&b"HOSP"[..])
    );
    assert!(String::from_utf8_lossy(state.doc.records[0].bytes())
        .contains("PID|1||12345^^^HOSP||DOE^JOHN"));
    assert_eq!(
        state.doc.records[1].field_bytes(1, &pid3_4),
        Some(&b"EXISTING"[..])
    );

    // Undo cleanly restores original PID|1||12345||DOE^JOHN
    state.undo(&settings).unwrap();
    assert!(
        String::from_utf8_lossy(state.doc.records[0].bytes()).contains("PID|1||12345||DOE^JOHN")
    );
}
