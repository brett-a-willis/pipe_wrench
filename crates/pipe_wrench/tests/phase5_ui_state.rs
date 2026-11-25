//! Phase 5 tests for `pipe_wrench` GUI session state, 4-pane synchronization,
//! display options, dictionary lookup, and 50 ms record selection performance (NFR-1).

use std::time::Instant;

use hl7_core::{Delimiters, Record, RecordFlags, RecordKind, SourceTerminator};
use pipe_wrench::{AppSettings, DataSource, SessionState};
use tempfile::TempDir;

// §5.1, §5.2, §5.3, §5.4, §5.5, G-2, NET-4
#[test]
fn synchronized_pane_selection_and_display_options() {
    let mut settings = AppSettings::default();
    let mut state = SessionState::new();

    let raw = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ORU^R01|MSG100|P|2.5.1||||||UNKNOWN_CS\r",
        "PID|1||PAT123^^^HOSP^MR||DOE^JOHN~ALT^NAME||\r",
        "OBX|1|TX|NOTE||Line1\\.br\\Line2 \\F\\ Pipe\r",
    );
    state.paste_from_text(raw, false, &settings);

    assert_eq!(state.doc.len(), 1);
    assert_eq!(state.source, DataSource::Clipboard);
    assert_eq!(state.selected_record, Some(0));
    assert_eq!(state.selected_segment, Some(0));

    let cache = state.record_cache.as_ref().expect("cache built");
    // G-2: Unknown charset fallback flagged
    assert!(cache.charset.is_unknown_fallback);
    assert_eq!(cache.charset.declared.as_deref(), Some("UNKNOWN_CS"));
    assert_eq!(cache.segments.len(), 3);
    assert_eq!(
        cache.segments[1].description.as_deref(),
        Some("Patient Identification")
    );

    // Selecting segment 1 (PID) scrolls and selects its first field row in the field list (§5.1)
    state.select_segment(1);
    assert_eq!(state.selected_segment, Some(1));
    assert!(state.scroll_to_field_row);
    let pid_first_row = state.selected_field_row.expect("field row selected");
    let frow = &state.record_cache.as_ref().unwrap().field_rows[pid_first_row];
    assert_eq!(frow.address.unwrap().to_string(), "PID-1");
    assert_eq!(frow.description.as_deref(), Some("Set ID - PID"));

    // Selecting an OBX field row synchronizes segment selection back to segment 2 (§5.1)
    let obx_row_idx = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .position(|r| r.address.map(|a| a.to_string()).as_deref() == Some("OBX-5"))
        .unwrap();
    state.select_field_row(obx_row_idx);
    assert_eq!(state.selected_segment, Some(2));
    assert!(state.scroll_to_segment);

    // Raw pane click offset synchronization back to segment & field (§5.1)
    let obx_raw_offset = state.record_cache.as_ref().unwrap().field_rows[obx_row_idx]
        .raw_text_range
        .start;
    state.select_segment(0);
    state.select_from_raw_offset(obx_raw_offset);
    assert_eq!(state.selected_segment, Some(2));
    assert_eq!(state.selected_field_row, Some(obx_row_idx));

    // §5.5: Decode Escape Sequences option renders decoded text and makes fields read-only
    settings.decode_escape_sequences = true;
    state.rebuild_selected_record_cache(&settings);
    let obx_decoded = &state.record_cache.as_ref().unwrap().field_rows[obx_row_idx];
    assert_eq!(obx_decoded.display_value, "Line1\nLine2 | Pipe");
    assert!(!obx_decoded.is_editable);
}

// NFR-1: Selecting a record updates all panes in under 50 ms even with a large record list.
#[test]
fn large_record_list_selection_updates_under_50ms() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();

    // Build 50,000 synthetic records in memory
    let sample_bytes = b"MSH|^~\\&|S|F|R|F|20260115120000||ADT^A01|MSG0001|P|2.5.1\rPID|1||PAT001^^^HOSP^MR||TEST^PATIENT\rPV1|1|I|WARD^101^A".to_vec();
    let records: Vec<Record> = (0..50_000)
        .map(|_| {
            Record::from_normalized_bytes(
                sample_bytes.clone(),
                RecordKind::Msh,
                Delimiters::STANDARD,
                SourceTerminator::Cr,
                RecordFlags::default(),
            )
        })
        .collect();

    state.set_records(records, DataSource::Clipboard, &settings);
    assert_eq!(state.summaries.len(), 50_000);

    let start = Instant::now();
    state.select_record(49_999, false, &settings);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "selecting a record took {} ms (limit 50 ms, NFR-1)",
        elapsed.as_millis()
    );
    assert_eq!(state.record_cache.as_ref().unwrap().segments.len(), 3);
}

// G-6, §5.3: Persisted AppSettings TOML round-trip
#[test]
fn app_settings_toml_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.toml");

    let settings = AppSettings {
        display_empty_fields: true,
        decode_escape_sequences: true,
        record_pane_width: 512.0,
        ..AppSettings::default()
    };
    settings.save_to(&path).unwrap();

    let loaded = AppSettings::load_from(&path);
    assert_eq!(loaded, settings);
}
