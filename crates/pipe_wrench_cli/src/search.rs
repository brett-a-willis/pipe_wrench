//! Implementation of the `pipe_wrench_cli search` subcommand (HL7-aware `grep`, `D-26`).
//!
//! Searches HL7 v2 files or directories (including `.gz` and `.zip`) by text/regex query
//! and/or canonical field path (or field comparison like `OBX-5>200`), enriches each hit
//! with context fields (`--with MSH-10,MSH-9,PID-3.1`), optionally prints the matching
//! segment or full record, or summarizes hit counts by field path (`--summary`).
//! Exit codes: `0` (≥1 match), `1` (error), `2` (0 matches).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use hl7_core::{
    find_search_matches_with_options, load_file, summarize_matches_by_field, FieldAddress,
    FieldHitCount, FilterMatchOptions, Record, RecordFilter, RecoveryMode, SaveTerminator,
    ScannerConfig, SearchQueryOptions,
};
use serde::{Deserialize, Serialize};

use crate::filter::collect_input_files;

/// Output format for `pipe_wrench_cli search` (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SearchOutputFormat {
    /// Human-readable ASCII table.
    #[default]
    Table,
    /// RFC 4180 CSV.
    Csv,
    /// Pretty-printed JSON.
    Json,
}

/// Arguments for `pipe_wrench_cli search` (`D-26`).
#[derive(Debug, Clone, Parser)]
pub struct SearchArgs {
    /// One or more input files or directories (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`).
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Text or regex query to search for, or field predicate with `(*)` wildcard repetitions
    /// and `=`, `!=`, `~=`, `=~`, `contains`, `exists`, `>`, `>=`, `<`, `<=` (e.g. `PID-3(*).1=123`, `OBX-5>=200`, `ROL-4 exists`).
    #[arg(long, short = 'q')]
    pub query: Option<String>,

    /// Restrict search to a canonical HL7 field path (`OBX-5`) or field predicate
    /// supporting `(*)` wildcards and `=`, `!=`, `~=`, `=~`, `contains`, `exists`, `>`, `>=`, `<`, `<=`.
    #[arg(long, short = 'f')]
    pub field: Option<String>,

    /// Case-insensitive matching (default: true; pass `--ignore-case=false` for case-sensitive).
    #[arg(long, short = 'i', default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub ignore_case: bool,

    /// Interpret `--query` as a regular expression.
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Maximum number of matches to return.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Stop after the first match per record.
    #[arg(long, default_value_t = false)]
    pub first: bool,

    /// Comma-separated context field paths included with each hit (default: `MSH-10,MSH-9,PID-3.1`).
    #[arg(long, default_value = "MSH-10,MSH-9,PID-3.1")]
    pub with: String,

    /// Output format (`table`, `csv`, or `json`).
    #[arg(long, value_enum, default_value_t = SearchOutputFormat::Table)]
    pub format: SearchOutputFormat,

    /// Include the full matching segment line in the output.
    #[arg(long, default_value_t = false)]
    pub print_segment: bool,

    /// Include the full matching record in the output.
    #[arg(long, default_value_t = false)]
    pub print_record: bool,

    /// Output a hit-count breakdown grouped by canonical field path.
    #[arg(long, default_value_t = false)]
    pub summary: bool,

    /// Recurse into input directories (default: true).
    #[arg(long, default_value_t = true, conflicts_with = "no_recursive")]
    pub recursive: bool,

    /// Disable directory recursion.
    #[arg(long, default_value_t = false)]
    pub no_recursive: bool,

    /// Comma-separated extension patterns when traversing directories (default: `txt,dat,hl7,gz,zip`).
    #[arg(long, default_value = "txt,dat,hl7,gz,zip")]
    pub ext: String,

    /// Write output to a file instead of `stdout`.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// One enriched search result row produced by `pipe_wrench_cli search` (`D-26`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResultRow {
    /// Source file path.
    pub file: String,
    /// 1-based record index within `file`.
    pub record_number: usize,
    /// 1-based segment index within the record.
    pub segment_number: usize,
    /// Canonical field path (or segment tag).
    pub field_path: String,
    /// Matched field or segment value.
    pub value: String,
    /// Extracted `--with` context field values `(address_string, value)`.
    pub context_fields: Vec<(String, String)>,
    /// Optional full segment text when `--print-segment` is passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_text: Option<String>,
    /// Optional full record text when `--print-record` is passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_text: Option<String>,
}

/// Complete JSON output payload for `pipe_wrench_cli search --format json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchJsonOutput {
    /// Total matches returned.
    pub total_matches: usize,
    /// Hit rows.
    pub matches: Vec<SearchResultRow>,
    /// Field-path hit breakdown (populated when `--summary` is enabled or in JSON mode).
    pub summary_by_field: Vec<FieldHitCount>,
}

/// Execute `pipe_wrench_cli search`, returning `(formatted_bytes, exit_code)` where exit code is
/// `0` when at least one hit was found, `2` when 0 hits were found, or `Err` (`1`) on error.
pub fn execute_search_command(args: &SearchArgs) -> Result<(Vec<u8>, i32), String> {
    let query_raw = args.query.as_deref().unwrap_or("").trim();
    let field_raw = args.field.as_deref().unwrap_or("").trim();

    if query_raw.is_empty() && field_raw.is_empty() {
        return Err("At least one of --query or --field must be specified.".to_string());
    }

    let filter_opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: false,
    };

    // `--field` can be either a pure canonical path (`OBX-5`) or a comparison expression (`OBX-5>200`).
    let (effective_query, field_restriction) = if !field_raw.is_empty() {
        if let Ok(addr) = field_raw.parse::<FieldAddress>() {
            (query_raw.to_string(), Some(addr))
        } else {
            // Validate as a strict field comparison predicate
            let _ = RecordFilter::parse_field_predicate_with_options(field_raw, filter_opts)?;
            (field_raw.to_string(), None)
        }
    } else {
        (query_raw.to_string(), None)
    };

    let search_opts = SearchQueryOptions {
        query: effective_query,
        field_restriction,
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        match_whole: false,
    };

    let with_addrs: Vec<FieldAddress> = args
        .with
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<FieldAddress>()
                .map_err(|e| format!("Invalid --with field address '{s}': {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let recursive = !args.no_recursive && args.recursive;
    let files = collect_input_files(&args.inputs, recursive, &args.ext)?;

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };

    let mut all_core_matches = Vec::new();
    let mut rows: Vec<SearchResultRow> = Vec::new();

    'file_loop: for path in &files {
        let out = load_file(path, scanner_cfg, &cancel).map_err(|e| e.to_string())?;
        let file_matches = find_search_matches_with_options(&out.records, &search_opts)?;
        let mut last_rec_seen: Option<usize> = None;

        for m in file_matches {
            if args.first && last_rec_seen == Some(m.record_index) {
                continue;
            }
            last_rec_seen = Some(m.record_index);

            let rec = &out.records[m.record_index];
            let charset = rec.charset();
            let field_path = m
                .field_address
                .map_or_else(|| "(segment)".to_string(), |a| a.to_string());

            let context_fields: Vec<(String, String)> = with_addrs
                .iter()
                .map(|addr| {
                    let val = extract_first_field_value(rec, addr);
                    (addr.to_string(), val)
                })
                .collect();

            let segment_text = args.print_segment.then(|| {
                rec.segment_bytes(m.segment_index)
                    .map(|b| charset.decode_display(b).into_owned())
                    .unwrap_or_default()
            });

            let record_text = args.print_record.then(|| {
                let mut buf = Vec::new();
                let _ = rec.write_to(&mut buf, SaveTerminator::CrLf, None);
                charset.decode_display(&buf).trim_end().to_string()
            });

            all_core_matches.push(m.clone());
            rows.push(SearchResultRow {
                file: path.display().to_string(),
                record_number: m.record_index + 1,
                segment_number: m.segment_index + 1,
                field_path,
                value: m.matched_value,
                context_fields,
                segment_text,
                record_text,
            });

            if let Some(limit) = args.limit {
                if rows.len() >= limit {
                    break 'file_loop;
                }
            }
        }
    }

    let summary_by_field = summarize_matches_by_field(&all_core_matches);
    let exit_code = if rows.is_empty() { 2 } else { 0 };

    let formatted = match args.format {
        SearchOutputFormat::Json => {
            let payload = SearchJsonOutput {
                total_matches: rows.len(),
                matches: rows,
                summary_by_field,
            };
            serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?
        }
        SearchOutputFormat::Csv => {
            format_search_csv(&rows, &with_addrs, args.summary, &summary_by_field)?
        }
        SearchOutputFormat::Table => {
            format_search_table(&rows, args.summary, &summary_by_field).into_bytes()
        }
    };

    if let Some(ref out_path) = args.out {
        std::fs::write(out_path, &formatted)
            .map_err(|e| format!("Failed writing '{}': {e}", out_path.display()))?;
    }

    Ok((formatted, exit_code))
}

fn extract_first_field_value(rec: &Record, addr: &FieldAddress) -> String {
    let Some((seg_idx, _)) = rec.segments().find_by_tag(&addr.segment).next() else {
        return String::new();
    };
    rec.field_bytes(seg_idx, addr)
        .map(|b| rec.charset().decode_display(b).into_owned())
        .unwrap_or_default()
}

fn format_search_csv(
    rows: &[SearchResultRow],
    with_addrs: &[FieldAddress],
    summary_only: bool,
    summary: &[FieldHitCount],
) -> Result<Vec<u8>, String> {
    let mut wtr = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());

    if summary_only {
        wtr.write_record(["field_path", "count"])
            .map_err(|e| e.to_string())?;
        for s in summary {
            wtr.write_record([&s.field_path, &s.count.to_string()])
                .map_err(|e| e.to_string())?;
        }
    } else {
        let mut headers = vec![
            "file".to_string(),
            "record".to_string(),
            "segment".to_string(),
            "field_path".to_string(),
            "value".to_string(),
        ];
        for a in with_addrs {
            headers.push(a.to_string());
        }
        wtr.write_record(&headers).map_err(|e| e.to_string())?;

        for r in rows {
            let mut rec = vec![
                r.file.clone(),
                r.record_number.to_string(),
                r.segment_number.to_string(),
                r.field_path.clone(),
                r.value.clone(),
            ];
            for (_, v) in &r.context_fields {
                rec.push(v.clone());
            }
            wtr.write_record(&rec).map_err(|e| e.to_string())?;
        }
    }

    wtr.into_inner().map_err(|e| e.to_string())
}

fn format_search_table(
    rows: &[SearchResultRow],
    include_summary: bool,
    summary: &[FieldHitCount],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Matches found: {}\n", rows.len()));

    if include_summary && !summary.is_empty() {
        out.push_str("\nHit Summary by Field Path:\n");
        for item in summary {
            out.push_str(&format!("  {:<16} {}\n", item.field_path, item.count));
        }
        out.push('\n');
    }

    for r in rows {
        let ctx_str: Vec<String> = r
            .context_fields
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let ctx_joined = if ctx_str.is_empty() {
            String::new()
        } else {
            format!(" [{}]", ctx_str.join(" "))
        };
        out.push_str(&format!(
            "{}:rec#{}:seg#{}:{:<12} = {}{}\n",
            r.file, r.record_number, r.segment_number, r.field_path, r.value, ctx_joined
        ));
        if let Some(ref seg_txt) = r.segment_text {
            out.push_str(&format!("  Segment: {seg_txt}\n"));
        }
        if let Some(ref rec_txt) = r.record_text {
            for line in rec_txt.lines() {
                out.push_str(&format!("  | {line}\n"));
            }
        }
    }

    out
}
