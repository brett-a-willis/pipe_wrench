//! Phase 15 integration tests covering:
//! 1. Global `Ctrl+V` clipboard paste (`paste_records_undoable`) for single & multi-message
//!    HL7 batches (with `UndoStack` undo/redo) and non-destructive rejection of invalid clipboard text.
//! 2. Bottom Record List CRUD & Reorder actions (`+` Insert New Scaffold Record, `↑` Move Record Up,
//!    `↓` Move Record Down, Drag-and-Drop Row Reorder, and `-` Delete Selected Record(s)),
//!    verifying full `UndoStack` undo and redo across every operation.

use pipe_wrench::{AppSettings, SessionState};

#[test]
fn global_clipboard_paste_undoable_and_rejects_invalid_text() {
    let settings = AppSettings::default();
    let mut state = SessionState::new();

    // 1. Invalid / empty clipboard text returns Err without mutating loaded records
    assert!(state.paste_records_undoable("", &settings).is_err());
    assert!(state
        .paste_records_undoable("Just some random non-HL7 clipboard text", &settings)
        .is_err());
    assert!(state.doc.is_empty());
    assert!(state.status_message.contains("No valid HL7 messages found"));

    // 2. Paste single valid HL7 message into empty session
    let msg1 =
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|CLIP1|P|2.5.1\rPID|1||P1||DOE^JOHN\r";
    let count1 = state.paste_records_undoable(msg1, &settings).unwrap();
    assert_eq!(count1, 1);
    assert_eq!(state.doc.len(), 1);
    assert_eq!(state.selected_record, Some(0));
    assert_eq!(state.summaries[0].control_id, "CLIP1");
    assert!(state.undo_stack.can_undo());

    // 3. Paste multi-message batch (with CRLF + MLLP framing) after selection -> inserts 2 more records
    let batch = "\x0BMSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ORU^R01|CLIP2|P|2.5.1\r\nOBX|1|NM|GLU||110\r\n\x1C\r\nMSH|^~\\&|SND|FAC|RCV|FAC|20260115120002||ORM^O01|CLIP3|P|2.5.1\r\nORC|NW|ORD1\r\n";
    let count2 = state.paste_records_undoable(batch, &settings).unwrap();
    assert_eq!(count2, 2);
    assert_eq!(state.doc.len(), 3);
    assert_eq!(state.selected_record, Some(1)); // selects first newly pasted record
    assert_eq!(state.summaries[1].control_id, "CLIP2");
    assert_eq!(state.summaries[2].control_id, "CLIP3");

    // Invalid paste now leaves the 3 loaded records completely untouched
    assert!(state.paste_records_undoable("not hl7", &settings).is_err());
    assert_eq!(state.doc.len(), 3);

    // 4. Undo removes the 2 pasted batch records cleanly; Redo restores them
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.len(), 1);
    assert_eq!(state.summaries[0].control_id, "CLIP1");

    state.redo(&settings).unwrap();
    assert_eq!(state.doc.len(), 3);
    assert_eq!(state.summaries[1].control_id, "CLIP2");
    assert_eq!(state.summaries[2].control_id, "CLIP3");
}

#[test]
fn record_list_crud_insert_move_drag_reorder_delete_and_undo_redo() {
    let settings = AppSettings {
        prompt_before_deleting: false,
        ..AppSettings::default()
    };
    let mut state = SessionState::new();

    // 1. Insert `+` into an empty list creates record at index 0
    let idx0 = state.insert_new_scaffold_record(&settings).unwrap();
    assert_eq!(idx0, 0);
    assert_eq!(state.doc.len(), 1);
    assert_eq!(state.selected_record, Some(0));
    assert_eq!(state.summaries[0].message_type, "ADT^A01");
    assert_eq!(state.summaries[0].control_id, "NEW0001");

    // 2. Insert `+` below index 0 creates NEW0002 at index 1, then NEW0003 at index 2
    let idx1 = state.insert_new_scaffold_record(&settings).unwrap();
    assert_eq!(idx1, 1);
    assert_eq!(state.summaries[1].control_id, "NEW0002");

    let idx2 = state.insert_new_scaffold_record(&settings).unwrap();
    assert_eq!(idx2, 2);
    assert_eq!(state.summaries[2].control_id, "NEW0003");
    assert_eq!(state.doc.len(), 3);

    // Order is [NEW0001, NEW0002, NEW0003], selected = 2 (NEW0003)
    assert!(state.can_move_selected_records_up());
    assert!(!state.can_move_selected_records_down());

    // 3. Move Up (`↑`): NEW0003 moves from index 2 to index 1 -> [NEW0001, NEW0003, NEW0002]
    assert!(state.move_selected_records_up(&settings).unwrap());
    assert_eq!(state.selected_record, Some(1));
    assert_eq!(state.summaries[0].control_id, "NEW0001");
    assert_eq!(state.summaries[1].control_id, "NEW0003");
    assert_eq!(state.summaries[2].control_id, "NEW0002");

    // 4. Move Down (`↓`): NEW0003 moves back to index 2 -> [NEW0001, NEW0002, NEW0003]
    assert!(state.move_selected_records_down(&settings).unwrap());
    assert_eq!(state.selected_record, Some(2));
    assert_eq!(state.summaries[2].control_id, "NEW0003");

    // Undo Move Down -> [NEW0001, NEW0003, NEW0002]
    state.undo(&settings).unwrap();
    assert_eq!(state.summaries[1].control_id, "NEW0003");
    // Redo Move Down -> [NEW0001, NEW0002, NEW0003]
    state.redo(&settings).unwrap();
    assert_eq!(state.summaries[2].control_id, "NEW0003");

    // 5. Drag-and-drop reorder: drag index 2 (NEW0003) to index 0 -> [NEW0003, NEW0001, NEW0002]
    assert!(state.reorder_record_drag(2, 0, &settings).unwrap());
    assert_eq!(state.selected_record, Some(0));
    assert_eq!(state.summaries[0].control_id, "NEW0003");
    assert_eq!(state.summaries[1].control_id, "NEW0001");
    assert_eq!(state.summaries[2].control_id, "NEW0002");

    // Undo Drag-and-drop -> [NEW0001, NEW0002, NEW0003]
    state.undo(&settings).unwrap();
    assert_eq!(state.summaries[0].control_id, "NEW0001");
    assert_eq!(state.summaries[2].control_id, "NEW0003");

    // 6. Delete Selected (`-`): delete index 1 (NEW0002), verify Undo & Redo
    state.select_record(1, false, &settings);
    let to_del: Vec<usize> = state.selected_records.iter().copied().collect();
    state.request_delete_records(to_del, &settings).unwrap();
    assert_eq!(state.doc.len(), 2);
    assert_eq!(state.summaries[0].control_id, "NEW0001");
    assert_eq!(state.summaries[1].control_id, "NEW0003");

    state.undo(&settings).unwrap();
    assert_eq!(state.doc.len(), 3);
    assert_eq!(state.summaries[1].control_id, "NEW0002");

    state.redo(&settings).unwrap();
    assert_eq!(state.doc.len(), 2);
}

#[test]
fn multi_record_copy_round_trips_with_undoable_paste() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG_A|P|2.5.1\rPID|1||P1||ALPHA^ONE\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ORU^R01|MSG_B|P|2.5.1\rPID|1||P2||BETA^TWO\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120002||ORM^O01|MSG_C|P|2.5.1\rPID|1||P3||GAMMA^THREE\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);
    assert_eq!(state.doc.len(), 3);

    // Select records 0 and 2 (Ctrl+Click non-contiguous multi-select)
    state.select_record_with_modifiers(0, false, false, &settings);
    state.select_record_with_modifiers(2, true, false, &settings);
    assert_eq!(state.selected_records.len(), 2);

    // Copy Selected Record(s) (`⧉` / `Ctrl+C`) preserves document order (MSG_A then MSG_C)
    let copied = state.format_selected_records_for_clipboard(&settings);
    let pos_a = copied.find("MSG_A").expect("MSG_A copied");
    let pos_c = copied.find("MSG_C").expect("MSG_C copied");
    assert!(pos_a < pos_c);
    assert!(!copied.contains("MSG_B"));

    // Paste back via global `Ctrl+V` (`paste_records_undoable`) -> inserts 2 copies after active selection (index 2)
    let pasted_count = state.paste_records_undoable(&copied, &settings).unwrap();
    assert_eq!(pasted_count, 2);
    assert_eq!(state.doc.len(), 5);
    assert_eq!(state.summaries[3].control_id, "MSG_A");
    assert_eq!(state.summaries[4].control_id, "MSG_C");

    // Undo cleanly removes both pasted copies; Redo restores them
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.len(), 3);
    state.redo(&settings).unwrap();
    assert_eq!(state.doc.len(), 5);
}

#[test]
fn message_type_pill_badges_are_family_consistent_and_trigger_distinct() {
    use pipe_wrench::ui::theme::message_type_pill_colors;

    for is_dark in [true, false] {
        let adt_a01 = message_type_pill_colors("ADT^A01", is_dark);
        let adt_a03 = message_type_pill_colors("ADT^A03", is_dark);
        let adt_a04 = message_type_pill_colors("ADT^A04", is_dark);
        let adt_a08 = message_type_pill_colors("ADT^A08", is_dark);
        assert_ne!(adt_a01, adt_a03);
        assert_ne!(adt_a01, adt_a04);
        assert_ne!(adt_a01, adt_a08);
        assert_ne!(adt_a03, adt_a08);

        let oru_r01 = message_type_pill_colors("ORU^R01", is_dark);
        let oru_r03 = message_type_pill_colors("ORU^R03", is_dark);
        assert_ne!(oru_r01, oru_r03);

        let orm = message_type_pill_colors("ORM^O01", is_dark);
        let siu = message_type_pill_colors("SIU^S12", is_dark);
        let dft = message_type_pill_colors("DFT^P03", is_dark);
        let mdm = message_type_pill_colors("MDM^T02", is_dark);
        let fhs = message_type_pill_colors("FHS", is_dark);
        assert_ne!(adt_a01.bg, oru_r01.bg);
        assert_ne!(orm.bg, siu.bg);
        assert_ne!(dft.bg, mdm.bg);
        assert_ne!(fhs.bg, adt_a01.bg);
    }
}

#[test]
fn custom_component_delimiter_message_type_pill_colors_match_standard() {
    use pipe_wrench::ui::theme::{message_type_pill_colors, message_type_pill_colors_with_delim};

    let settings = AppSettings::default();
    let mut state = SessionState::default();
    let batch = concat!(
        "MSH|^~\\&|SEND|FAC|RECV|FAC|20260115120000||ADT^A01|STD001|P|2.5.1\r",
        "PID|1||P1||DOE^JANE\r",
        "MSH|:~\\&|SEND|FAC|RECV|FAC|20260115120100||ADT:A01|ALT001|P|2.5.1\r",
        "PID|1||P2||DOE:JOHN\r",
        "MSH|:~\\&|SEND|FAC|RECV|FAC|20260115120200||ADT:A03|ALT003|P|2.5.1\r",
        "PID|1||P3||SMITH:ALICE\r",
    );
    let pasted = state.paste_records_undoable(batch, &settings).unwrap();
    assert_eq!(pasted, 3);

    // Displayed badge text remains faithful to each message's raw MSH-9
    assert_eq!(state.summaries[0].message_type, "ADT^A01");
    assert_eq!(state.summaries[1].message_type, "ADT:A01");
    assert_eq!(state.summaries[2].message_type, "ADT:A03");

    let std_comp = char::from(state.doc.records[0].delimiters().component);
    let alt_comp_1 = char::from(state.doc.records[1].delimiters().component);
    let alt_comp_3 = char::from(state.doc.records[2].delimiters().component);
    assert_eq!(std_comp, '^');
    assert_eq!(alt_comp_1, ':');
    assert_eq!(alt_comp_3, ':');

    for is_dark in [true, false] {
        let std_a01 = message_type_pill_colors_with_delim(
            &state.summaries[0].message_type,
            std_comp,
            is_dark,
        );
        let alt_a01 = message_type_pill_colors_with_delim(
            &state.summaries[1].message_type,
            alt_comp_1,
            is_dark,
        );
        let alt_a03 = message_type_pill_colors_with_delim(
            &state.summaries[2].message_type,
            alt_comp_3,
            is_dark,
        );
        let std_a03 = message_type_pill_colors("ADT^A03", is_dark);

        assert_eq!(alt_a01, std_a01);
        assert_eq!(alt_a03, std_a03);
        assert_ne!(alt_a01, alt_a03);
    }
}

#[test]
fn multi_row_drag_reorders_contiguous_and_non_contiguous_selections_atomically() {
    let settings = AppSettings::default();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG_A|P|2.5.1\rPID|1||P1\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ADT^A01|MSG_B|P|2.5.1\rPID|1||P2\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120002||ADT^A01|MSG_C|P|2.5.1\rPID|1||P3\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120003||ADT^A01|MSG_D|P|2.5.1\rPID|1||P4\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120004||ADT^A01|MSG_E|P|2.5.1\rPID|1||P5\r",
    );
    let mut state = SessionState::new();
    state.paste_from_text(raw, false, &settings);
    fn ids(s: &SessionState) -> Vec<&str> {
        s.summaries.iter().map(|m| m.control_id.as_str()).collect()
    }
    assert_eq!(
        ids(&state),
        vec!["MSG_A", "MSG_B", "MSG_C", "MSG_D", "MSG_E"]
    );

    // 1. Contiguous multi-select [0, 1] (MSG_A, MSG_B), drag row 1 down onto row 3 (MSG_D)
    //    -> drops after MSG_D: [MSG_C, MSG_D, MSG_A, MSG_B, MSG_E] with [2, 3] selected
    state.select_record_with_modifiers(0, false, false, &settings);
    state.select_record_with_modifiers(1, false, true, &settings);
    assert!(state.reorder_record_drag(1, 3, &settings).unwrap());
    assert_eq!(
        ids(&state),
        vec!["MSG_C", "MSG_D", "MSG_A", "MSG_B", "MSG_E"]
    );
    assert_eq!(
        state.selected_records.iter().copied().collect::<Vec<_>>(),
        vec![2, 3]
    );

    // Undo restores original order in a single step
    state.undo(&settings).unwrap();
    assert_eq!(
        ids(&state),
        vec!["MSG_A", "MSG_B", "MSG_C", "MSG_D", "MSG_E"]
    );

    // 2. Non-contiguous multi-select [0, 2] (MSG_A, MSG_C), drag row 0 down onto row 3 (MSG_D)
    //    -> moves MSG_A, MSG_C together in relative order after MSG_D: [MSG_B, MSG_D, MSG_A, MSG_C, MSG_E]
    state.select_record_with_modifiers(0, false, false, &settings);
    state.select_record_with_modifiers(2, true, false, &settings);
    assert!(state.reorder_record_drag(0, 3, &settings).unwrap());
    assert_eq!(
        ids(&state),
        vec!["MSG_B", "MSG_D", "MSG_A", "MSG_C", "MSG_E"]
    );
    assert_eq!(
        state.selected_records.iter().copied().collect::<Vec<_>>(),
        vec![2, 3]
    );

    // 3. Drag upward: move selected [2, 3] (MSG_A, MSG_C) onto row 0 (MSG_B)
    //    -> drops before MSG_B: [MSG_A, MSG_C, MSG_B, MSG_D, MSG_E] with [0, 1] selected
    assert!(state.reorder_record_drag(3, 0, &settings).unwrap());
    assert_eq!(
        ids(&state),
        vec!["MSG_A", "MSG_C", "MSG_B", "MSG_D", "MSG_E"]
    );
    assert_eq!(
        state.selected_records.iter().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );

    // 4. Dragging an unselected row (4 = MSG_E) moves only that single row
    assert!(state.reorder_record_drag(4, 1, &settings).unwrap());
    assert_eq!(
        ids(&state),
        vec!["MSG_A", "MSG_E", "MSG_C", "MSG_B", "MSG_D"]
    );
    assert_eq!(
        state.selected_records.iter().copied().collect::<Vec<_>>(),
        vec![1]
    );
}
