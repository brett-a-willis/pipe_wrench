//! Implementation of the `pipe_wrench_cli convert` subcommand (`FS-1..FS-7`, `SORT-1`, `EDIT-9`).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use hl7_core::{
    load_files, save_records_atomic, ChangeDelimitersCmd, Document, EditCommand,
    FilterMatchOptions, MllpEnvelope, RecordFilter, RecoveryMode, SaveOptions, SaveTerminator,
    ScannerConfig, SortDirection, SortRecordsCmd,
};

/// Segment terminator policy for `pipe_wrench_cli convert` (`FS-5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CliTerminator {
    /// Preserve detected source terminator (or CRLF if unknown).
    #[default]
    Preserve,
    /// Standard HL7 CR (`0x0D`).
    Cr,
    /// Windows CRLF (`0x0D 0x0A`).
    Crlf,
    /// Unix LF (`0x0A`).
    Lf,
}

impl From<CliTerminator> for SaveTerminator {
    fn from(val: CliTerminator) -> Self {
        match val {
            CliTerminator::Preserve => Self::Preserve,
            CliTerminator::Cr => Self::Cr,
            CliTerminator::Crlf => Self::CrLf,
            CliTerminator::Lf => Self::Lf,
        }
    }
}

/// Arguments for `pipe_wrench_cli convert` (`FS-1..FS-7`, `SORT-1`, `EDIT-9`).
#[derive(Debug, Clone, Parser)]
pub struct ConvertArgs {
    /// One or more input files (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`) to convert and concatenate.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Destination output file path (written atomically via `save_records_atomic`).
    #[arg(long)]
    pub out: PathBuf,

    /// Segment terminator normalization (`preserve`, `cr`, `crlf`, or `lf`) (`FS-5`).
    #[arg(long, value_enum, default_value_t = CliTerminator::Preserve)]
    pub term: CliTerminator,

    /// Wrap each saved record in MLLP framing bytes (`0x0B ... 0x1C 0x0D`) (`FS-4`).
    #[arg(long, default_value_t = false, conflicts_with = "strip_envelope")]
    pub envelope: bool,

    /// Strip MLLP framing bytes when saving (default behavior unless `--envelope` is set) (`FS-4`).
    #[arg(long, default_value_t = false)]
    pub strip_envelope: bool,

    /// Stable sort records ascending by `MSH-7` timestamp before saving (`SORT-1`).
    #[arg(long, default_value_t = false)]
    pub sort_msh7: bool,

    /// Translate all 5 HL7 delimiters across loaded records (e.g. `|^~\&` or `:^~\&`) (`EDIT-9`).
    #[arg(long)]
    pub delims: Option<String>,

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
}

/// Execute `pipe_wrench_cli convert`, returning the number of records written to `--out`.
pub fn execute_convert_command(args: &ConvertArgs) -> Result<usize, String> {
    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };
    let mut load_out =
        load_files(&args.files, scanner_cfg, &cancel, |_| {}).map_err(|e| e.to_string())?;

    let filter_opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: args.invert_match,
    };
    if let Some(filter) = RecordFilter::from_cli_specs_with_options(
        args.filter.as_deref(),
        &args.filter_field,
        filter_opts,
    )? {
        load_out.records.retain(|r| filter.matches_record(r));
    }

    let mut doc = Document::from_records(load_out.records);

    if let Some(ref new_delims) = args.delims {
        let mut cmd = ChangeDelimitersCmd::new(new_delims.as_bytes().to_vec());
        cmd.apply(&mut doc).map_err(|e| e.to_string())?;
    }

    if args.sort_msh7 {
        let mut cmd = SortRecordsCmd::by_msh7(SortDirection::Ascending);
        cmd.apply(&mut doc).map_err(|e| e.to_string())?;
    }

    let disk_envelope = if args.envelope && !args.strip_envelope {
        Some(MllpEnvelope::DEFAULT)
    } else {
        None
    };

    let save_opts = SaveOptions {
        term_mode: args.term.into(),
        disk_envelope,
    };

    save_records_atomic(&args.out, &doc.records, save_opts).map_err(|e| e.to_string())?;
    Ok(doc.len())
}
