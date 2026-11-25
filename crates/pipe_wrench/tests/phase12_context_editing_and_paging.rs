//! Phase 12 integration tests covering:
//! 1. Bounded Record List Windowing / Paging (`max_records_per_page`, `Prev Page` / `Next Page` /
//!    `Rec # / Total` jump across page boundaries, and `Load All` override).
//! 2. Segment Right-Click Context-Menu Editing (`Edit Segment...` including `MSH` delimiter updates,
//!    `Insert Segment Above`, `Insert Segment Below`, `Insert Segment at End`, `Move Segment Up`,
//!    `Move Segment Down` with `MSH` index 0 and bottom boundary guards, and `Delete Segment`).
//! 3. Field Right-Click Context-Menu Editing (`Edit Field...` and `Clear / Delete Field` with
//!    trailing empty separator trimming), verifying full `UndoStack` undo and redo across all operations.

use hl7_core::{load_from_bytes, Delimiters, FieldAddress, ScannerConfig};
use pipe_wrench::{AppSettings, DataSource, SessionState, DEFAULT_MAX_RECORDS_PER_PAGE};

#[test]
fn bounded_record_list_paging_and_cross_page_navigation() {
    let mut settings = AppSettings::default();
    assert_eq!(settings.max_records_per_page, DEFAULT_MAX_RECORDS_PER_PAGE);

    // Configure 4 records per page and load 10 synthetic records (3 pages: 4 + 4 + 2).
    settings.max_records_per_page = 4;
    let mut raw = String::new();
    for i in 1..=10 {
        raw.push_str(&format!(
            "MSH|^~\\&|SND|FAC|RCV|FAC|202601151200{:02}||ADT^A01|MSG{i:02}|P|2.5.1\rPID|1||PAT{i:02}||DOE^USER{i:02}\r",
            i
        ));
    }
    let out = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
    let mut state = SessionState::new();
    state.set_records(out.records, DataSource::Clipboard, &settings);

    assert_eq!(state.doc.len(), 10);
    assert!(state.is_record_list_paged(&settings));
    assert_eq!(state.total_record_pages(&settings), 3);
    assert_eq!(state.record_page_index, 0);

    let (page0_indices, page0_offset) = state.current_page_visible_indices(&settings);
    assert_eq!(page0_indices, vec![0, 1, 2, 3]);
    assert_eq!(page0_offset, 0);

    // Next Page -> Page 2 of 3 (records 4..=7, selects first record on page 2 = doc index 4)
    let sel = state.navigate_next_record_page(&settings);
    assert_eq!(sel, Some(4));
    assert_eq!(state.record_page_index, 1);
    let (page1_indices, page1_offset) = state.current_page_visible_indices(&settings);
    assert_eq!(page1_indices, vec![4, 5, 6, 7]);
    assert_eq!(page1_offset, 4);

    // Next Page -> Page 3 of 3 (records 8..=9, selects doc index 8)
    let sel = state.navigate_next_record_page(&settings);
    assert_eq!(sel, Some(8));
    assert_eq!(state.record_page_index, 2);
    let (page2_indices, page2_offset) = state.current_page_visible_indices(&settings);
    assert_eq!(page2_indices, vec![8, 9]);
    assert_eq!(page2_offset, 8);

    // Prev Page -> back to Page 2 of 3 (selects doc index 4)
    let sel = state.navigate_prev_record_page(&settings);
    assert_eq!(sel, Some(4));
    assert_eq!(state.record_page_index, 1);

    // Rec # / Total inline jump across page boundaries (jump to Rec 10 -> switches to Page 3, index 9)
    state.begin_record_jump();
    state.record_jump_input = "10".to_string();
    let jumped = state.commit_record_jump_input(&settings);
    assert_eq!(jumped, Some(9));
    assert_eq!(state.selected_record, Some(9));
    assert_eq!(state.record_page_index, 2);

    // Jump back to Rec 2 -> switches to Page 1, index 1
    let jumped = state.jump_to_visible_record_number(2, &settings);
    assert_eq!(jumped, Some(1));
    assert_eq!(state.record_page_index, 0);

    // Clicking "Load All" overrides page capping and exposes all 10 records in a single view
    state.load_all_record_pages();
    assert!(!state.is_record_list_paged(&settings));
    assert_eq!(state.total_record_pages(&settings), 1);
    let (all_indices, offset) = state.current_page_visible_indices(&settings);
    assert_eq!(all_indices.len(), 10);
    assert_eq!(offset, 0);
}

#[test]
fn segment_context_menu_edit_insert_move_delete_and_undo_redo() {
    let settings = AppSettings {
        prompt_before_deleting: false,
        ..AppSettings::default()
    };

    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG01|P|2.5.1\r",
        "PID|1||12345||DOE^JOHN\r",
        "PV1|1|I|WARD^101^A\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.doc.records[0].segments().len(), 3);

    // 1. Edit Segment... on PID (index 1), plus Undo & Redo
    state.open_edit_segment_dialog(1);
    assert_eq!(
        state.edit_segment_dialog.as_ref().unwrap().text,
        "PID|1||12345||DOE^JOHN"
    );
    state
        .edit_segment_raw(1, "PID|1||99999||SMITH^JANE", &settings)
        .unwrap();
    state.edit_segment_dialog = None;

    let pid3: FieldAddress = "PID-3".parse().unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3),
        Some(&b"99999"[..])
    );

    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3),
        Some(&b"12345"[..])
    );
    state.redo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].field_bytes(1, &pid3),
        Some(&b"99999"[..])
    );

    // 2. Edit Segment... on MSH (index 0) updates active delimiters on commit and restores on Undo
    state
        .edit_segment_raw(
            0,
            "MSH:;~\\&:SND:FAC:RCV:FAC:20260115120000::ADT;A01:MSG01:P:2.5.1",
            &settings,
        )
        .unwrap();
    assert_eq!(
        state.doc.records[0].delimiters(),
        Delimiters {
            field: b':',
            component: b';',
            repetition: b'~',
            escape: b'\\',
            subcomponent: b'&',
        }
    );
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].delimiters(), Delimiters::STANDARD);

    // 3. Insert Segment Above (blocked on MSH index 0, allowed on index 1), Insert Below, and Insert at End
    assert!(!state.can_insert_segment_above(0));
    assert!(state
        .insert_segment_above(0, "NTE|1|L|Illegal above MSH", &settings)
        .is_err());

    assert!(state.can_insert_segment_above(1));
    state
        .insert_segment_above(1, "EVN|A01|20260115120000", &settings)
        .unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 4);
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"EVN|A01|20260115120000"[..])
    );
    assert_eq!(state.selected_segment, Some(1));

    // Insert Segment Below PID (which is now at index 2 -> inserts NK1 at index 3)
    state
        .insert_segment_below(2, "NK1|1|SMITH^SPOUSE|SPO", &settings)
        .unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 5);
    assert_eq!(
        state.doc.records[0].segment_bytes(3),
        Some(&b"NK1|1|SMITH^SPOUSE|SPO"[..])
    );

    // Deselect segment and Insert Segment at End
    state.selected_segment = None;
    state
        .insert_segment_at_end("NTE|1|L|Trailing note", &settings)
        .unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 6);
    assert_eq!(
        state.doc.records[0].segment_bytes(5),
        Some(&b"NTE|1|L|Trailing note"[..])
    );
    assert_eq!(state.selected_segment, Some(5));

    // Undo and Redo the Insert Segment at End
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 5);
    state.redo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 6);

    // 4. Move Segment Up and Move Segment Down (with boundary guards around index 0 MSH and last segment)
    // Current segments: [0: MSH, 1: EVN, 2: PID, 3: NK1, 4: PV1, 5: NTE]
    assert!(!state.can_move_segment_up(0)); // MSH at top
    assert!(!state.can_move_segment_down(0)); // MSH cannot move down away from index 0
    assert!(!state.can_move_segment_up(1)); // EVN at index 1 cannot move above MSH at index 0
    assert!(!state.can_move_segment_down(5)); // NTE at bottom cannot move down
    assert!(state.can_move_segment_up(2)); // PID at index 2 can move up to index 1
    assert!(state.can_move_segment_down(2)); // PID at index 2 can move down to index 3

    // Move NTE (index 5) Up -> swaps with PV1 (index 4)
    state.move_segment_up(5, &settings).unwrap();
    assert_eq!(state.selected_segment, Some(4));
    assert_eq!(
        state.doc.records[0].segment_bytes(4),
        Some(&b"NTE|1|L|Trailing note"[..])
    );
    assert_eq!(
        state.doc.records[0].segment_bytes(5),
        Some(&b"PV1|1|I|WARD^101^A"[..])
    );

    // Move NTE (now at index 4) Down -> swaps back to index 5
    state.move_segment_down(4, &settings).unwrap();
    assert_eq!(state.selected_segment, Some(5));
    assert_eq!(
        state.doc.records[0].segment_bytes(5),
        Some(&b"NTE|1|L|Trailing note"[..])
    );

    // Undo and Redo segment move
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(4),
        Some(&b"NTE|1|L|Trailing note"[..])
    );
    state.redo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(5),
        Some(&b"NTE|1|L|Trailing note"[..])
    );

    // 5. Delete Segment via context-menu handler (blocked on MSH index 0, undoable on other segments)
    assert!(state.request_delete_segment_at(0, &settings).is_err());
    state.request_delete_segment_at(5, &settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 5);
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 6);
}

#[test]
fn field_context_menu_clear_delete_field_trims_separators_and_supports_undo_redo() {
    let settings = AppSettings {
        prompt_before_deleting: false,
        ..AppSettings::default()
    };

    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG01|P|2.5.1\r",
        "PID|1||12345^^^HOSP&OID&ISO||DOE^JOHN^MIDDLE\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);

    // 1. Clear subcomponent PID-3.4.3 ("ISO") -> trims trailing `&` so PID-3 becomes `12345^^^HOSP&OID`
    let pid3_4_3: FieldAddress = "PID-3.4.3".parse().unwrap();
    state.clear_field_at(1, pid3_4_3, &settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID||DOE^JOHN^MIDDLE"[..])
    );

    // 2. Clear component PID-5.3 ("MIDDLE") -> trims trailing `^` so PID-5 becomes `DOE^JOHN`
    let pid5_3: FieldAddress = "PID-5.3".parse().unwrap();
    state.clear_field_at(1, pid5_3, &settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID||DOE^JOHN"[..])
    );

    // 3. Clear entire trailing field PID-5 -> trims trailing empty field separators (`||`) cleanly!
    let pid5: FieldAddress = "PID-5".parse().unwrap();
    state.clear_field_at(1, pid5, &settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID"[..])
    );

    // 4. Verify MSH-1 and MSH-2 cannot be cleared
    let msh1: FieldAddress = "MSH-1".parse().unwrap();
    let msh2: FieldAddress = "MSH-2".parse().unwrap();
    assert!(state.clear_field_at(0, msh1, &settings).is_err());
    assert!(state.clear_field_at(0, msh2, &settings).is_err());

    // 5. Undo and Redo all three field clears
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID||DOE^JOHN"[..])
    );
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID||DOE^JOHN^MIDDLE"[..])
    );
    state.undo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID&ISO||DOE^JOHN^MIDDLE"[..])
    );

    state.redo(&settings).unwrap();
    state.redo(&settings).unwrap();
    state.redo(&settings).unwrap();
    assert_eq!(
        state.doc.records[0].segment_bytes(1),
        Some(&b"PID|1||12345^^^HOSP&OID"[..])
    );
}

#[test]
fn live_raw_editor_auto_apply_synchronization_and_undo_coalescing() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG100|P|2.5.1\r",
        "PID|1||12345||DOE^JOHN\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.summaries[0].message_type, "ADT^A01");
    assert_eq!(state.summaries[0].control_id, "MSG100");
    assert_eq!(state.doc.records[0].segments().len(), 2);

    // Simulate a multi-keystroke typing burst in the Raw Message Editor:
    // Keystroke 1: change MSH-9 from ADT^A01 to ADT^A08 and MSH-10 to MSG999
    state.inline_raw_cursor = Some(10);
    state.inline_raw_text = state
        .inline_raw_text
        .replace("ADT^A01|MSG100", "ADT^A08|MSG999");
    state.apply_live_inline_raw_edit(&settings);

    // Verify real-time synchronization before UndoStack flush:
    // - Records table summary (Type, Control ID) updated immediately
    // - Cursor position preserved
    // - UndoStack not yet flooded
    assert_eq!(state.summaries[0].message_type, "ADT^A08");
    assert_eq!(state.summaries[0].control_id, "MSG999");
    assert_eq!(state.inline_raw_cursor, Some(10));
    assert!(!state.undo_stack.can_undo());
    assert!(state.pending_inline_raw_edit.is_some());

    // Keystroke 2 & 3 in the same typing burst: append a new NTE segment line
    state.inline_raw_text.push_str("\nNTE|1|L|Live note");
    state.apply_live_inline_raw_edit(&settings);

    // Segment Accordion cache immediately reflects 3 segments
    assert_eq!(state.doc.records[0].segments().len(), 3);
    assert_eq!(state.record_cache.as_ref().unwrap().segments.len(), 3);
    assert_eq!(
        state.record_cache.as_ref().unwrap().segments[2].tag,
        "NTE".to_string()
    );
    // Still coalescing into a single pending edit
    assert!(!state.undo_stack.can_undo());

    // When focus is lost (or debounce timer / Ctrl+Z triggers), the entire typing burst
    // commits as a SINGLE EditRecordCmd on UndoStack.
    let outcome = state.flush_pending_inline_raw_edit(&settings).unwrap();
    assert!(outcome.is_some());
    assert!(state.pending_inline_raw_edit.is_none());
    assert!(state.undo_stack.can_undo());
    assert!(state.undo_stack.is_dirty());

    // A single Undo (Ctrl+Z) reverts both the MSH-9/10 edits AND the appended NTE segment!
    state.undo(&settings).unwrap();
    assert!(!state.undo_stack.can_undo());
    assert_eq!(state.summaries[0].message_type, "ADT^A01");
    assert_eq!(state.summaries[0].control_id, "MSG100");
    assert_eq!(state.doc.records[0].segments().len(), 2);

    // A single Redo (Ctrl+Y) restores the entire coalesced typing burst!
    state.redo(&settings).unwrap();
    assert_eq!(state.summaries[0].message_type, "ADT^A08");
    assert_eq!(state.summaries[0].control_id, "MSG999");
    assert_eq!(state.doc.records[0].segments().len(), 3);
}
