//! Implementation of the `pipe_wrench_cli extract` subcommand for decoding and saving
//! Base64 / HL7 `ED` encapsulated attachments from HL7 v2 messages.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use hl7_core::{extract_record_attachments, load_file, RecoveryMode, ScannerConfig};

use crate::filter::collect_input_files;

/// Arguments for `pipe_wrench_cli extract`.
#[derive(Debug, Clone, Parser)]
pub struct ExtractArgs {
    /// One or more input files or directories (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`).
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Destination directory where decoded attachment files will be written.
    #[arg(long)]
    pub out: PathBuf,

    /// HL7 field path(s) to scan for Base64 / `ED` payloads (default: `OBX-5`; pass `*` to scan all fields).
    #[arg(long, value_delimiter = ',')]
    pub field: Vec<String>,

    /// Recurse into input directories (default: true).
    #[arg(long, default_value_t = true, conflicts_with = "no_recursive")]
    pub recursive: bool,

    /// Disable directory recursion.
    #[arg(long, default_value_t = false)]
    pub no_recursive: bool,

    /// Comma-separated extension patterns when traversing directories (default: `txt,dat,hl7,gz,zip`).
    #[arg(long, default_value = "txt,dat,hl7,gz,zip")]
    pub ext: String,
}

/// Execute `pipe_wrench_cli extract`, returning the list of written output file paths.
pub fn execute_extract_command(args: &ExtractArgs) -> Result<Vec<PathBuf>, String> {
    let recursive = args.recursive && !args.no_recursive;
    let files = collect_input_files(&args.inputs, recursive, &args.ext)?;

    std::fs::create_dir_all(&args.out).map_err(|e| {
        format!(
            "failed to create output directory '{}': {e}",
            args.out.display()
        )
    })?;

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };

    let mut written: Vec<PathBuf> = Vec::new();
    let mut name_counts: HashMap<String, usize> = HashMap::new();

    for path in &files {
        let out = load_file(path, scanner_cfg, &cancel)
            .map_err(|e| format!("failed to load '{}': {e}", path.display()))?;
        for (rec_idx, rec) in out.records.iter().enumerate() {
            let summary = rec.summary();
            let control_id = if summary.control_id.trim().is_empty() {
                format!("MSG{}", rec_idx + 1)
            } else {
                summary.control_id
            };
            let attachments = extract_record_attachments(rec, &args.field);
            for item in attachments {
                let base_name = item
                    .attachment
                    .suggested_filename(&control_id, &item.field_path);
                let count = name_counts.entry(base_name.clone()).or_insert(0);
                *count += 1;
                let final_name = if *count == 1 {
                    base_name
                } else if let Some((stem, ext)) = base_name.rsplit_once('.') {
                    format!("{stem}_{}.{ext}", *count)
                } else {
                    format!("{base_name}_{}", *count)
                };
                let dest = args.out.join(final_name);
                std::fs::write(&dest, &item.attachment.bytes)
                    .map_err(|e| format!("failed to write '{}': {e}", dest.display()))?;
                written.push(dest);
            }
        }
    }

    Ok(written)
}
