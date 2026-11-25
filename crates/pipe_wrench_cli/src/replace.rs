//! `pipe_wrench_cli replace` subcommand (`D-27`).
//!
//! Granular HL7 v2 field/component/subcomponent-aware find-and-replace across files or directories
//! supporting `--find`, `--replace`, `--field <PATH>` (e.g. `PID-3.1.4`), `--regex` (with `$1`, `$2`
//! capture groups), `--ignore-case`, `--whole-value`, `--dry-run`, and atomic `--out <file>`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Args;
use hl7_core::{
    load_file, replace_in_record, save_records_atomic, FieldAddress, Hl7ReplaceOptions, Record,
    RecoveryMode, ReplaceSummary, SaveOptions, SaveTerminator, ScannerConfig,
};

use crate::filter::collect_input_files;

/// Arguments for `pipe_wrench_cli replace`.
#[derive(Debug, Clone, Args)]
pub struct ReplaceArgs {
    /// Input HL7 files or directories (supports `.hl7`, `.txt`, `.dat`, `.gz`, `.zip`).
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Text or regex pattern to find (may be omitted when `--field` is set to replace the entire value).
    #[arg(long, default_value = "")]
    pub find: String,

    /// Replacement string (supports regex capture groups `$1`, `$2`, ... when `--regex` is set).
    #[arg(long)]
    pub replace: String,

    /// Optional canonical HL7 v2 field/component/subcomponent path (`SEG-F[(R)][.C[.S]]`, e.g. `PID-3.1.4`).
    #[arg(long, short = 'f')]
    pub field: Option<String>,

    /// Treat `--find` as a regular expression (supports `$1`, `$2` capture groups in `--replace`).
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Case-insensitive matching.
    #[arg(long, short = 'i', default_value_t = false)]
    pub ignore_case: bool,

    /// Match the entire targeted field/subfield value (when `--field` is set) or whole words (unrestricted).
    #[arg(long, default_value_t = false)]
    pub whole_value: bool,

    /// Output file path (written atomically). If omitted and `--dry-run` is not set, writes to `stdout`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Output segment terminator: `preserve`, `cr`, `crlf`, `lf`.
    #[arg(long, default_value = "preserve")]
    pub term: String,

    /// Print match/replacement summary without writing or modifying any files.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Recurse into subdirectories when an input path is a directory (default: true).
    #[arg(long, default_value_t = true)]
    pub recursive: bool,

    /// Disable directory recursion.
    #[arg(long, default_value_t = false)]
    pub no_recursive: bool,

    /// Comma-separated extension patterns when traversing directories (default: `txt,dat,hl7,gz,zip`).
    #[arg(long, default_value = "txt,dat,hl7,gz,zip")]
    pub ext: String,
}

/// Execute `pipe_wrench_cli replace`, returning `(output_bytes, summary, exit_code)` where
/// exit code is `0` on success (≥1 replacement or dry-run) or `2` when 0 occurrences matched.
pub fn execute_replace_command(
    args: &ReplaceArgs,
) -> Result<(Vec<u8>, ReplaceSummary, i32), String> {
    let field_restriction = match args
        .field
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(
            raw.parse::<FieldAddress>()
                .map_err(|e| format!("Invalid --field path '{raw}': {e}"))?,
        ),
        None => None,
    };

    if args.find.is_empty() && field_restriction.is_none() {
        return Err("Either --find or --field must be specified.".to_string());
    }

    let term_mode = match args.term.trim().to_ascii_lowercase().as_str() {
        "preserve" => SaveTerminator::Preserve,
        "cr" => SaveTerminator::Cr,
        "crlf" => SaveTerminator::CrLf,
        "lf" => SaveTerminator::Lf,
        other => {
            return Err(format!(
                "Invalid --term '{other}'; expected preserve|cr|crlf|lf."
            ))
        }
    };

    let opts = Hl7ReplaceOptions {
        find: args.find.clone(),
        replace: args.replace.clone(),
        field_restriction,
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        match_whole: args.whole_value,
    };

    let recursive = !args.no_recursive && args.recursive;
    let files = collect_input_files(&args.inputs, recursive, &args.ext)?;

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };

    let mut all_records: Vec<Record> = Vec::new();
    let mut summary = ReplaceSummary::default();

    for path in &files {
        let out = load_file(path, scanner_cfg, &cancel).map_err(|e| e.to_string())?;
        for mut rec in out.records {
            if let Some((new_bytes, occ)) = replace_in_record(&rec, &opts)? {
                let _ = rec.replace_all_bytes(new_bytes);
                summary.occurrences += occ;
                summary.records_modified += 1;
            }
            all_records.push(rec);
        }
    }

    let exit_code = if summary.occurrences > 0 { 0 } else { 2 };

    if args.dry_run {
        let report = format!(
            "Dry run: would replace {} occurrence(s) across {} record(s) (of {} total).\n",
            summary.occurrences,
            summary.records_modified,
            all_records.len()
        );
        return Ok((report.into_bytes(), summary, exit_code));
    }

    if let Some(ref out_path) = args.out {
        save_records_atomic(
            out_path,
            &all_records,
            SaveOptions {
                term_mode,
                disk_envelope: None,
            },
        )
        .map_err(|e| e.to_string())?;
        let report = format!(
            "Replaced {} occurrence(s) across {} record(s); wrote {} record(s) to {}.\n",
            summary.occurrences,
            summary.records_modified,
            all_records.len(),
            out_path.display()
        );
        return Ok((report.into_bytes(), summary, exit_code));
    }

    let mut buf = Vec::new();
    for rec in &all_records {
        rec.write_to(&mut buf, term_mode, None)
            .map_err(|e| e.to_string())?;
    }
    Ok((buf, summary, exit_code))
}
