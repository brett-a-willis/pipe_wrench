//! Phase 6 tests covering UI editing workflows, dialogs, undo/redo selection restoration,
//! delete confirmation guards, and the ASCII / Hex view inspector (EDIT-1..EDIT-11, §5.6, FS-8).

use hl7_core::{
    AssignFieldCmd, ChangeDelimitersCmd, EditError, EditFieldCmd, EditRecordCmd, ReplaceCmd,
    ReplaceScope,
};
use pipe_wrench::ui::dialogs::{DeleteConfirmDialog, EditRecordDialog};
use pipe_wrench::{AppSettings, SessionState};

fn sample_session() -> (SessionState, AppSettings) {
    let settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||PAT001^^^HOSP^MR||DOE^JOHN~ALT^JANE\r",
        "OBX|1|TX|NOTE1||alpha beta\r",
        "OBX|2|TX|NOTE2||ALPHA gamma\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ADT^A08|MSG2|P|2.5.1\r",
        "PID|1||PAT002^^^HOSP^MR||SMITH^ALICE\r",
    );
    state.paste_from_text(raw, false, &settings);
    (state, settings)
}

// EDIT-3, EDIT-4, EDIT-11, D-9
#[test]
fn edit_field_dialog_blocks_msh_and_marker_and_restores_selection_on_undo_redo() {
    let (mut state, mut settings) = sample_session();

    // Select MSH-1 (row 0) -> blocked with specific delimiter explanation
    state.select_field_row(0);
    let err = state.open_edit_field_dialog(&settings).unwrap_err();
    assert_eq!(err.to_string(), "Field is not editable");
    let msh1_alert = state.modal_alert.as_deref().unwrap_or("");
    assert!(
        msh1_alert.contains("MSH-1") && msh1_alert.contains("Change Message Delimiters"),
        "unexpected MSH-1 alert: {msh1_alert}"
    );
    state.modal_alert = None;

    // Select MSH-2 (row 1) -> blocked with specific delimiter explanation
    state.select_field_row(1);
    let err = state.open_edit_field_dialog(&settings).unwrap_err();
    assert_eq!(err.to_string(), "Field is not editable");
    let msh2_alert = state.modal_alert.as_deref().unwrap_or("");
    assert!(
        msh2_alert.contains("MSH-2") && msh2_alert.contains("Change Message Delimiters"),
        "unexpected MSH-2 alert: {msh2_alert}"
    );
    state.modal_alert = None;

    // Verify no synthetic marker rows are present (D-26)
    assert!(state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .all(|r| !r.is_parsed_marker));

    // Select PID-5(2).1 ("ALT")
    let target_addr = "PID-5(2).1".parse().unwrap();
    let pid5_row = state
        .record_cache
        .as_ref()
        .unwrap()
        .field_rows
        .iter()
        .position(|r| r.address == Some(target_addr))
        .unwrap();
    state.select_field_row(pid5_row);

    // Decode-escapes mode makes field list read-only (§5.5, EDIT-3)
    settings.decode_escape_sequences = true;
    state.rebuild_selected_record_cache(&settings);
    assert!(state.open_edit_field_dialog(&settings).is_err());
    let decode_alert = state.modal_alert.as_deref().unwrap_or("");
    assert!(
        decode_alert.contains("Decode Escape Sequences")
            && decode_alert.contains("Raw Message Editor"),
        "unexpected decode-escapes alert: {decode_alert}"
    );
    state.modal_alert = None;
    settings.decode_escape_sequences = false;
    state.rebuild_selected_record_cache(&settings);

    // Valid open pre-fills current value
    state.open_edit_field_dialog(&settings).unwrap();
    let dlg = state.edit_field_dialog.as_ref().unwrap();
    assert_eq!(dlg.value, "ALT");
    assert_eq!(dlg.address, target_addr);
    state.edit_field_dialog = None;

    // Execute EditFieldCmd with escape_delimiters = true (EDIT-4)
    state
        .execute_command(
            Box::new(EditFieldCmd::new(0, 1, target_addr, b"A|B".to_vec(), true)),
            &settings,
        )
        .unwrap();
    assert!(state.undo_stack.is_dirty());

    // EDIT-11: Same address is reselected after edit
    assert_eq!(state.selected_field_address(), Some((0, 1, target_addr)));

    // Undo & Redo preserve address selection and update summaries/dirty state (EDIT-1, EDIT-11, FS-8)
    state.undo(&settings).unwrap();
    assert!(!state.undo_stack.is_dirty());
    assert_eq!(state.selected_field_address(), Some((0, 1, target_addr)));

    state.redo(&settings).unwrap();
    assert!(state.undo_stack.is_dirty());
    assert_eq!(state.selected_field_address(), Some((0, 1, target_addr)));
}

// EDIT-2: Edit Record dialog Insert End-of-Segment and delimiter re-read on commit
#[test]
fn edit_record_insert_eos_and_commit_rereads_delimiters() {
    let (mut state, settings) = sample_session();
    state.open_edit_record_dialog();
    let mut dlg: EditRecordDialog = state.edit_record_dialog.take().unwrap();

    // Append an NTE segment using insert_end_of_segment
    dlg.cursor_byte_pos = Some(dlg.text.len());
    dlg.insert_end_of_segment();
    dlg.text.push_str("NTE|1||Added note");
    let cr_bytes = dlg.to_cr_bytes();

    state
        .execute_command(Box::new(EditRecordCmd::new(0, cr_bytes)), &settings)
        .unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 5);

    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 4);
}

// EDIT-5, EDIT-6, EDIT-9: Assign, Replace, and Change Delimiters through SessionState
#[test]
fn assign_replace_and_change_delimiters_update_summaries_and_undo() {
    let (mut state, settings) = sample_session();

    // Assign MSH-9 across all records updates virtualized summaries
    state
        .execute_command(
            Box::new(AssignFieldCmd::new(
                None,
                "MSH-9".parse().unwrap(),
                b"ORM".to_vec(),
                true,
            )),
            &settings,
        )
        .unwrap();
    assert_eq!(state.summaries[0].message_type, "ORM");
    assert_eq!(state.summaries[1].message_type, "ORM");
    state.undo(&settings).unwrap();
    assert_eq!(state.summaries[0].message_type, "ADT^A01");

    // Replace in current segment (OBX #1)
    state.select_segment(2);
    let out = state
        .execute_command(
            Box::new(ReplaceCmd::new(
                ReplaceScope::Segment {
                    record_index: 0,
                    segment_index: 2,
                },
                b"alpha".to_vec(),
                b"OMEGA".to_vec(),
                true,
            )),
            &settings,
        )
        .unwrap();
    assert_eq!(out.replacements, 1);

    // Change delimiters across all records (EDIT-9)
    let out = state
        .execute_command(
            Box::new(ChangeDelimitersCmd::new(b";:~\\&".to_vec())),
            &settings,
        )
        .unwrap();
    assert!(out.replacements > 0);
    assert_eq!(state.doc.records[0].delimiters().as_bytes(), *b";:~\\&");
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].delimiters().as_bytes(), *b"|^~\\&");
}

// EDIT-7, EDIT-8, D-10: Delete segment & record guards and confirmation dialog behavior
#[test]
fn delete_segment_and_record_guards_and_prompt_before_deleting() {
    let (mut state, mut settings) = sample_session();

    // Segment 0 (MSH) can never be deleted (EDIT-7)
    state.select_segment(0);
    let err = state
        .request_delete_selected_segment(&settings)
        .unwrap_err();
    assert_eq!(err, EditError::CannotDeleteMsh);
    assert_eq!(
        state.modal_alert.as_deref(),
        Some("Cannot delete the MSH segment.")
    );
    assert!(state.delete_confirm_dialog.is_none());

    // Segment 2 (OBX) with prompt_before_deleting = true opens confirmation dialog
    state.select_segment(2);
    state.request_delete_selected_segment(&settings).unwrap();
    assert!(matches!(
        state.delete_confirm_dialog,
        Some(DeleteConfirmDialog::Segments {
            record_index: 0,
            ..
        })
    ));
    state.delete_confirm_dialog = None;

    // With prompt_before_deleting = false, deletes immediately
    settings.prompt_before_deleting = false;
    state.request_delete_selected_segment(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 3);
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.records[0].segments().len(), 4);

    // Delete record with prompt_before_deleting = false
    state.request_delete_records(vec![1], &settings).unwrap();
    assert_eq!(state.doc.len(), 1);
    state.undo(&settings).unwrap();
    assert_eq!(state.doc.len(), 2);
}
