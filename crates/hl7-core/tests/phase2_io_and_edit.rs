//! Phase 2 tests covering file I/O (FO-1..FO-5, FO-13..FO-15, FS-1..FS-8)
//! and edit commands + undo/redo stack (EDIT-1..EDIT-11, NFR-3..NFR-5).

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use flate2::write::GzEncoder;
use flate2::Compression;
use hl7_core::{
    list_zip_entries, load_file, load_files, load_from_bytes, load_zip_entries,
    save_records_atomic, save_selected_records_atomic, AssignFieldCmd, ChangeDelimitersCmd,
    DeleteRecordsCmd, DeleteSegmentsCmd, Document, EditError, EditFieldCmd, EditRecordCmd,
    FileFilterList, IoError, MllpEnvelope, ReplaceCmd, ReplaceScope, SaveOptions, SaveTerminator,
    ScannerConfig, SourceTerminator, UndoStack,
};
use proptest::prelude::*;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn sample_doc() -> Document {
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||PAT001^^^HOSP^MR||ALPHA^ONE~ALT^TWO\r",
        "OBX|1|TX|CODE1||hello world\r",
        "OBX|2|TX|CODE2||HELLO again\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120001||ADT^A08|MSG2|P|2.5.1\r",
        "PID|1||PAT002^^^HOSP^MR||BETA^ONE\r",
    );
    let out = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
    Document::from_records(out.records)
}

// FO-1, FO-2, FS-3
#[test]
fn file_filter_list_defaults_unlimited_and_rejects_duplicates() {
    let mut filters = FileFilterList::default();
    assert_eq!(filters.patterns(), &["*.*", "*.dat", "*.txt", "*.hl7"]);
    assert_eq!(filters.active_pattern(), "*.*");

    // Duplicate rejected (case-insensitive)
    assert!(matches!(
        filters.add_pattern("*.HL7"),
        Err(IoError::InvalidFilterPattern { .. })
    ));

    // Unlimited patterns (> 10, FO-2)
    for i in 0..15 {
        filters.add_pattern(format!("*.ext{i}")).unwrap();
    }
    assert_eq!(filters.patterns().len(), 19);
}

// FO-3, FO-13, FO-14, FS-1, FS-4, FS-5, FS-7, D-2
#[test]
fn multi_file_lexicographic_load_and_atomic_save_options() {
    let dir = TempDir::new().unwrap();
    let f_b = dir.path().join("b_second.hl7");
    let f_a = dir.path().join("a_first.hl7");

    // a_first uses CR, b_second uses LF
    std::fs::write(&f_a, b"MSH|^~\\&|A|F|R|F|20260101||ADT^A01|ID_A|P|2.5.1\r").unwrap();
    std::fs::write(&f_b, b"MSH|^~\\&|B|F|R|F|20260102||ADT^A08|ID_B|P|2.5.1\n").unwrap();

    let cancel = AtomicBool::new(false);
    let mut progress_updates = Vec::new();
    let loaded = load_files(
        &[f_b.clone(), f_a.clone()],
        ScannerConfig::default(),
        &cancel,
        |p| progress_updates.push(p.files_done),
    )
    .unwrap();

    assert_eq!(progress_updates, vec![1, 2]);
    assert_eq!(loaded.loaded_paths, vec![f_a, f_b]);
    assert_eq!(loaded.records.len(), 2);
    assert_eq!(loaded.records[0].summary().control_id, "ID_A");
    assert_eq!(loaded.records[0].source_terminator(), SourceTerminator::Cr);
    assert_eq!(loaded.records[1].summary().control_id, "ID_B");
    assert_eq!(loaded.records[1].source_terminator(), SourceTerminator::Lf);

    // Atomic save with Preserve + Disk Envelope (FS-4, FS-5, FS-7)
    let out_path = dir.path().join("saved_env.dat");
    save_records_atomic(
        &out_path,
        &loaded.records,
        SaveOptions {
            term_mode: SaveTerminator::Preserve,
            disk_envelope: Some(MllpEnvelope::DEFAULT),
        },
    )
    .unwrap();
    let saved_bytes = std::fs::read(&out_path).unwrap();
    assert!(saved_bytes.starts_with(&[0x0B]));
    assert!(saved_bytes.ends_with(&[0x1C, 0x0D]));

    // Save selected record only with forced CRLF (FS-1, FS-5)
    let sel_path = dir.path().join("selected.hl7");
    save_selected_records_atomic(
        &sel_path,
        &loaded.records,
        &[1],
        SaveOptions {
            term_mode: SaveTerminator::CrLf,
            disk_envelope: None,
        },
    )
    .unwrap();
    let sel_bytes = std::fs::read(&sel_path).unwrap();
    assert_eq!(
        sel_bytes,
        b"MSH|^~\\&|B|F|R|F|20260102||ADT^A08|ID_B|P|2.5.1\r\n"
    );
}

// FO-4, FO-5, FO-15, D-5, D-6
#[test]
fn transparent_gz_zip_decompression_z_rejection_and_clipboard_preserve_crlf() {
    let dir = TempDir::new().unwrap();
    let cancel = AtomicBool::new(false);

    // .gz
    let gz_path = dir.path().join("msg.hl7.gz");
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(b"MSH|^~\\&|GZ|F|R|F|20260101||ADT^A01|GZ1|P|2.5.1\rPID|1||P1\r")
        .unwrap();
    std::fs::write(&gz_path, enc.finish().unwrap()).unwrap();
    let gz_loaded = load_file(&gz_path, ScannerConfig::default(), &cancel).unwrap();
    assert_eq!(gz_loaded.records.len(), 1);
    assert_eq!(gz_loaded.records[0].summary().control_id, "GZ1");

    // .zip with 2 entries
    let zip_path = dir.path().join("bundle.zip");
    {
        let f = std::fs::File::create(&zip_path).unwrap();
        let mut zw = ZipWriter::new(f);
        zw.start_file("first.hl7", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"MSH|^~\\&|Z1|F|R|F|20260101||ADT^A01|ZIP1|P|2.5.1\r")
            .unwrap();
        zw.start_file("second.hl7", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"MSH|^~\\&|Z2|F|R|F|20260102||ADT^A08|ZIP2|P|2.5.1\r")
            .unwrap();
        zw.finish().unwrap();
    }
    let entries = list_zip_entries(&zip_path).unwrap();
    assert_eq!(entries, vec!["first.hl7", "second.hl7"]);
    // Default load_file reads first entry (FO-4)
    let first_only = load_file(&zip_path, ScannerConfig::default(), &cancel).unwrap();
    assert_eq!(first_only.records.len(), 1);
    assert_eq!(first_only.records[0].summary().control_id, "ZIP1");
    // Explicit multi-entry load reads both (D-5)
    let both =
        load_zip_entries(&zip_path, Some(&entries), ScannerConfig::default(), &cancel).unwrap();
    assert_eq!(both.records.len(), 2);

    // .Z rejected with clear message (FO-5, D-6)
    let z_path = dir.path().join("legacy.hl7.Z");
    std::fs::write(&z_path, [0x1F, 0x9D, 0x90, 0x00]).unwrap();
    assert!(matches!(
        load_file(&z_path, ScannerConfig::default(), &cancel),
        Err(IoError::UnsupportedCompressFormat { .. })
    ));

    // Clipboard paste -> Unknown terminator -> Preserve writes CRLF (FO-15, FS-5)
    let pasted = load_from_bytes(
        b"MSH|^~\\&|C|F|R|F|20260101||ADT^A01|CLIP1|P|2.5.1\r",
        ScannerConfig::default(),
    )
    .unwrap();
    assert_eq!(
        pasted.records[0].source_terminator(),
        SourceTerminator::Unknown
    );
    let mut out = Vec::new();
    pasted.records[0]
        .write_to(&mut out, SaveTerminator::Preserve, None)
        .unwrap();
    assert!(out.ends_with(b"\r\n"));
}

// EDIT-1, EDIT-2, EDIT-3, EDIT-4, EDIT-11, FS-8, D-9, D-12
#[test]
fn edit_field_record_validation_escape_undo_redo_and_dirty_tracking() {
    let mut doc = sample_doc();
    let mut stack = UndoStack::new(64); // tiny threshold to test soft warning (D-12)
    assert!(!stack.is_dirty());

    // MSH-1/2 blocked (EDIT-3, D-9)
    let err = stack
        .execute(
            Box::new(EditFieldCmd::new(
                0,
                0,
                "MSH-2".parse().unwrap(),
                b"xxxx".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap_err();
    assert_eq!(err.to_string(), "Field is not editable");
    assert!(!stack.is_dirty());

    // Active delimiter rejected unless escape_delimiters = true (EDIT-4)
    let err = stack
        .execute(
            Box::new(EditFieldCmd::new(
                0,
                1,
                "PID-5(2)-1".parse().unwrap(),
                b"BAD|VAL".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap_err();
    assert!(matches!(err, EditError::DelimiterInValue { .. }));

    // Accepted with escape_delimiters = true (EDIT-4)
    let orig_bytes = doc.records[0].bytes().to_vec();
    let (outcome, warning) = stack
        .execute(
            Box::new(EditFieldCmd::new(
                0,
                1,
                "PID-5(2)-1".parse().unwrap(),
                b"GOOD|VAL".to_vec(),
                true,
            )),
            &mut doc,
        )
        .unwrap();
    assert!(warning.is_some()); // crossed 64-byte soft threshold
    assert!(stack.is_dirty());
    assert_eq!(
        outcome.selection_hint.unwrap().address.unwrap().to_string(),
        "PID-5(2).1"
    );
    assert_eq!(
        doc.records[0].field_bytes(1, &"PID-5(2)-1".parse().unwrap()),
        Some(&b"GOOD\\F\\VAL"[..])
    );

    // Undo restores exact original bytes and clears dirty flag (EDIT-1, FS-8)
    stack.undo(&mut doc).unwrap();
    assert_eq!(doc.records[0].bytes(), orig_bytes.as_slice());
    assert!(!stack.is_dirty());

    // Redo re-applies and marks dirty again
    stack.redo(&mut doc).unwrap();
    assert_eq!(
        doc.records[0].field_bytes(1, &"PID-5(2)-1".parse().unwrap()),
        Some(&b"GOOD\\F\\VAL"[..])
    );
    assert!(stack.is_dirty());

    // EditRecordCmd re-reads delimiters on commit (EDIT-2)
    stack
        .execute(
            Box::new(EditRecordCmd::new(
                1,
                b"MSH;:~\\&;S;F;R;F;20260101;;ADT:A08;NEW2;P;2.5.1\rPID;1;;PAT002".to_vec(),
            )),
            &mut doc,
        )
        .unwrap();
    assert_eq!(doc.records[1].delimiters().as_bytes(), *b";:~\\&");
    stack.undo(&mut doc).unwrap();
    assert_eq!(doc.records[1].delimiters().as_bytes(), *b"|^~\\&");
}

// EDIT-5, EDIT-6, D-8
#[test]
fn assign_field_and_replace_all_scopes_with_delimiter_validation() {
    let mut doc = sample_doc();
    let mut stack = UndoStack::default();

    // Hierarchy-aware delimiter validation:
    // - Field-level (MSH-9 / OBX-5) allows composite `^` / `~` / `&` / `\`, rejects `|` and line breaks.
    // - Component-level (MSH-9.1 / OBX-5.1) rejects `|`, `~`, `^`, and line breaks.
    assert!(matches!(
        stack.execute(
            Box::new(AssignFieldCmd::new(
                None,
                "OBX-5".parse().unwrap(),
                b"X|Y".to_vec(),
                false,
            )),
            &mut doc,
        ),
        Err(EditError::DelimiterInValue { .. })
    ));
    assert!(matches!(
        stack.execute(
            Box::new(AssignFieldCmd::new(
                None,
                "OBX-5.1".parse().unwrap(),
                b"X^Y".to_vec(),
                false,
            )),
            &mut doc,
        ),
        Err(EditError::DelimiterInValue { .. })
    ));

    // Composite edit on whole field MSH-9 (ADT^A01 -> ADT^A04) succeeds cleanly
    stack
        .execute(
            Box::new(EditFieldCmd::new(
                0,
                0,
                "MSH-9".parse().unwrap(),
                b"ADT^A04".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap();
    assert_eq!(
        doc.records[0].field_bytes(0, &"MSH-9".parse().unwrap()),
        Some(&b"ADT^A04"[..])
    );
    assert_eq!(
        doc.records[0].field_bytes(0, &"MSH-9.2".parse().unwrap()),
        Some(&b"A04"[..])
    );
    // Rejects `^` on component MSH-9.1
    let comp_err = stack
        .execute(
            Box::new(EditFieldCmd::new(
                0,
                0,
                "MSH-9.1".parse().unwrap(),
                b"ADT^BAD".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap_err();
    assert!(matches!(comp_err, EditError::DelimiterInValue { .. }));
    assert!(comp_err.to_string().contains("Component values"));
    stack.undo(&mut doc).unwrap();

    // Assign across all occurrences of OBX (D-8)
    let (res, _) = stack
        .execute(
            Box::new(AssignFieldCmd::new(
                None,
                "OBX-5".parse().unwrap(),
                b"UPDATED".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap();
    assert_eq!(res.replacements, 2);
    assert_eq!(
        doc.records[0].field_bytes(2, &"OBX-5".parse().unwrap()),
        Some(&b"UPDATED"[..])
    );
    assert_eq!(
        doc.records[0].field_bytes(3, &"OBX-5".parse().unwrap()),
        Some(&b"UPDATED"[..])
    );
    stack.undo(&mut doc).unwrap();

    // Replace case-insensitive across record list (EDIT-6)
    let (res, _) = stack
        .execute(
            Box::new(ReplaceCmd::new(
                ReplaceScope::RecordList {
                    record_indices: None,
                },
                b"hello".to_vec(),
                b"HI".to_vec(),
                false,
            )),
            &mut doc,
        )
        .unwrap();
    assert_eq!(res.replacements, 2);
    assert_eq!(
        doc.records[0].field_bytes(2, &"OBX-5".parse().unwrap()),
        Some(&b"HI world"[..])
    );
    assert_eq!(
        doc.records[0].field_bytes(3, &"OBX-5".parse().unwrap()),
        Some(&b"HI again"[..])
    );
    stack.undo(&mut doc).unwrap();
    assert_eq!(
        doc.records[0].field_bytes(2, &"OBX-5".parse().unwrap()),
        Some(&b"hello world"[..])
    );
}

// EDIT-7, EDIT-8, EDIT-9, D-10, D-11
#[test]
fn delete_segments_delete_records_and_change_delimiters() {
    let mut doc = sample_doc();
    let mut stack = UndoStack::default();

    // Cannot delete MSH (EDIT-7)
    let err = stack
        .execute(Box::new(DeleteSegmentsCmd::new(0, vec![0])), &mut doc)
        .unwrap_err();
    assert_eq!(err.to_string(), "Cannot delete the MSH segment.");

    // Delete OBX segments and undo
    let orig_rec0 = doc.records[0].bytes().to_vec();
    stack
        .execute(Box::new(DeleteSegmentsCmd::new(0, vec![2, 3])), &mut doc)
        .unwrap();
    assert_eq!(doc.records[0].segments().len(), 2);
    stack.undo(&mut doc).unwrap();
    assert_eq!(doc.records[0].bytes(), orig_rec0.as_slice());

    // Delete records through_last and undo (EDIT-8)
    stack
        .execute(
            Box::new(DeleteRecordsCmd::through_last(1, doc.len())),
            &mut doc,
        )
        .unwrap();
    assert_eq!(doc.len(), 1);
    stack.undo(&mut doc).unwrap();
    assert_eq!(doc.len(), 2);

    // ChangeDelimitersCmd validates length 5 (EDIT-9) and uniqueness (D-11)
    let err = stack
        .execute(
            Box::new(ChangeDelimitersCmd::new(b"1234".to_vec())),
            &mut doc,
        )
        .unwrap_err();
    assert_eq!(err.to_string(), "Enter exactly five delimiter characters.");

    let (res, _) = stack
        .execute(
            Box::new(ChangeDelimitersCmd::new(b";:~\\&".to_vec())),
            &mut doc,
        )
        .unwrap();
    assert!(res.replacements > 0);
    assert_eq!(doc.records[0].delimiters().as_bytes(), *b";:~\\&");
    assert_eq!(
        doc.records[0].field_bytes(1, &"PID-5(1)-1".parse().unwrap()),
        Some(&b"ALPHA"[..])
    );

    stack.undo(&mut doc).unwrap();
    assert_eq!(doc.records[0].bytes(), orig_rec0.as_slice());
    assert_eq!(doc.records[0].delimiters().as_bytes(), *b"|^~\\&");
}

// NFR-3, EDIT-1: Property test asserting edit -> undo restores exact original bytes.
proptest! {
    #[test]
    fn edit_field_and_undo_restores_exact_bytes(
        val in "[A-Za-z0-9 _.-]{0,32}",
    ) {
        let mut doc = sample_doc();
        let orig = doc.records[0].bytes().to_vec();
        let mut stack = UndoStack::default();

        let cmd = EditFieldCmd::new(
            0,
            1,
            "PID-5(1)-2".parse().unwrap(),
            val.into_bytes(),
            false,
        );
        stack.execute(Box::new(cmd), &mut doc).unwrap();
        stack.undo(&mut doc).unwrap();

        prop_assert_eq!(doc.records[0].bytes(), orig.as_slice());
        prop_assert!(!stack.is_dirty());
        let _ = PathBuf::new();
    }
}
