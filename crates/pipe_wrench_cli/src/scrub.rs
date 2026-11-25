//! Implementation of the `pipe_wrench_cli scrub` subcommand for deterministic,
//! referentially-consistent HL7 v2 PHI de-identification.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use hl7_core::{
    load_file, save_records_atomic, MllpEnvelope, PhiScrubber, Record, RecoveryMode, SaveOptions,
    SaveTerminator, ScannerConfig,
};

use crate::convert::CliTerminator;
use crate::filter::collect_input_files;

/// Arguments for `pipe_wrench_cli scrub`.
#[derive(Debug, Clone, Parser)]
#[command(after_help = hl7_core::PHI_SCRUB_DISCLAIMER)]
pub struct ScrubArgs {
    /// One or more input files or directories (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`).
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Destination output file path for synthetic HL7 records (written atomically).
    #[arg(long)]
    pub out: PathBuf,

    /// Optional deterministic seed string. Reusing the same seed across runs guarantees
    /// identical synthetic identities for the same patient MRN, name, and encounter.
    /// Omit to generate a fresh random seed per run.
    #[arg(long)]
    pub seed: Option<String>,

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

    /// Wrap each saved record in MLLP framing bytes (`0x0B ... 0x1C 0x0D`).
    #[arg(long, default_value_t = false)]
    pub envelope: bool,

    /// Additional custom HL7 path(s) to scrub (repeatable, e.g. `--phi-field ZPI-4 --phi-field OBX-5:redact`).
    /// Optional `:synthetic`, `:redact`, or `:mask` suffix selects the scrub mode (default: synthetic).
    #[arg(long = "phi-field")]
    pub phi_fields: Vec<String>,
}

/// Execute `pipe_wrench_cli scrub`, returning `(records_written, seed_used)`.
pub fn execute_scrub_command(args: &ScrubArgs) -> Result<(usize, String), String> {
    let recursive = args.recursive && !args.no_recursive;
    let files = collect_input_files(&args.inputs, recursive, &args.ext)?;

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };

    let mut all_records: Vec<Record> = Vec::new();
    for path in &files {
        let out = load_file(path, scanner_cfg, &cancel)
            .map_err(|e| format!("failed to load '{}': {e}", path.display()))?;
        all_records.extend(out.records);
    }

    let mut extra_rules = Vec::with_capacity(args.phi_fields.len());
    for spec in &args.phi_fields {
        extra_rules.push(hl7_core::CustomPhiRule::parse_cli_spec(spec)?);
    }
    let scrubber = match args
        .seed
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(seed) => PhiScrubber::new(seed),
        None => PhiScrubber::with_random_seed(),
    }
    .with_extra_paths(extra_rules);
    let used_seed = scrubber.seed().to_string();
    let scrubbed = scrubber.scrub_records(&all_records);

    let opts = SaveOptions {
        term_mode: SaveTerminator::from(args.term),
        disk_envelope: args.envelope.then(MllpEnvelope::default),
    };
    save_records_atomic(&args.out, &scrubbed, opts).map_err(|e| e.to_string())?;
    Ok((scrubbed.len(), used_seed))
}
