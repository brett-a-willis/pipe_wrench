//! Phase 14 integration tests covering:
//! 1. Multi-file opening (`load_files_sync`) and appending (`append_files_sync` / `Ctrl+Shift+O`)
//!    with per-record source file metadata and multi-file source label (`"3 files loaded"`).
//! 2. Global UI zoom (`ui_zoom` clamping `0.75..=2.0`, `zoom_in`/`zoom_out`/`reset_zoom`, and
//!    TOML persistence in `AppSettings`).
//! 3. HL7 escape sequence syntax highlighting (`\F\`, `\.br\`, `\X0D\`) in both Dark and Light
//!    `EditorPalette` modes.
//! 4. Notepad++-style Show Control Characters (`¶`) rendering `[CR]`, `[LF]`, `[CR][LF]`, `[VT]`,
//!    `[FS]`, `·`, and `→` markers cleanly while keeping raw message bytes lossless.

use hl7_core::{Delimiters, SourceTerminator};
use pipe_wrench::ui::detail_pane::{
    build_editor_layout_job_with_controls, control_char_overlay_badges, tokenize_hl7_syntax,
    SyntaxSpanKind,
};
use pipe_wrench::ui::theme::EditorPalette;
use pipe_wrench::{AppSettings, DataSource, SessionState, MAX_UI_ZOOM, MIN_UI_ZOOM};

#[test]
fn multi_file_open_and_append_preserves_per_record_source_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let f1 = temp.path().join("01_alpha.hl7");
    let f2 = temp.path().join("02_beta.hl7");
    let f3 = temp.path().join("03_gamma.hl7");

    std::fs::write(
        &f1,
        "MSH|^~\\&|S|F|R|F|20260115120000||ADT^A01|M1|P|2.5.1\rPID|1||P1||DOE^JOHN\r",
    )
    .unwrap();
    std::fs::write(
        &f2,
        "MSH|^~\\&|S|F|R|F|20260115120001||ORU^R01|M2|P|2.5.1\rPID|1||P2||SMITH^ALICE\r",
    )
    .unwrap();
    std::fs::write(
        &f3,
        "MSH|^~\\&|S|F|R|F|20260115120002||ORM^O01|M3|P|2.5.1\rPID|1||P3||BROWN^BOB\r",
    )
    .unwrap();

    let settings = AppSettings::default();
    let mut state = SessionState::new();

    // 1. Open two files simultaneously (lexicographic order)
    let loaded = state
        .load_files_sync(
            &[f2.clone(), f1.clone()],
            hl7_core::RecoveryMode::Strict,
            &settings,
        )
        .unwrap();
    assert_eq!(loaded, 2);
    assert_eq!(state.doc.len(), 2);
    assert_eq!(state.source.display_label(), "2 files loaded");
    assert_eq!(
        state.record_sources,
        vec![Some(f1.clone()), Some(f2.clone())]
    );

    // 2. Append a third file (File -> Append File(s)... / Ctrl+Shift+O) without clearing existing records
    let appended = state
        .append_files_sync(
            std::slice::from_ref(&f3),
            hl7_core::RecoveryMode::Strict,
            &settings,
        )
        .unwrap();
    assert_eq!(appended, 1);
    assert_eq!(state.doc.len(), 3);
    assert_eq!(state.source.display_label(), "3 files loaded");
    assert_eq!(state.record_sources, vec![Some(f1), Some(f2), Some(f3)]);
    assert!(matches!(state.source, DataSource::Files(ref v) if v.len() == 3));
}

#[test]
fn ui_zoom_clamping_steps_reset_and_toml_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let cfg_path = temp.path().join("settings.toml");

    let mut settings = AppSettings::default();
    assert_eq!(settings.zoom_percent(), 100);
    assert!(!settings.show_control_chars);

    // Zoom in steps and clamp at MAX_UI_ZOOM (2.0)
    settings.zoom_in();
    assert_eq!(settings.zoom_percent(), 110);
    for _ in 0..20 {
        settings.zoom_in();
    }
    assert!((settings.ui_zoom - MAX_UI_ZOOM).abs() < 1e-5);
    assert_eq!(settings.zoom_percent(), 200);

    // Zoom out and clamp at MIN_UI_ZOOM (0.75)
    for _ in 0..30 {
        settings.zoom_out();
    }
    assert!((settings.ui_zoom - MIN_UI_ZOOM).abs() < 1e-5);
    assert_eq!(settings.zoom_percent(), 75);

    // Reset to 100%
    settings.reset_zoom();
    assert_eq!(settings.zoom_percent(), 100);

    // Persist custom zoom + show_control_chars to TOML and reload
    settings.set_ui_zoom(1.30);
    settings.show_control_chars = true;
    settings.save_to(&cfg_path).unwrap();

    let reloaded = AppSettings::load_from(&cfg_path);
    assert_eq!(reloaded.zoom_percent(), 130);
    assert!(reloaded.show_control_chars);
}

#[test]
fn hl7_escape_sequence_highlighting_in_dark_and_light_palettes() {
    let text = "MSH|^~\\&|SND|FAC\nOBX|1|TX|NOTE||A\\F\\B\\.br\\C\\X0D\\D\\E\\E";
    let spans = tokenize_hl7_syntax(text, Delimiters::STANDARD, Some(1));

    let escape_texts: Vec<&str> = spans
        .iter()
        .filter(|s| s.kind == SyntaxSpanKind::EscapeSequence)
        .map(|s| &text[s.range.clone()])
        .collect();
    assert_eq!(escape_texts, vec!["\\F\\", "\\.br\\", "\\X0D\\", "\\E\\"]);

    // Verify MSH-2's `\` is still classified as EscapeDelimiter (not EscapeSequence)
    assert!(spans
        .iter()
        .any(|s| s.kind == SyntaxSpanKind::EscapeDelimiter && &text[s.range.clone()] == "\\"));

    // Verify distinct escape_sequence colors in both Dark and Light EditorPalettes
    let dark = EditorPalette::DARK_PLUS;
    let light = EditorPalette::LIGHT_PLUS;
    assert_ne!(
        SyntaxSpanKind::EscapeSequence.color_in(&dark),
        SyntaxSpanKind::Text.color_in(&dark)
    );
    assert_ne!(
        SyntaxSpanKind::EscapeSequence.color_in(&light),
        SyntaxSpanKind::Text.color_in(&light)
    );
    assert_ne!(
        SyntaxSpanKind::EscapeSequence.color_in(&dark),
        SyntaxSpanKind::EscapeSequence.color_in(&light)
    );
}

#[test]
fn show_control_chars_preserves_1to1_cursor_mapping_and_renders_overlay_badges() {
    // Exact 3-line HL7 message: verify strict 1:1 char count and exact cursor offset at "NOTE"
    let msg_text = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ORU^R01|ESC001|P|2.5.1\n",
        "PID|1||SYNTH5001||ESCAPE^TEST\n",
        "OBX|1|TX|NOTE||Pipe\\F\\Caret\\S\\Amp\\T\\Tilde\\R\\Slash\\E\\Hex\\X4142\\Break\\.br\\End",
    );

    let job_off = build_editor_layout_job_with_controls(
        msg_text,
        800.0,
        Delimiters::STANDARD,
        Some(2),
        &EditorPalette::DARK_PLUS,
        false,
    );
    let job_on = build_editor_layout_job_with_controls(
        msg_text,
        800.0,
        Delimiters::STANDARD,
        Some(2),
        &EditorPalette::DARK_PLUS,
        true,
    );

    // Strict 1:1 character invariant: CCursor indices in TextEdit MUST match the underlying string
    assert_eq!(job_off.text.chars().count(), msg_text.chars().count());
    assert_eq!(job_on.text.chars().count(), msg_text.chars().count());

    // Placing cursor at the start of "NOTE" on line 3 maps to the exact same char index in both modes
    let note_char_idx = msg_text[..msg_text.find("NOTE").unwrap()].chars().count();
    let job_on_char_at_note: String = job_on.text.chars().skip(note_char_idx).take(4).collect();
    assert_eq!(job_on_char_at_note, "NOTE");

    // Verify 1:1 inline markers (·, →, ␋, ␜) and non-intrusive overlay badges ([CR], [LF], [CR][LF], [VT], [FS])
    let sample = "\x0BMSH|^~\\&|A B\tC\r\nPID|1||ID1\rOBX|1||VAL\n\x1C";
    let sample_job = build_editor_layout_job_with_controls(
        sample,
        800.0,
        Delimiters::STANDARD,
        Some(0),
        &EditorPalette::DARK_PLUS,
        true,
    );
    assert_eq!(sample_job.text.chars().count(), sample.chars().count());
    assert!(sample_job.text.contains("A·B→C"));

    let badges = control_char_overlay_badges(sample, SourceTerminator::Lf);
    assert!(badges.contains(&(0, "[VT]")));
    assert!(badges.contains(&(0, "[CR][LF]")));
    assert!(badges.contains(&(1, "[CR]")));
    assert!(badges.contains(&(2, "[LF]")));
    assert!(badges.contains(&(3, "[FS]")));

    // Verify underlying SessionState record bytes remain 100% lossless when toggling show_control_chars
    let mut settings = AppSettings::default();
    let mut state = SessionState::new();
    let raw =
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\rPID|1||12345||DOE^JOHN\r";
    state.paste_from_text(raw, false, &settings);
    let original_bytes = state.doc.records[0].bytes().to_vec();

    settings.show_control_chars = true;
    state.rebuild_selected_record_cache(&settings);
    assert_eq!(state.doc.records[0].bytes(), original_bytes.as_slice());
}
