//! Implementation of the `pipe_wrench_cli filter` subcommand (`D-26`).
//!
//! Filters HL7 v2 records across input files or directories (including `.gz` and `.zip`)
//! using plain-text, regex, or canonical field-path comparison predicates (`=`, `!=`, `~=`,
//! `>`, `>=`, `<`, `<=`), writing matching records atomically to `--out` (or `stdout`) or
//! emitting `--count`. Exit codes: `0` (matched), `1` (error), `2` (0 matches).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::Parser;
use hl7_core::{
    load_file, normalize_extension_glob, save_records_atomic, FilterMatchOptions, Record,
    RecordFilter, RecoveryMode, SaveOptions, SaveTerminator, ScannerConfig,
};
use walkdir::WalkDir;

use crate::convert::CliTerminator;

/// Arguments for `pipe_wrench_cli filter` (`D-26`).
#[derive(Debug, Clone, Parser)]
pub struct FilterArgs {
    /// One or more input files or directories (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`).
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Plain-text, regex, or field-path filter expression supporting `(*)` wildcard repetitions
    /// and `=`, `!=`, `~=`, `=~`, `contains`, `exists`, `>`, `>=`, `<`, `<=`
    /// (e.g. `PID-3(*).1=123`, `PID-5.1~=DOE`, `PID-5.1=~^SM.*`, `ROL-4 exists`, `OBX-5>=200`).
    #[arg(long, alias = "query", short = 'q')]
    pub filter: Option<String>,

    /// Strict field-path predicate supporting `(*)` wildcard repetitions and `=`, `!=`, `~=`, `=~`,
    /// `contains`, `exists`, `>`, `>=`, `<`, `<=` (e.g. `PID-3(*).1=123`, `ROL-4 exists`).
    #[arg(long)]
    pub filter_field: Vec<String>,

    /// Case-insensitive matching (default: true; pass `--ignore-case=false` for case-sensitive).
    #[arg(long, short = 'i', default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub ignore_case: bool,

    /// Interpret filter pattern(s) as regular expressions.
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Invert match to select non-matching records (`-v` / `--invert-match`).
    #[arg(long, short = 'v', default_value_t = false)]
    pub invert_match: bool,

    /// Recurse into input directories (default: true).
    #[arg(long, default_value_t = true, conflicts_with = "no_recursive")]
    pub recursive: bool,

    /// Disable directory recursion.
    #[arg(long, default_value_t = false)]
    pub no_recursive: bool,

    /// Comma-separated extension patterns when traversing directories (default: `txt,dat,hl7,gz,zip`).
    #[arg(long, default_value = "txt,dat,hl7,gz,zip")]
    pub ext: String,

    /// Segment terminator normalization (`preserve`, `cr`, `crlf`, or `lf`).
    #[arg(long, value_enum, default_value_t = CliTerminator::Preserve)]
    pub term: CliTerminator,

    /// Print only the count of matching records instead of the record payloads.
    #[arg(long, short = 'c', default_value_t = false)]
    pub count: bool,

    /// Write matching records atomically to `<OUT>` instead of `stdout`.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Discover input files from a mix of file and directory paths (`D-26`).
pub fn collect_input_files(
    inputs: &[PathBuf],
    recursive: bool,
    ext_spec: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut allowed_exts: Vec<String> = Vec::new();
    let mut allow_all = false;
    for raw in ext_spec.split(',') {
        let norm = normalize_extension_glob(raw);
        if norm == "*.*" || norm == "*" {
            allow_all = true;
        } else if let Some(stripped) = norm.strip_prefix("*.") {
            allowed_exts.push(stripped.to_ascii_lowercase());
        } else {
            allowed_exts.push(norm.to_ascii_lowercase());
        }
    }

    let mut files = Vec::new();
    for input in inputs {
        if !input.exists() {
            return Err(format!("Input path '{}' does not exist.", input.display()));
        }
        if input.is_file() {
            files.push(input.clone());
        } else if input.is_dir() {
            let max_depth = if recursive { usize::MAX } else { 1 };
            let mut dir_files = Vec::new();
            for entry in WalkDir::new(input)
                .max_depth(max_depth)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file()
                    && matches_ext(entry.path(), allow_all, &allowed_exts)
                {
                    dir_files.push(entry.into_path());
                }
            }
            dir_files.sort();
            files.extend(dir_files);
        }
    }
    Ok(files)
}

fn matches_ext(path: &Path, allow_all: bool, allowed_exts: &[String]) -> bool {
    if allow_all || allowed_exts.is_empty() {
        return true;
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    allowed_exts.iter().any(|a| a == &lower)
}

/// Execute `pipe_wrench_cli filter`, returning `(output_bytes, exit_code)` where exit code is
/// `0` when at least one record matched, `2` when 0 records matched, or `Err` on fatal error (`1`).
pub fn execute_filter_command(args: &FilterArgs) -> Result<(Vec<u8>, i32), String> {
    let opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: args.invert_match,
    };
    let predicate = RecordFilter::from_cli_specs_with_options(
        args.filter.as_deref(),
        &args.filter_field,
        opts,
    )?;

    let recursive = !args.no_recursive && args.recursive;
    let files = collect_input_files(&args.inputs, recursive, &args.ext)?;

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };

    let mut matched_records: Vec<Record> = Vec::new();
    for path in &files {
        let out = load_file(path, scanner_cfg, &cancel).map_err(|e| e.to_string())?;
        for rec in out.records {
            let keep = match &predicate {
                Some(f) => f.matches_record(&rec),
                None => true,
            };
            if keep {
                matched_records.push(rec);
            }
        }
    }

    let match_count = matched_records.len();
    let exit_code = if match_count == 0 { 2 } else { 0 };

    if args.count {
        let text = format!("{match_count}\n").into_bytes();
        if let Some(ref out_path) = args.out {
            std::fs::write(out_path, &text)
                .map_err(|e| format!("Failed writing '{}': {e}", out_path.display()))?;
        }
        return Ok((text, exit_code));
    }

    let term_mode: SaveTerminator = args.term.into();
    if let Some(ref out_path) = args.out {
        save_records_atomic(
            out_path,
            &matched_records,
            SaveOptions {
                term_mode,
                disk_envelope: None,
            },
        )
        .map_err(|e| e.to_string())?;
    }

    let mut out_bytes = Vec::new();
    for rec in &matched_records {
        rec.write_to(&mut out_bytes, term_mode, None)
            .map_err(|e| e.to_string())?;
    }

    Ok((out_bytes, exit_code))
}
