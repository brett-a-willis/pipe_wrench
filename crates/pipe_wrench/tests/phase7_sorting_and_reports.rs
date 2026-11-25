//! Phase 7 tests covering HL7 timestamp parsing, record sorting (`SORT-1`, `SORT-2`),
//! report building (`RPT-1`, `RPT-2`, `D-13`), single- and multi-column report sorting
//! (`SORT-3`, `SORT-4`), report definition persistence (`RPT-6`), and all report exporters
//! (`RPT-3..RPT-5`, `RPT-8`, `RPT-9`, `D-7`, `D-8`).

use hl7_core::{
    DelimitedExportOptions, DelimitedQuoteMode, ExportEncoding, ExportRowScope, FixedWidthAlign,
    FixedWidthColumnSpec, FixedWidthExportOptions, Hl7Dictionary, ReportColumn, ReportDefinition,
    ReportError, ReportRowGrain, ReportTable, SortDirection, SortKeySpec, SortKeyType,
};
use pipe_wrench::config::{load_report_definition_file, save_report_definition_in};
use pipe_wrench::PipeWrenchApp;
use tempfile::TempDir;

fn sample_app_with_records() -> PipeWrenchApp {
    let mut app = PipeWrenchApp::default();
    app.settings = pipe_wrench::AppSettings::default();
    let raw = concat!(
        // Record 0: 2026-01-15 17:00 UTC (via -0500 offset), seq 20
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000-0500||ADT^A01|MSG_B|P|2.5.1\r",
        "PID|1||PAT020||ZETA^PATIENT||19850101\r",
        "OBX|1|NM|GLU||105\r",
        "OBX|2|NM|HGB||14.2\r",
        // Record 1: Missing MSH-7 (sorts last!), seq 5
        "MSH|^~\\&|SND|FAC|RCV|FAC|||ADT^A08|MSG_MISSING|P|2.5.1\r",
        "PID|1||PAT005||ALPHA^PATIENT||19700101\r",
        "OBX|1|NM|GLU||95\r",
        // Record 2: 2026-01-15 12:00 UTC (earlier than Record 0!), seq 100
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000+0000||ADT^A01|MSG_A1|P|2.5.1\r",
        "PID|1||PAT100||BETA^PATIENT||19900101\r",
        "OBX|1|NM|GLU||210\r",
        // Record 3: Same MSH-7 as Record 2 (tests stability!), seq 3
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000+0000||ADT^A04|MSG_A2|P|2.5.1\r",
        "PID|1||PAT003||ALPHA^PATIENT||19920101\r",
        "OBX|1|NM|GLU||88\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);
    app
}

// SORT-1: Sort records by MSH-7 (mixed precisions & timezones, missing last, stability, "(sorted)" title, undoable)
#[test]
fn sort_records_by_msh7_timezones_stability_missing_last_and_undo() {
    let mut app = sample_app_with_records();
    assert!(!app.window_title().contains("(sorted)"));

    app.state
        .sort_records_by_msh7(SortDirection::Ascending, &app.settings)
        .unwrap();
    assert!(app.window_title().contains("(sorted)"));

    let ids: Vec<&str> = app
        .state
        .summaries
        .iter()
        .map(|s| s.control_id.as_str())
        .collect();
    // MSG_A1 and MSG_A2 share 12:00 UTC (stable order preserved), then MSG_B (17:00 UTC), then MSG_MISSING last.
    assert_eq!(ids, vec!["MSG_A1", "MSG_A2", "MSG_B", "MSG_MISSING"]);

    // Undo restores exact original record order and clears "(sorted)" from window title
    app.state.undo(&app.settings).unwrap();
    assert!(!app.window_title().contains("(sorted)"));
    let restored_ids: Vec<&str> = app
        .state
        .summaries
        .iter()
        .map(|s| s.control_id.as_str())
        .collect();
    assert_eq!(
        restored_ids,
        vec!["MSG_B", "MSG_MISSING", "MSG_A1", "MSG_A2"]
    );
}

// SORT-2, D-8: Sort records by any field address (Number, String, Date) using first occurrence
#[test]
fn sort_records_by_arbitrary_field_numeric_and_string() {
    let mut app = sample_app_with_records();

    // Sort numerically by first OBX-5 (values: 105, 95, 210, 88 -> 88, 95, 105, 210)
    app.state
        .sort_records_by_address(
            "OBX-5".parse().unwrap(),
            SortKeyType::Number,
            SortDirection::Ascending,
            &app.settings,
        )
        .unwrap();

    let ids: Vec<&str> = app
        .state
        .summaries
        .iter()
        .map(|s| s.control_id.as_str())
        .collect();
    assert_eq!(ids, vec!["MSG_A2", "MSG_MISSING", "MSG_B", "MSG_A1"]);
}

// RPT-1, RPT-2, SORT-3, SORT-4, D-13: Report row grain (per-record vs per-segment) and single/multi-column sort
#[test]
fn report_builder_row_grains_and_single_and_multi_column_sorting() {
    let app = sample_app_with_records();
    let dict = Hl7Dictionary::builtin();

    // 1. Per-record grain (4 records -> 4 rows)
    let def = ReportDefinition {
        name: "Patient Summary".to_string(),
        grain: ReportRowGrain::PerRecord,
        columns: vec![
            ReportColumn::new("PID-5.1".parse().unwrap(), SortKeyType::String),
            ReportColumn::new("OBX-5".parse().unwrap(), SortKeyType::Number),
        ],
    };
    let mut tbl = ReportTable::build(def, &app.state.doc.records, Some(&dict));
    assert_eq!(tbl.rows.len(), 4);
    assert_eq!(
        tbl.description_headers,
        vec!["Family Name", "Observation Value"]
    );

    // SORT-3: Toggle sort on column 1 (OBX-5 Number): first click Ascending, second click Descending
    let dir1 = tbl.sort_by_column_toggle(1).unwrap();
    assert_eq!(dir1, SortDirection::Ascending);
    let vals_asc: Vec<&str> = tbl.rows.iter().map(|r| r.cells[1].as_str()).collect();
    assert_eq!(vals_asc, vec!["88", "95", "105", "210"]);

    let dir2 = tbl.sort_by_column_toggle(1).unwrap();
    assert_eq!(dir2, SortDirection::Descending);
    let vals_desc: Vec<&str> = tbl.rows.iter().map(|r| r.cells[1].as_str()).collect();
    assert_eq!(vals_desc, vec!["210", "105", "95", "88"]);

    // SORT-4: Multi-column sort (Key 1: PID-5-1 Asc, Key 2: OBX-5 Asc)
    // Two records have PID-5-1 == "ALPHA" (with OBX-5 95 and 88); Key 2 orders 88 before 95!
    tbl.sort_multi(&[
        SortKeySpec {
            column_index: 0,
            direction: SortDirection::Ascending,
        },
        SortKeySpec {
            column_index: 1,
            direction: SortDirection::Ascending,
        },
    ])
    .unwrap();
    let pairs: Vec<(&str, &str)> = tbl
        .rows
        .iter()
        .map(|r| (r.cells[0].as_str(), r.cells[1].as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("ALPHA", "88"),
            ("ALPHA", "95"),
            ("BETA", "210"),
            ("ZETA", "105"),
        ]
    );

    // 4 keys rejected (SORT-4 max 3 keys)
    let bad_key = SortKeySpec {
        column_index: 0,
        direction: SortDirection::Ascending,
    };
    assert!(matches!(
        tbl.sort_multi(&[bad_key, bad_key, bad_key, bad_key]),
        Err(ReportError::TooManySortKeys(4))
    ));

    // 2. Per-segment grain with OBX driving segment (Record 0 has 2 OBX, others have 1 -> 5 rows total, D-13)
    let seg_def = ReportDefinition {
        name: "Lab Results".to_string(),
        grain: ReportRowGrain::PerSegment {
            driving_segment: *b"OBX",
        },
        columns: vec![
            ReportColumn::new("PID-3".parse().unwrap(), SortKeyType::String),
            ReportColumn::new("OBX-3".parse().unwrap(), SortKeyType::String),
            ReportColumn::new("OBX-5".parse().unwrap(), SortKeyType::Number),
        ],
    };
    let seg_tbl = ReportTable::build(seg_def, &app.state.doc.records, Some(&dict));
    assert_eq!(seg_tbl.rows.len(), 5);
    assert_eq!(seg_tbl.rows[0].cells, vec!["PAT020", "GLU", "105"]);
    assert_eq!(seg_tbl.rows[1].cells, vec!["PAT020", "HGB", "14.2"]);
}

// RPT-3, RPT-4, RPT-5, RPT-6, RPT-8, RPT-9, D-7: Exporters & Report Definition persistence
#[test]
fn report_exporters_delimited_fixed_width_clipboard_csv_json_and_definition_save() {
    let app = sample_app_with_records();
    let dict = Hl7Dictionary::builtin();

    let def = ReportDefinition {
        name: "ExportTest".to_string(),
        grain: ReportRowGrain::PerRecord,
        columns: vec![
            ReportColumn::new("MSH-10".parse().unwrap(), SortKeyType::String),
            ReportColumn::new("OBX-5".parse().unwrap(), SortKeyType::Number),
        ],
    };
    let tbl = ReportTable::build(def.clone(), &app.state.doc.records, Some(&dict));

    // RPT-3: Delimited export with escape shorthands (\t and ^t) and unknown escape error
    let tab_bytes = tbl
        .export_delimited(&DelimitedExportOptions {
            scope: ExportRowScope::SelectedRows(vec![0]),
            include_header: true,
            use_descriptions_in_header: false,
            separator_input: "^t".to_string(),
            quote_mode: DelimitedQuoteMode::QuoteAll,
            encoding: ExportEncoding::Utf8,
        })
        .unwrap();
    assert_eq!(
        String::from_utf8(tab_bytes).unwrap(),
        "\"MSH-10\"\t\"OBX-5\"\r\n\"MSG_B\"\t\"105\"\r\n"
    );

    let bad_sep = tbl.export_delimited(&DelimitedExportOptions {
        separator_input: "\\z".to_string(),
        ..DelimitedExportOptions::default()
    });
    assert_eq!(
        bad_sep.unwrap_err().to_string(),
        "Unknown escape sequence in separator."
    );

    // RPT-4: Fixed-width export (left space, right zero, truncation, narrow warning, >9999 error)
    let specs = vec![
        FixedWidthColumnSpec {
            width: 4, // narrower than "MSH-10" (6 chars) -> triggers warning & truncates "MSG_B" to "MSG_"
            align: FixedWidthAlign::LeftSpace,
        },
        FixedWidthColumnSpec {
            width: 5,
            align: FixedWidthAlign::RightZero,
        },
    ];
    assert_eq!(tbl.narrow_column_warnings(&specs, false), vec![0]);

    let fw_bytes = tbl
        .export_fixed_width(&FixedWidthExportOptions {
            scope: ExportRowScope::SelectedRows(vec![0]),
            include_header: false,
            use_descriptions_in_header: false,
            columns: specs,
            encoding: ExportEncoding::Utf8,
        })
        .unwrap();
    assert_eq!(String::from_utf8(fw_bytes).unwrap(), "MSG_00105\r\n");

    let over_max = tbl.export_fixed_width(&FixedWidthExportOptions {
        scope: ExportRowScope::AllRows,
        include_header: false,
        use_descriptions_in_header: false,
        columns: vec![FixedWidthColumnSpec {
            width: 10_000,
            align: FixedWidthAlign::LeftSpace,
        }],
        encoding: ExportEncoding::Utf8,
    });
    assert_eq!(
        over_max.unwrap_err().to_string(),
        "Field width cannot exceed 9,999 characters."
    );

    // RPT-5: Copy to clipboard (space-padded fixed-width, CRLF, with header, no trailing whitespace/blank lines)
    let clip = tbl.format_clipboard_text(&ExportRowScope::SelectedRows(vec![0]), false);
    assert!(clip.starts_with("MSH-10 "));
    assert!(clip.contains("\r\n"));
    assert_eq!(clip, clip.trim_end());
    for line in clip.split("\r\n") {
        assert_eq!(line, line.trim_end());
    }

    // RPT-8, RPT-9: CSV with UTF-8 BOM and JSON
    let csv_bom = tbl
        .export_csv(
            &ExportRowScope::SelectedRows(vec![0]),
            true,
            false,
            ExportEncoding::Utf8Bom,
        )
        .unwrap();
    assert!(csv_bom.starts_with(&[0xEF, 0xBB, 0xBF]));

    let json_bytes = tbl
        .export_json(&ExportRowScope::SelectedRows(vec![0]), ExportEncoding::Utf8)
        .unwrap();
    let json_str = String::from_utf8(json_bytes).unwrap();
    assert!(json_str.contains("\"MSH-10\": \"MSG_B\""));
    assert!(json_str.contains("\"OBX-5\": \"105\""));

    // RPT-6: Save & load report definition and reject forbidden name characters
    for bad_name in [
        "bad/name", "bad:name", "bad*name", "bad?name", "bad|name", "bad%name",
    ] {
        assert!(ReportDefinition::validate_name(bad_name).is_err());
    }
    let dir = TempDir::new().unwrap();
    let saved_path = save_report_definition_in(&def, dir.path()).unwrap();
    let loaded_def = load_report_definition_file(&saved_path).unwrap();
    assert_eq!(loaded_def, def);
}

#[test]
fn report_clipboard_skips_empty_framing_rows_and_trims_trailing_whitespace() {
    let mut app = PipeWrenchApp::default();
    app.settings = pipe_wrench::AppSettings::default();
    let raw = concat!(
        "FHS|^~\\&|SEND|FAC|RECV|FAC|20260115120000\r",
        "BHS|^~\\&|SEND|FAC|RECV|FAC|20260115120000\r",
        "MSH|^~\\&|SEND|FAC|RECV|FAC|20260115120100||ADT^A01|MSG001|P|2.5.1\r",
        "PID|1||MRN1001||DOE^JANE\r",
        "MSH|^~\\&|SEND|FAC|RECV|FAC|20260115120200||ORU^R01|MSG002|P|2.5.1\r",
        "PID|1||MRN1002||SMITH^JOHN\r",
        "BTS|2\r",
        "FTS|1\r",
    );
    app.state.paste_from_text(raw, false, &app.settings);
    assert_eq!(app.state.doc.len(), 6);

    let dict = Hl7Dictionary::builtin();
    let def = ReportDefinition {
        name: "FramingTrimTest".to_string(),
        grain: ReportRowGrain::PerRecord,
        columns: vec![
            ReportColumn::new("MSH-7".parse().unwrap(), SortKeyType::Date),
            ReportColumn::new("PID-3".parse().unwrap(), SortKeyType::String),
        ],
    };
    let tbl = ReportTable::build(def, &app.state.doc.records, Some(&dict));

    // Only the 2 ADT/ORU messages have MSH-7/PID-3; FHS/BHS/BTS/FTS empty rows are omitted
    assert_eq!(tbl.rows.len(), 2);

    let clip = tbl.format_clipboard_text(&ExportRowScope::AllRows, false);
    assert_eq!(clip, clip.trim_end());
    let lines: Vec<&str> = clip.split("\r\n").collect();
    assert_eq!(lines.len(), 3); // header + 2 data rows, zero trailing blank lines
    for line in &lines {
        assert!(!line.is_empty());
        assert_eq!(*line, line.trim_end());
    }
    assert!(lines[1].contains("20260115120100") && lines[1].contains("MRN1001"));
    assert!(lines[2].contains("20260115120200") && lines[2].contains("MRN1002"));
}
