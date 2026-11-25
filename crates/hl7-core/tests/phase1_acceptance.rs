//! Phase 1 acceptance and property-based tests for `hl7-core`.
//!
//! Covers: G-1..G-3, G-7, FO-6..FO-11, FO-14, SCAN-3, §5.2, NFR-3..NFR-5.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use hl7_core::{
    load_records_from_reader, Delimiters, FieldAddress, FieldDisplayOptions, MllpEnvelope,
    RecordBoundaryScanner, RecordKind, RecoveryMode, SaveTerminator, ScanEvent, ScannerConfig,
    SourceTerminator,
};
use proptest::prelude::*;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).expect("fixture must exist")
}

fn load_bytes(bytes: &[u8], mode: RecoveryMode) -> hl7_core::LoadOutput {
    let cancel = AtomicBool::new(false);
    let config = ScannerConfig {
        recovery_mode: mode,
        ..ScannerConfig::default()
    };
    load_records_from_reader(BufReader::new(bytes), config, &cancel).expect("scan must succeed")
}

// G-1, G-3, FO-6, FO-14, FS-5, D-1
#[test]
fn uniform_fixtures_round_trip_byte_identically() {
    let uniform_fixtures = [
        ("adt_a01_basic.hl7", SourceTerminator::Cr, None),
        ("adt_a01_crlf.hl7", SourceTerminator::CrLf, None),
        ("adt_a01_lf.hl7", SourceTerminator::Lf, None),
        ("batch_fhs_bhs.hl7", SourceTerminator::Cr, None),
        (
            "mllp_framed.dat",
            SourceTerminator::Cr,
            Some(MllpEnvelope::DEFAULT),
        ),
        ("delims_alt.hl7", SourceTerminator::Cr, None),
        ("escapes.hl7", SourceTerminator::Cr, None),
        ("repeats_components.hl7", SourceTerminator::Cr, None),
        ("utf8_msh18.hl7", SourceTerminator::Cr, None),
        ("empty_fields.hl7", SourceTerminator::Cr, None),
    ];

    for (name, expected_term, envelope) in uniform_fixtures {
        let original = read_fixture(name);
        let loaded = load_bytes(&original, RecoveryMode::Strict);
        assert!(
            !loaded.aborted_on_non_hl7,
            "{name} should not contain non-HL7 data"
        );
        assert_eq!(
            loaded.summary.detected_terminator, expected_term,
            "terminator mismatch for {name}"
        );

        let mut serialized = Vec::new();
        for rec in &loaded.records {
            assert!(!rec.is_edited());
            rec.write_to(&mut serialized, SaveTerminator::Preserve, envelope)
                .unwrap();
        }
        assert_eq!(
            serialized, original,
            "byte-identical round-trip failed for {name}"
        );
    }
}

// FO-6, G-3, D-1
#[test]
fn terminator_normalization_lflf_crnul_and_stray_del() {
    let basic = load_bytes(&read_fixture("adt_a01_basic.hl7"), RecoveryMode::Strict);
    let lflf = load_bytes(&read_fixture("adt_a01_lflf.hl7"), RecoveryMode::Strict);
    let cr_nul = load_bytes(&read_fixture("adt_a01_cr_nul.hl7"), RecoveryMode::Strict);

    assert_eq!(lflf.records.len(), 1);
    assert_eq!(cr_nul.records.len(), 1);
    assert_eq!(lflf.records[0].bytes(), basic.records[0].bytes());
    assert_eq!(cr_nul.records[0].bytes(), basic.records[0].bytes());
    assert_eq!(lflf.summary.detected_terminator, SourceTerminator::Lf);
    assert_eq!(cr_nul.summary.detected_terminator, SourceTerminator::Cr);

    // Stray DEL (0x7F) is stripped (FO-6)
    let mut with_del = read_fixture("adt_a01_basic.hl7");
    with_del.insert(20, 0x7F);
    let cleaned = load_bytes(&with_del, RecoveryMode::Strict);
    assert_eq!(cleaned.records[0].bytes(), basic.records[0].bytes());
}

// FO-7, FO-8, D-3
#[test]
fn batch_envelope_splitting_and_mllp_stripping() {
    let batch = load_bytes(&read_fixture("batch_fhs_bhs.hl7"), RecoveryMode::Strict);
    let kinds: Vec<RecordKind> = batch.records.iter().map(|r| r.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            RecordKind::Fhs,
            RecordKind::Bhs,
            RecordKind::Msh,
            RecordKind::Msh,
            RecordKind::Msh,
            RecordKind::Bts,
            RecordKind::Fts,
        ]
    );
    assert_eq!(batch.records[0].summary().control_id, "FILE001");
    assert_eq!(batch.records[5].summary().control_id, "3");

    let mllp = load_bytes(&read_fixture("mllp_framed.dat"), RecoveryMode::Strict);
    assert!(mllp.summary.saw_mllp_envelope);
    assert_eq!(mllp.records.len(), 2);
    for rec in &mllp.records {
        assert!(!rec.bytes().contains(&0x0B));
        assert!(!rec.bytes().contains(&0x1C));
    }
}

// FO-9, FO-10
#[test]
fn alternate_delimiters_detected_positionally_per_record() {
    let loaded = load_bytes(&read_fixture("delims_alt.hl7"), RecoveryMode::Strict);
    assert_eq!(loaded.records.len(), 2);

    let r0 = &loaded.records[0];
    assert_eq!(r0.delimiters().as_bytes(), *b";:~\\&");
    let addr: FieldAddress = "PID-5-1".parse().unwrap();
    assert_eq!(r0.field_bytes(1, &addr), Some(&b"ALT"[..]));

    let r1 = &loaded.records[1];
    assert_eq!(r1.delimiters().as_bytes(), *b"#@!$%");
    assert_eq!(r1.summary().message_type, "ORU@R01");
    assert_eq!(r1.field_bytes(1, &addr), Some(&b"PIPE"[..]));
}

// FO-11, SCAN-3, D-4
#[test]
fn non_hl7_strict_aborts_and_recovery_extracts_both_messages() {
    let raw = read_fixture("nonhl7_mixed.txt");

    let strict = load_bytes(&raw, RecoveryMode::Strict);
    assert!(strict.aborted_on_non_hl7);
    assert!(strict.summary.saw_non_hl7);

    let recovered = load_bytes(&raw, RecoveryMode::RecoverNonHl7);
    assert!(!recovered.aborted_on_non_hl7);
    assert!(recovered.summary.saw_non_hl7);
    assert_eq!(recovered.records.len(), 2);
    assert_eq!(recovered.records[0].summary().control_id, "MIX001");
    assert_eq!(recovered.records[1].summary().control_id, "MIX002");

    // SCAN-3: direct streaming scanner event count matches loader record count.
    let cancel = AtomicBool::new(false);
    let mut scanner = RecordBoundaryScanner::new(
        BufReader::new(raw.as_slice()),
        ScannerConfig {
            recovery_mode: RecoveryMode::RecoverNonHl7,
            ..ScannerConfig::default()
        },
    );
    let mut direct_count = 0usize;
    scanner
        .scan_all(&cancel, |ev| {
            if matches!(ev, ScanEvent::RecordStart { .. }) {
                direct_count += 1;
            }
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(direct_count, recovered.records.len());
}

// §5.2, D-8, D-9
#[test]
fn field_address_generation_matches_spec_for_repeats_and_empty_fields() {
    let rep = load_bytes(
        &read_fixture("repeats_components.hl7"),
        RecoveryMode::Strict,
    );
    let pid_entries = rep.records[0].field_entries(1, FieldDisplayOptions::default());
    let addrs: Vec<String> = pid_entries
        .iter()
        .map(|e| {
            e.address
                .map(|a| a.to_string())
                .unwrap_or_else(|| String::from_utf8_lossy(&e.raw_value).into_owned())
        })
        .collect();

    assert!(addrs.contains(&"PID-3.4.1".to_string()));
    assert!(addrs.contains(&"PID-3.4.2".to_string()));
    assert!(addrs.contains(&"PID-3.4.3".to_string()));
    assert!(addrs.contains(&"PID-5(1).1".to_string()));
    assert!(addrs.contains(&"PID-5(2).1".to_string()));
    assert!(addrs.contains(&"PID-5(2).2".to_string()));
    // D-26: No synthetic marker lines are emitted
    assert!(pid_entries.iter().all(|e| !e.is_parsed_marker));

    // MSH-1 and MSH-2 verbatim and uneditable (§5.2, D-9)
    let msh_entries = rep.records[0].field_entries(0, FieldDisplayOptions::default());
    assert_eq!(msh_entries[0].address.unwrap().to_string(), "MSH-1");
    assert_eq!(msh_entries[0].raw_value, b"|");
    assert!(!msh_entries[0].is_editable);
    assert_eq!(msh_entries[1].address.unwrap().to_string(), "MSH-2");
    assert_eq!(msh_entries[1].raw_value, b"^~\\&");
    assert!(!msh_entries[1].is_editable);

    // Empty fields fixture (§5.2)
    let emp = load_bytes(&read_fixture("empty_fields.hl7"), RecoveryMode::Strict);
    let compact = emp.records[0].field_entries(
        1,
        FieldDisplayOptions {
            display_empty_fields: false,
            display_compound_fields: false,
        },
    );
    let compact_addrs: Vec<String> = compact
        .iter()
        .filter_map(|e| e.address.map(|a| a.to_string()))
        .collect();
    assert_eq!(compact_addrs, vec!["PID-1", "PID-3", "PID-5.2"]);

    let expanded = emp.records[0].field_entries(
        1,
        FieldDisplayOptions {
            display_empty_fields: true,
            display_compound_fields: false,
        },
    );
    let expanded_addrs: Vec<String> = expanded
        .iter()
        .filter_map(|e| e.address.map(|a| a.to_string()))
        .collect();
    assert!(expanded_addrs.contains(&"PID-2".to_string()));
    assert!(expanded_addrs.contains(&"PID-5.1".to_string()));
    assert!(expanded_addrs.contains(&"PID-5.3".to_string()));
}

// G-2, §5.5
#[test]
fn utf8_msh18_and_escape_decoding() {
    let utf8 = load_bytes(&read_fixture("utf8_msh18.hl7"), RecoveryMode::Strict);
    let rec = &utf8.records[0];
    let cs = rec.charset();
    assert!(!cs.is_unknown_fallback);
    let pid5 = rec
        .field_bytes(1, &"PID-5".parse().unwrap())
        .expect("PID-5 present");
    assert_eq!(cs.decode_display(pid5), "SYNTH\u{00fc}LLER^J\u{00e9}AN");

    let esc = load_bytes(&read_fixture("escapes.hl7"), RecoveryMode::Strict);
    let obx5 = esc.records[0]
        .field_bytes(2, &"OBX-5".parse().unwrap())
        .expect("OBX-5 present");
    assert_eq!(
        esc.records[0].delimiters().decode_escapes(obx5),
        b"Pipe|Caret^Amp&Tilde~Slash\\HexABBreak\nEnd"
    );
}

// G-1, EDIT-3 (segment splice touches only the target segment)
#[test]
fn splicing_one_segment_leaves_other_segments_byte_identical() {
    let mut loaded = load_bytes(&read_fixture("adt_a01_basic.hl7"), RecoveryMode::Strict);
    let rec = &mut loaded.records[0];
    let orig_msh = rec.segment_bytes(0).unwrap().to_vec();
    let orig_evn = rec.segment_bytes(1).unwrap().to_vec();
    let orig_pv1 = rec.segment_bytes(3).unwrap().to_vec();

    let old_pid = rec
        .splice_segment(2, b"PID|1||SYNTH9999||EDITED^NAME")
        .unwrap();
    assert!(old_pid.starts_with(b"PID|1||SYNTH0001"));
    assert!(rec.is_edited());
    assert_eq!(rec.segment_bytes(0).unwrap(), orig_msh.as_slice());
    assert_eq!(rec.segment_bytes(1).unwrap(), orig_evn.as_slice());
    assert_eq!(
        rec.segment_bytes(2).unwrap(),
        b"PID|1||SYNTH9999||EDITED^NAME"
    );
    assert_eq!(rec.segment_bytes(3).unwrap(), orig_pv1.as_slice());
}

// G-7: Never panic on arbitrary malformed bytes
proptest! {
    #[test]
    fn scanner_never_panics_on_arbitrary_bytes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = load_bytes(&data, RecoveryMode::Strict);
        let _ = load_bytes(&data, RecoveryMode::RecoverNonHl7);
    }

    // NFR-3, G-1: Parse -> serialize round-trip is byte-identical for generated HL7 streams.
    #[test]
    fn round_trip_is_byte_identical_for_generated_messages(
        msg_count in 1usize..=5,
        term_kind in 0u8..3,
        use_mllp in any::<bool>(),
        payload_words in proptest::collection::vec("[A-Za-z0-9 _-]{1,16}", 1..6),
    ) {
        let term: &[u8] = match term_kind {
            0 => b"\r",
            1 => b"\r\n",
            _ => b"\n",
        };
        let mut raw = Vec::new();
        for i in 0..msg_count {
            if use_mllp {
                raw.push(MllpEnvelope::DEFAULT.start_block);
            }
            raw.extend_from_slice(
                format!("MSH|^~\\&|SND|FAC|RCV|FAC|2026011512000{i}||ADT^A01|ID{i}|P|2.5.1")
                    .as_bytes(),
            );
            raw.extend_from_slice(term);
            raw.extend_from_slice(b"PID|1||");
            raw.extend_from_slice(payload_words[i % payload_words.len()].as_bytes());
            raw.extend_from_slice(term);
            if use_mllp {
                raw.push(MllpEnvelope::DEFAULT.end_data);
                raw.push(MllpEnvelope::DEFAULT.end_block);
            }
        }

        let loaded = load_bytes(&raw, RecoveryMode::Strict);
        prop_assert!(!loaded.aborted_on_non_hl7);
        prop_assert_eq!(loaded.records.len(), msg_count);

        let envelope = use_mllp.then_some(MllpEnvelope::DEFAULT);
        let mut out = Vec::new();
        for rec in &loaded.records {
            rec.write_to(&mut out, SaveTerminator::Preserve, envelope).unwrap();
        }
        prop_assert_eq!(out, raw);
    }
}

// Extra coverage for Delimiters::from_header_bytes with arbitrary bytes (G-7)
#[test]
fn delimiters_and_record_index_handle_malformed_inputs() {
    assert!(Delimiters::from_header_bytes(b"").is_none());
    assert!(Delimiters::from_header_bytes(b"MSH").is_none());
    assert!(Delimiters::from_header_bytes(b"MSH|||||").is_none());
}
