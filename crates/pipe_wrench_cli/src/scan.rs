//! Implementation of the `pipe_wrench_cli scan` subcommand (`SCAN-14`, `D-21`).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use hl7_core::{
    scan_directory, DirectoryScanConfig, FieldAddress, FilterMatchOptions, GroupByKey, RecordFilter,
};

/// Output format for `pipe_wrench_cli scan` (`SCAN-14`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable ASCII table.
    #[default]
    Table,
    /// RFC 4180 CSV.
    Csv,
    /// Pretty-printed JSON.
    Json,
}

/// Arguments for `pipe_wrench_cli scan` (`SCAN-14`, `D-21`).
#[derive(Debug, Clone, Parser)]
pub struct ScanArgs {
    /// Target directory to scan.
    pub dir: PathBuf,

    /// Extension or glob patterns (repeatable or comma-separated; default: `txt,dat,hl7`; `*.*` allowed) (`SCAN-1`, `D-21`).
    #[arg(long, default_values_t = vec!["txt,dat,hl7".to_string()])]
    pub ext: Vec<String>,

    /// Recurse into subdirectories (default: true).
    #[arg(long, default_value_t = true, conflicts_with = "no_recursive")]
    pub recursive: bool,

    /// Disable recursion into subdirectories (`--no-recursive`).
    #[arg(long, default_value_t = false)]
    pub no_recursive: bool,

    /// Streaming decompression of `.zip` and `.gz` archives (`SCAN-9`).
    #[arg(long, default_value_t = true)]
    pub include_archives: bool,

    /// Exclude glob pattern(s) (can be repeated or comma-separated).
    #[arg(long)]
    pub exclude: Vec<String>,

    /// Minimum file size in bytes.
    #[arg(long)]
    pub min_size: Option<u64>,

    /// Maximum file size in bytes.
    #[arg(long)]
    pub max_size: Option<u64>,

    /// Comma-separated group-by keys: `type` (MSH-9), `app` (MSH-3/4), `recv` (MSH-5/6),
    /// `date` (MSH-7 day), or any field address like `PID-8` (`SCAN-7`, `SCAN-8`).
    #[arg(long)]
    pub group_by: Option<String>,

    /// Validate `BHS..BTS` batch message counts against `BTS-1` (`SCAN-5`, `D-20`).
    #[arg(long, default_value_t = true)]
    pub check_batches: bool,

    /// Check for duplicate MSH-10 control IDs within each file and across the scan (`SCAN-12`).
    #[arg(long, alias = "check-duplicate-control-ids", default_value_t = false)]
    pub dupe_control_id: bool,

    /// Optional plain-text or field-path filter expression supporting `(*)` wildcard repetitions
    /// and `=`, `!=`, `~=`, `=~`, `contains`, `exists`, `>`, `>=`, `<`, `<=` (`D-26`).
    #[arg(long)]
    pub filter: Option<String>,

    /// Optional field-path filter expression supporting `(*)` wildcard repetitions and `=`, `!=`, `~=`, `=~`,
    /// `contains`, `exists`, `>`, `>=`, `<`, `<=` (e.g. `PID-3(*).1=123`, `ROL-4 exists`) (`D-26`).
    #[arg(long)]
    pub filter_field: Vec<String>,

    /// Case-insensitive filter matching (default: true; pass `--ignore-case=false` for case-sensitive) (`D-26`).
    #[arg(long, short = 'i', default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub ignore_case: bool,

    /// Interpret `--filter` and `--filter-field` values as regular expressions (`D-26`).
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Invert filter match (`-v` / `--invert-match`) (`D-26`).
    #[arg(long, short = 'v', default_value_t = false)]
    pub invert_match: bool,

    /// Output format (`table`, `csv`, or `json`).
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    /// Write output to file instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Number of parallel worker threads (`--jobs N` or `--threads N`).
    #[arg(long, alias = "threads")]
    pub jobs: Option<usize>,

    /// Suppress table/stdout output when running silently.
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
}

/// Parse a comma-separated `--group-by` specification into [`GroupByKey`]s (`SCAN-7`, `SCAN-8`, `SCAN-14`).
pub fn parse_group_by_spec(spec: &str) -> Result<Vec<GroupByKey>, String> {
    let mut keys = Vec::new();
    for raw in spec.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let key = match token.to_ascii_lowercase().as_str() {
            "type" | "msh-9" => GroupByKey::MessageType,
            "app" | "send" | "sending" | "msh-3" | "msh-3/4" => GroupByKey::SendingAppFacility,
            "recv" | "receiving" | "msh-5" | "msh-5/6" => GroupByKey::ReceivingAppFacility,
            "date" | "day" | "msh-7" => GroupByKey::MessageDate,
            _ => {
                let addr: FieldAddress = token
                    .parse()
                    .map_err(|e| format!("unknown --group-by key '{token}': {e}"))?;
                GroupByKey::CustomAddress(addr)
            }
        };
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// Execute `ScanArgs`, writing output to `--out` (or returning the formatted bytes) and returning
/// the process exit code (`0` = success, `1` = fatal error, `2` = completed with unreadable files) (`SCAN-14`).
pub fn execute_scan_command(args: &ScanArgs) -> Result<(Vec<u8>, i32), String> {
    let extension_patterns: Vec<String> = args
        .ext
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let exclude_globs: Vec<String> = args
        .exclude
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let group_by = match &args.group_by {
        Some(spec) => parse_group_by_spec(spec)?,
        None => Vec::new(),
    };
    let filter_opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: args.invert_match,
    };
    let record_filter = RecordFilter::from_cli_specs_with_options(
        args.filter.as_deref(),
        &args.filter_field,
        filter_opts,
    )?;

    let cfg = DirectoryScanConfig {
        root_dir: args.dir.clone(),
        extension_patterns,
        recursive: !args.no_recursive && args.recursive,
        exclude_globs,
        min_file_size: args.min_size,
        max_file_size: args.max_size,
        group_by,
        check_duplicate_control_ids: args.dupe_control_id,
        worker_threads: args.jobs,
        record_filter,
        ..DirectoryScanConfig::default()
    };

    let cancel = AtomicBool::new(false);
    let report = scan_directory(&cfg, &cancel, |_| {}).map_err(|e| e.to_string())?;

    let formatted = match args.format {
        OutputFormat::Table => report.format_table().into_bytes(),
        OutputFormat::Csv => report.export_csv().map_err(|e| e.to_string())?,
        OutputFormat::Json => report.export_json().map_err(|e| e.to_string())?,
    };

    if let Some(ref out_path) = args.out {
        std::fs::write(out_path, &formatted)
            .map_err(|e| format!("failed to write '{}': {e}", out_path.display()))?;
    }

    let exit_code = if report.had_unreadable_files { 2 } else { 0 };
    Ok((formatted, exit_code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use std::path::Path;

    fn scan_fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/scan")
    }

    // SCAN-14, D-21: CLI scan argument parsing, table/csv/json formatting, and exit codes 0 and 1
    #[test]
    fn cli_scan_subcommand_formats_and_exit_codes() {
        let dir = scan_fixtures_dir();
        let cli = Cli::try_parse_from([
            "pipe_wrench_cli",
            "scan",
            dir.to_str().unwrap(),
            "--ext",
            "txt,dat,hl7",
            "--group-by",
            "type,app,date",
            "--format",
            "json",
            "--jobs",
            "2",
        ])
        .unwrap();

        let Commands::Scan(args) = cli.command else {
            panic!("expected Scan command");
        };
        let (bytes, code) = execute_scan_command(&args).unwrap();
        assert_eq!(code, 0);
        let json_str = String::from_utf8(bytes).unwrap();
        assert!(json_str.contains("\"msh_count\": 18"));

        // Non-existent directory returns Err -> CLI main exits with code 1 (SCAN-14)
        let bad_args = ScanArgs {
            dir: PathBuf::from("non_existent_dir_12345"),
            ext: vec!["txt".to_string()],
            recursive: true,
            no_recursive: false,
            include_archives: true,
            exclude: Vec::new(),
            min_size: None,
            max_size: None,
            group_by: None,
            check_batches: true,
            dupe_control_id: false,
            filter: None,
            filter_field: Vec::new(),
            ignore_case: true,
            regex: false,
            invert_match: false,
            format: OutputFormat::Table,
            out: None,
            jobs: Some(1),
            quiet: false,
        };
        assert!(execute_scan_command(&bad_args).is_err());
    }
}
