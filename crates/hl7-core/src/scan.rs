//! Headless streaming directory scanner and message counter (`SCAN-1..SCAN-13`, `D-3`, `D-5`, `D-20`, `D-21`).
//!
//! Uses [`crate::scanner::RecordBoundaryScanner`] as the single source of truth for record
//! boundary detection (`SCAN-3`). Never loads whole files into memory (`SCAN-2`) and never logs
//! message content (`§13`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use flate2::read::GzDecoder;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::address::FieldAddress;
use crate::delimiters::Delimiters;
use crate::error::CoreError;
use crate::filter::RecordFilter;
use crate::record::RecordKind;
use crate::scanner::{MllpEnvelope, RecordBoundaryScanner, RecoveryMode, ScanEvent, ScannerConfig};

/// Maximum line buffer per worker (`16 MiB`, well within the `≤ 64 MiB` worker budget of `SCAN-2`).
pub const SCAN_WORKER_MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Default maximum unique MSH-10 hashes tracked before marking duplicate counts approximate (`SCAN-12`).
pub const DEFAULT_MAX_TRACKED_CONTROL_IDS: usize = 500_000;

/// Combinable group-by keys for directory scanning (`SCAN-7`, `SCAN-8`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupByKey {
    /// Message type (`MSH-9`, e.g. `ADT^A01`) (`SCAN-7`).
    MessageType,
    /// Sending application and facility (`MSH-3/MSH-4`) (`SCAN-7`).
    SendingAppFacility,
    /// Receiving application and facility (`MSH-5/MSH-6`) (`SCAN-7`).
    ReceivingAppFacility,
    /// Message date (`MSH-7` truncated to day `YYYYMMDD`) (`SCAN-7`).
    MessageDate,
    /// Arbitrary field address in the first occurrence of its segment (`SCAN-8`, P2).
    CustomAddress(FieldAddress),
}

impl GroupByKey {
    /// Column header label for this group-by dimension.
    pub fn header_label(&self) -> String {
        match self {
            Self::MessageType => "MSH-9 (Type)".to_string(),
            Self::SendingAppFacility => "MSH-3/4 (Sending App/Fac)".to_string(),
            Self::ReceivingAppFacility => "MSH-5/6 (Receiving App/Fac)".to_string(),
            Self::MessageDate => "MSH-7 (Date)".to_string(),
            Self::CustomAddress(addr) => addr.to_string(),
        }
    }
}

/// Configuration for [`scan_directory`] (`SCAN-1`, `SCAN-7..SCAN-12`, `D-21`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryScanConfig {
    /// Root directory to scan (`SCAN-1`).
    pub root_dir: PathBuf,
    /// Extension/file patterns (default `["*.txt", "*.dat", "*.hl7"]`; bare extensions like `"txt"`
    /// are normalized to `"*.txt"` per `D-21`; `"*.*"` allowed) (`SCAN-1`).
    pub extension_patterns: Vec<String>,
    /// Recurse into subdirectories (default `true`, `SCAN-1`).
    pub recursive: bool,
    /// Optional exclude glob patterns (`SCAN-1`).
    pub exclude_globs: Vec<String>,
    /// Optional minimum file size in bytes (`SCAN-1`).
    pub min_file_size: Option<u64>,
    /// Optional maximum file size in bytes (`SCAN-1`).
    pub max_file_size: Option<u64>,
    /// Follow directory/file symlinks (default `false`, `SCAN-1`).
    pub follow_symlinks: bool,
    /// MLLP envelope markers (`FO-7`, `SCAN-3`).
    pub envelope: MllpEnvelope,
    /// Optional combinable group-by keys (`SCAN-7`, `SCAN-8`).
    pub group_by: Vec<GroupByKey>,
    /// Stream-decompress `.zip` and `.gz` archives (`SCAN-9`).
    pub scan_archives: bool,
    /// Count duplicate `MSH-10` control IDs within each file and across the scan (`SCAN-12`).
    pub check_duplicate_control_ids: bool,
    /// Maximum unique `MSH-10` hashes tracked before reporting approximate duplicates (`SCAN-12`).
    pub max_tracked_control_ids: usize,
    /// Number of parallel worker threads (`None` = `logical_cores - 1`, min 1, `SCAN-10`).
    pub worker_threads: Option<usize>,
    /// Optional record filter predicate applied to each scanned record (`D-26`).
    #[serde(default)]
    pub record_filter: Option<RecordFilter>,
}

impl Default for DirectoryScanConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("."),
            extension_patterns: vec![
                "*.txt".to_string(),
                "*.dat".to_string(),
                "*.hl7".to_string(),
            ],
            recursive: true,
            exclude_globs: Vec::new(),
            min_file_size: None,
            max_file_size: None,
            follow_symlinks: false,
            envelope: MllpEnvelope::DEFAULT,
            group_by: Vec::new(),
            scan_archives: true,
            check_duplicate_control_ids: false,
            max_tracked_control_ids: DEFAULT_MAX_TRACKED_CONTROL_IDS,
            worker_threads: None,
            record_filter: None,
        }
    }
}

/// Per-file scan status (`SCAN-4`, `SCAN-5`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileScanStatus {
    /// Valid HL7 records found with no anomalies.
    Ok,
    /// Zero HL7 records found (`"No HL7 found"`).
    NoHl7Found,
    /// Valid HL7 records found alongside non-HL7 bytes (`"Contains non-HL7 data"`).
    ContainsNonHl7Data,
    /// One or more `BTS-1` batch counts differed from actual batch messages (`"Batch count mismatch"`, `SCAN-5`).
    BatchCountMismatch,
    /// File could not be opened or decompressed (`"Unreadable"`).
    Unreadable(String),
    /// File was skipped (e.g. outside min/max size bounds) (`"Skipped"`).
    Skipped(String),
}

impl FileScanStatus {
    /// Canonical status label required by `SCAN-4` and `SCAN-5`.
    pub fn display_label(&self) -> String {
        match self {
            Self::Ok => "OK".to_string(),
            Self::NoHl7Found => "No HL7 found".to_string(),
            Self::ContainsNonHl7Data => "Contains non-HL7 data".to_string(),
            Self::BatchCountMismatch => "Batch count mismatch".to_string(),
            Self::Unreadable(reason) => format!("Unreadable ({reason})"),
            Self::Skipped(reason) => format!("Skipped ({reason})"),
        }
    }
}

/// Details of a batch message-count mismatch between `BTS-1` and actual `MSH` records (`SCAN-5`, `D-20`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMismatch {
    /// 1-based batch index within the file.
    pub batch_index: usize,
    /// Raw `BTS-1` value declared in the batch trailer.
    pub declared_bts1: String,
    /// Parsed numeric `BTS-1` value, if valid integer.
    pub declared_count: Option<u64>,
    /// Actual number of `MSH` records found in that batch.
    pub actual_count: u64,
}

/// Per-file (or per-zip-entry) scan result (`SCAN-4`, `SCAN-5`, `SCAN-9`, `SCAN-12`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileScanResult {
    /// Relative path from scan root (or `archive.zip!entry.txt` for zip entries, `SCAN-9`).
    pub display_path: String,
    /// Underlying filesystem path on disk (`SCAN-15`).
    pub filesystem_path: PathBuf,
    /// Optional zip entry name when this result came from inside a `.zip` archive (`SCAN-9`).
    pub zip_entry: Option<String>,
    /// Uncompressed/entry size in bytes (`SCAN-4`).
    pub size_bytes: u64,
    /// Last modified timestamp (Unix epoch seconds), if available (`SCAN-4`).
    pub modified_unix_secs: Option<i64>,
    /// Count of `MSH` message records (`SCAN-4`).
    pub msh_count: u64,
    /// Count of `FHS` records (`SCAN-4`).
    pub fhs_count: u64,
    /// Count of `BHS` records (`SCAN-4`).
    pub bhs_count: u64,
    /// Count of `BTS` records (`SCAN-4`).
    pub bts_count: u64,
    /// Count of `FTS` records (`SCAN-4`).
    pub fts_count: u64,
    /// Overall status of the file (`SCAN-4`, `SCAN-5`).
    pub status: FileScanStatus,
    /// `true` if non-HL7 bytes were observed outside HL7 records (`SCAN-4`).
    pub saw_non_hl7: bool,
    /// All batch count mismatches detected in this file (`SCAN-5`, `D-20`).
    pub batch_mismatches: Vec<BatchMismatch>,
    /// Number of duplicate `MSH-10` control IDs within this file (`SCAN-12`).
    pub duplicate_control_ids: u64,
    /// Per-file group-by bucket counts (`SCAN-7`, `SCAN-8`).
    #[serde(skip)]
    pub group_counts: HashMap<Vec<String>, u64>,
    /// Hashed `MSH-10` values seen in this file (when `check_duplicate_control_ids` is enabled).
    #[serde(skip)]
    pub control_id_hashes: Vec<u64>,
}

impl FileScanResult {
    /// Total top-level HL7 records (`MSH + FHS + BHS + BTS + FTS`) (`SCAN-3`).
    pub fn total_records(&self) -> u64 {
        self.msh_count + self.fhs_count + self.bhs_count + self.bts_count + self.fts_count
    }
}

/// Aggregated counts for grand total, extension rollups, and subdirectory rollups (`SCAN-6`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupCounts {
    /// Number of files/entries in this rollup.
    pub files: u64,
    /// Total bytes across files in this rollup.
    pub size_bytes: u64,
    /// Total `MSH` records.
    pub msh_count: u64,
    /// Total `FHS` records.
    pub fhs_count: u64,
    /// Total `BHS` records.
    pub bhs_count: u64,
    /// Total `BTS` records.
    pub bts_count: u64,
    /// Total `FTS` records.
    pub fts_count: u64,
    /// Number of files with batch count mismatches.
    pub batch_mismatch_files: u64,
}

impl RollupCounts {
    fn add_file(&mut self, res: &FileScanResult) {
        self.files += 1;
        self.size_bytes += res.size_bytes;
        self.msh_count += res.msh_count;
        self.fhs_count += res.fhs_count;
        self.bhs_count += res.bhs_count;
        self.bts_count += res.bts_count;
        self.fts_count += res.fts_count;
        if !res.batch_mismatches.is_empty() {
            self.batch_mismatch_files += 1;
        }
    }
}

/// One row in the group-by summary table (`SCAN-7`, `SCAN-8`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupByRow {
    /// Tuple of key values corresponding to `DirectoryScanConfig::group_by`.
    pub key_values: Vec<String>,
    /// Number of `MSH` messages matching this key combination.
    pub message_count: u64,
}

/// Live progress snapshot emitted during [`scan_directory`] (`SCAN-11`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    /// Number of candidate files processed so far.
    pub files_done: usize,
    /// Total candidate files discovered.
    pub files_total: usize,
    /// Cumulative bytes processed so far.
    pub bytes_done: u64,
    /// Total candidate bytes discovered.
    pub bytes_total: u64,
}

/// Complete output of a directory scan (`SCAN-4..SCAN-13`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryScanReport {
    /// Deterministically sorted per-file results (sorted by `display_path`, `SCAN-10`).
    pub files: Vec<FileScanResult>,
    /// Grand total across all scanned files (`SCAN-6`).
    pub grand_total: RollupCounts,
    /// Rollup by lowercase file extension (`SCAN-6`).
    pub by_extension: BTreeMap<String, RollupCounts>,
    /// Rollup by relative subdirectory (`SCAN-6`).
    pub by_subdirectory: BTreeMap<String, RollupCounts>,
    /// Group-by header labels (`SCAN-7`, `SCAN-8`).
    pub group_by_headers: Vec<String>,
    /// Deterministically sorted group-by rows (`SCAN-7`, `SCAN-8`).
    pub group_by_rows: Vec<GroupByRow>,
    /// Total duplicate `MSH-10` control IDs across the entire scan (`SCAN-12`).
    pub duplicate_control_ids_total: u64,
    /// `true` if the unique control-ID cap was reached (`"approximate"`, `SCAN-12`).
    pub duplicate_control_ids_approximate: bool,
    /// `true` if at least one file had `FileScanStatus::Unreadable(_)` (CLI exit code `2`, `SCAN-14`).
    pub had_unreadable_files: bool,
    /// `true` if the scan was canceled early (`SCAN-11`).
    pub canceled: bool,
}

/// Normalize a user-supplied extension or glob pattern (`D-21`).
///
/// Bare extensions like `"txt"` or `".txt"` become `"*.txt"`; `"*.*"` becomes `"*"`.
pub fn normalize_extension_glob(pattern: &str) -> String {
    let trimmed = pattern.trim();
    if trimmed.is_empty() || trimmed == "*.*" || trimmed == "*" {
        return "*".to_string();
    }
    if trimmed.contains('*') || trimmed.contains('?') {
        return trimmed.to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix('.') {
        format!("*.{stripped}")
    } else {
        format!("*.{trimmed}")
    }
}

/// Pending group-by extraction state for the active `MSH` message (`SCAN-8`).
type PendingMshGroup = (Delimiters, Vec<u8>, HashMap<[u8; 3], Vec<u8>>);
/// Pending streaming record lines when `config.record_filter` is active (`D-26`).
type PendingFilteredRecord = (RecordKind, Delimiters, Vec<Vec<u8>>);

/// Stream-scan a single [`BufRead`] using [`RecordBoundaryScanner`] (`SCAN-2`, `SCAN-3`).
#[allow(clippy::too_many_arguments)]
pub fn scan_reader<R: BufRead>(
    reader: R,
    display_path: String,
    filesystem_path: PathBuf,
    zip_entry: Option<String>,
    size_bytes: u64,
    modified_unix_secs: Option<i64>,
    config: &DirectoryScanConfig,
    cancel: &AtomicBool,
) -> Result<FileScanResult, CoreError> {
    let scanner_cfg = ScannerConfig {
        envelope: config.envelope,
        recovery_mode: RecoveryMode::RecoverNonHl7,
        max_line_bytes: SCAN_WORKER_MAX_LINE_BYTES,
    };
    let mut scanner = RecordBoundaryScanner::new(reader, scanner_cfg);

    let mut msh_count = 0u64;
    let mut fhs_count = 0u64;
    let mut bhs_count = 0u64;
    let mut bts_count = 0u64;
    let mut fts_count = 0u64;

    let mut batch_index = 0usize;
    let mut in_batch = false;
    let mut batch_msh_count = 0u64;
    let mut batch_mismatches = Vec::new();

    let mut group_counts: HashMap<Vec<String>, u64> = HashMap::new();
    let has_group_by = !config.group_by.is_empty();
    let custom_segments_needed: Vec<[u8; 3]> = config
        .group_by
        .iter()
        .filter_map(|k| match k {
            GroupByKey::CustomAddress(addr) if addr.segment != *b"MSH" => Some(addr.segment),
            _ => None,
        })
        .collect();

    // Pending group-by values for the current MSH message when custom non-MSH segments are requested (SCAN-8).
    let mut pending_msh_group: Option<PendingMshGroup> = None;
    let mut pending_filtered: Option<PendingFilteredRecord> = None;

    let mut seen_hashes_in_file: HashSet<u64> = HashSet::new();
    let mut control_id_hashes: Vec<u64> = Vec::new();
    let mut duplicate_control_ids = 0u64;

    let flush_pending_group =
        |pending: &mut Option<PendingMshGroup>, group_counts: &mut HashMap<Vec<String>, u64>| {
            if let Some((delims, msh_line, custom_map)) = pending.take() {
                let key = evaluate_group_by_keys(&config.group_by, delims, &msh_line, &custom_map);
                *group_counts.entry(key).or_insert(0) += 1;
            }
        };

    let summary;

    if let Some(ref filter) = config.record_filter {
        let mut flush_filtered = |pending: &mut Option<PendingFilteredRecord>| {
            let Some((kind, delimiters, lines)) = pending.take() else {
                return;
            };
            if !filter.matches_raw_segments(delimiters, &lines) {
                return;
            }
            let header_line = &lines[0];
            match kind {
                RecordKind::Msh => {
                    msh_count += 1;
                    batch_msh_count += 1;
                    if config.check_duplicate_control_ids {
                        if let Some(msh10) = extract_msh_field(header_line, delimiters, 10) {
                            if !msh10.is_empty() {
                                let h = hash_bytes(msh10);
                                if !seen_hashes_in_file.insert(h) {
                                    duplicate_control_ids += 1;
                                }
                                if control_id_hashes.len() < config.max_tracked_control_ids {
                                    control_id_hashes.push(h);
                                }
                            }
                        }
                    }
                    if has_group_by {
                        let mut seg_map = HashMap::new();
                        for line in lines.iter().skip(1) {
                            if line.len() >= 3 {
                                let tag = [line[0], line[1], line[2]];
                                if custom_segments_needed.contains(&tag)
                                    && !seg_map.contains_key(&tag)
                                {
                                    seg_map.insert(tag, line.clone());
                                }
                            }
                        }
                        let key = evaluate_group_by_keys(
                            &config.group_by,
                            delimiters,
                            header_line,
                            &seg_map,
                        );
                        *group_counts.entry(key).or_insert(0) += 1;
                    }
                }
                RecordKind::Fhs => fhs_count += 1,
                RecordKind::Bhs => {
                    bhs_count += 1;
                    batch_index += 1;
                    in_batch = true;
                    batch_msh_count = 0;
                }
                RecordKind::Bts => {
                    bts_count += 1;
                    if !in_batch {
                        batch_index += 1;
                    }
                    in_batch = false;
                    batch_msh_count = 0;
                }
                RecordKind::Fts => fts_count += 1,
            }
        };

        summary = scanner.scan_all(cancel, |event| match event {
            ScanEvent::RecordStart {
                kind,
                delimiters,
                header_line,
                ..
            } => {
                flush_filtered(&mut pending_filtered);
                pending_filtered = Some((kind, delimiters, vec![header_line.to_vec()]));
                ControlFlow::Continue(())
            }
            ScanEvent::Segment { line, .. } => {
                if let Some((_, _, ref mut lines)) = pending_filtered {
                    lines.push(line.to_vec());
                }
                ControlFlow::Continue(())
            }
            ScanEvent::RecordEnd { .. } => {
                flush_filtered(&mut pending_filtered);
                ControlFlow::Continue(())
            }
            ScanEvent::NonHl7Data { .. } => ControlFlow::Continue(()),
        })?;
        flush_filtered(&mut pending_filtered);
    } else {
        summary = scanner.scan_all(cancel, |event| match event {
            ScanEvent::RecordStart {
                kind,
                delimiters,
                header_line,
                ..
            } => {
                flush_pending_group(&mut pending_msh_group, &mut group_counts);

                match kind {
                    RecordKind::Msh => {
                        msh_count += 1;
                        batch_msh_count += 1;

                        if config.check_duplicate_control_ids {
                            if let Some(msh10) = extract_msh_field(header_line, delimiters, 10) {
                                if !msh10.is_empty() {
                                    let h = hash_bytes(msh10);
                                    if !seen_hashes_in_file.insert(h) {
                                        duplicate_control_ids += 1;
                                    }
                                    if control_id_hashes.len() < config.max_tracked_control_ids {
                                        control_id_hashes.push(h);
                                    }
                                }
                            }
                        }

                        if has_group_by {
                            if custom_segments_needed.is_empty() {
                                let empty_map = HashMap::new();
                                let key = evaluate_group_by_keys(
                                    &config.group_by,
                                    delimiters,
                                    header_line,
                                    &empty_map,
                                );
                                *group_counts.entry(key).or_insert(0) += 1;
                            } else {
                                pending_msh_group =
                                    Some((delimiters, header_line.to_vec(), HashMap::new()));
                            }
                        }
                    }
                    RecordKind::Fhs => {
                        fhs_count += 1;
                    }
                    RecordKind::Bhs => {
                        bhs_count += 1;
                        batch_index += 1;
                        in_batch = true;
                        batch_msh_count = 0;
                    }
                    RecordKind::Bts => {
                        bts_count += 1;
                        if !in_batch {
                            batch_index += 1;
                        }
                        // SCAN-5, D-20: Check BTS-1 against actual MSH count in this batch.
                        if let Some(bts1_bytes) = extract_regular_field(header_line, delimiters, 1)
                        {
                            let bts1_str = String::from_utf8_lossy(bts1_bytes).trim().to_string();
                            if !bts1_str.is_empty() {
                                match bts1_str.parse::<u64>() {
                                    Ok(expected) if expected != batch_msh_count => {
                                        batch_mismatches.push(BatchMismatch {
                                            batch_index,
                                            declared_bts1: bts1_str,
                                            declared_count: Some(expected),
                                            actual_count: batch_msh_count,
                                        });
                                    }
                                    Err(_) => {
                                        batch_mismatches.push(BatchMismatch {
                                            batch_index,
                                            declared_bts1: bts1_str,
                                            declared_count: None,
                                            actual_count: batch_msh_count,
                                        });
                                    }
                                    Ok(_) => {}
                                }
                            }
                        }
                        in_batch = false;
                        batch_msh_count = 0;
                    }
                    RecordKind::Fts => {
                        fts_count += 1;
                    }
                }
                ControlFlow::Continue(())
            }
            ScanEvent::Segment { line, .. } => {
                if let Some((_, _, ref mut seg_map)) = pending_msh_group {
                    if line.len() >= 3 {
                        let tag = [line[0], line[1], line[2]];
                        if custom_segments_needed.contains(&tag) && !seg_map.contains_key(&tag) {
                            seg_map.insert(tag, line.to_vec());
                        }
                    }
                }
                ControlFlow::Continue(())
            }
            ScanEvent::RecordEnd { .. } => {
                flush_pending_group(&mut pending_msh_group, &mut group_counts);
                ControlFlow::Continue(())
            }
            ScanEvent::NonHl7Data { .. } => ControlFlow::Continue(()),
        })?;
        flush_pending_group(&mut pending_msh_group, &mut group_counts);
    }

    let total_recs = msh_count + fhs_count + bhs_count + bts_count + fts_count;
    let status = if !batch_mismatches.is_empty() {
        FileScanStatus::BatchCountMismatch
    } else if total_recs == 0 {
        FileScanStatus::NoHl7Found
    } else if summary.saw_non_hl7 {
        FileScanStatus::ContainsNonHl7Data
    } else {
        FileScanStatus::Ok
    };

    Ok(FileScanResult {
        display_path,
        filesystem_path,
        zip_entry,
        size_bytes: size_bytes.max(summary.bytes_read),
        modified_unix_secs,
        msh_count,
        fhs_count,
        bhs_count,
        bts_count,
        fts_count,
        status,
        saw_non_hl7: summary.saw_non_hl7,
        batch_mismatches,
        duplicate_control_ids,
        group_counts,
        control_id_hashes,
    })
}

/// Execute a parallel, streaming directory scan (`SCAN-1..SCAN-13`).
pub fn scan_directory<F>(
    config: &DirectoryScanConfig,
    cancel: &AtomicBool,
    on_progress: F,
) -> Result<DirectoryScanReport, CoreError>
where
    F: Fn(ScanProgress) + Sync + Send,
{
    if !config.root_dir.exists() || !config.root_dir.is_dir() {
        return Err(CoreError::Io(crate::error::IoError::FileIo {
            path: config.root_dir.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("directory '{}' does not exist", config.root_dir.display()),
            ),
        }));
    }

    let ext_globset = build_extension_globset(&config.extension_patterns)?;
    let exclude_globset = build_exclude_globset(&config.exclude_globs)?;

    // Discover candidate files on disk.
    let mut walker = WalkDir::new(&config.root_dir).follow_links(config.follow_symlinks);
    if !config.recursive {
        walker = walker.max_depth(1);
    }

    let mut candidates: Vec<(PathBuf, String, Option<std::fs::Metadata>)> = Vec::new();
    for entry_res in walker {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry_res else {
            continue;
        };
        if entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let rel = path
            .strip_prefix(&config.root_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if exclude_globset.is_match(&rel)
            || path
                .file_name()
                .is_some_and(|n| exclude_globset.is_match(n))
        {
            continue;
        }

        let is_zip = config.scan_archives
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"));

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        // Include file if it matches extension filters OR if it's a .zip archive whose inner entries may match (SCAN-9).
        if !is_zip && !matches_extension_filter(file_name, &ext_globset) {
            continue;
        }

        let meta = entry.metadata().ok();
        candidates.push((path, rel, meta));
    }

    let files_total = candidates.len();
    let bytes_total: u64 = candidates
        .iter()
        .filter_map(|(_, _, m)| m.as_ref().map(std::fs::Metadata::len))
        .sum();

    let files_done = AtomicUsize::new(0);
    let bytes_done = AtomicU64::new(0);

    let default_threads = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1);
    let num_threads = config.worker_threads.unwrap_or(default_threads).max(1);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .map_err(|e| CoreError::Dictionary(format!("rayon pool error: {e}")))?;

    let nested_results: Vec<Vec<FileScanResult>> = pool.install(|| {
        candidates
            .par_iter()
            .map(|(path, rel, meta)| {
                if cancel.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                let f_size = meta.as_ref().map_or(0, std::fs::Metadata::len);
                let mod_secs = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);

                let res = scan_candidate_file(
                    path,
                    rel,
                    f_size,
                    mod_secs,
                    config,
                    &ext_globset,
                    &exclude_globset,
                    cancel,
                );

                let done_f = files_done.fetch_add(1, Ordering::Relaxed) + 1;
                let done_b = bytes_done.fetch_add(f_size, Ordering::Relaxed) + f_size;
                on_progress(ScanProgress {
                    files_done: done_f,
                    files_total,
                    bytes_done: done_b,
                    bytes_total,
                });

                res
            })
            .collect()
    });

    let mut files: Vec<FileScanResult> = nested_results.into_iter().flatten().collect();
    // SCAN-10: Deterministic output ordering sorted by path regardless of worker timing.
    files.sort_by(|a, b| a.display_path.cmp(&b.display_path));

    Ok(aggregate_scan_report(
        files,
        config,
        cancel.load(Ordering::Relaxed),
    ))
}

#[allow(clippy::too_many_arguments)]
fn scan_candidate_file(
    path: &Path,
    rel_display: &str,
    size_bytes: u64,
    modified_unix_secs: Option<i64>,
    config: &DirectoryScanConfig,
    ext_globset: &GlobSet,
    exclude_globset: &GlobSet,
    cancel: &AtomicBool,
) -> Vec<FileScanResult> {
    if let Some(min_sz) = config.min_file_size {
        if size_bytes < min_sz {
            return vec![skipped_result(
                rel_display,
                path,
                size_bytes,
                modified_unix_secs,
                format!("under min size ({size_bytes} < {min_sz})"),
            )];
        }
    }
    if let Some(max_sz) = config.max_file_size {
        if size_bytes > max_sz {
            return vec![skipped_result(
                rel_display,
                path,
                size_bytes,
                modified_unix_secs,
                format!("over max size ({size_bytes} > {max_sz})"),
            )];
        }
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();

    // SCAN-9: Transparent streaming .zip archive scanning (`archive.zip!entry.txt`).
    if config.scan_archives && ext.eq_ignore_ascii_case("zip") {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                return vec![unreadable_result(
                    rel_display,
                    path,
                    size_bytes,
                    modified_unix_secs,
                    e.to_string(),
                )];
            }
        };
        let mut archive = match ZipArchive::new(file) {
            Ok(a) => a,
            Err(e) => {
                return vec![unreadable_result(
                    rel_display,
                    path,
                    size_bytes,
                    modified_unix_secs,
                    e.to_string(),
                )];
            }
        };

        let mut zip_results = Vec::new();
        for i in 0..archive.len() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(entry) = archive.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let entry_name = entry.name().replace('\\', "/");
            let entry_base = Path::new(&entry_name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&entry_name);

            if exclude_globset.is_match(&entry_name)
                || !matches_extension_filter(entry_base, ext_globset)
            {
                continue;
            }

            let entry_size = entry.size();
            let display = format!("{rel_display}!{entry_name}");
            let res = match scan_reader(
                BufReader::new(entry),
                display.clone(),
                path.to_path_buf(),
                Some(entry_name),
                entry_size,
                modified_unix_secs,
                config,
                cancel,
            ) {
                Ok(r) => r,
                Err(e) => unreadable_result(
                    &display,
                    path,
                    entry_size,
                    modified_unix_secs,
                    e.to_string(),
                ),
            };
            zip_results.push(res);
        }
        return zip_results;
    }

    // SCAN-9: Transparent streaming .gz decompression.
    if config.scan_archives && ext.eq_ignore_ascii_case("gz") {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                return vec![unreadable_result(
                    rel_display,
                    path,
                    size_bytes,
                    modified_unix_secs,
                    e.to_string(),
                )];
            }
        };
        let decoder = GzDecoder::new(file);
        let res = match scan_reader(
            BufReader::new(decoder),
            rel_display.to_string(),
            path.to_path_buf(),
            None,
            size_bytes,
            modified_unix_secs,
            config,
            cancel,
        ) {
            Ok(r) => r,
            Err(e) => unreadable_result(
                rel_display,
                path,
                size_bytes,
                modified_unix_secs,
                e.to_string(),
            ),
        };
        return vec![res];
    }

    // Regular file on disk.
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return vec![unreadable_result(
                rel_display,
                path,
                size_bytes,
                modified_unix_secs,
                e.to_string(),
            )];
        }
    };
    let res = match scan_reader(
        BufReader::new(file),
        rel_display.to_string(),
        path.to_path_buf(),
        None,
        size_bytes,
        modified_unix_secs,
        config,
        cancel,
    ) {
        Ok(r) => r,
        Err(e) => unreadable_result(
            rel_display,
            path,
            size_bytes,
            modified_unix_secs,
            e.to_string(),
        ),
    };
    vec![res]
}

fn aggregate_scan_report(
    mut files: Vec<FileScanResult>,
    config: &DirectoryScanConfig,
    canceled: bool,
) -> DirectoryScanReport {
    let mut grand_total = RollupCounts::default();
    let mut by_extension: BTreeMap<String, RollupCounts> = BTreeMap::new();
    let mut by_subdirectory: BTreeMap<String, RollupCounts> = BTreeMap::new();
    let mut combined_groups: BTreeMap<Vec<String>, u64> = BTreeMap::new();
    let mut global_seen_hashes: HashSet<u64> = HashSet::new();
    let mut duplicate_control_ids_total = 0u64;
    let mut duplicate_control_ids_approximate = false;
    let mut had_unreadable_files = false;

    for f in &mut files {
        if matches!(f.status, FileScanStatus::Unreadable(_)) {
            had_unreadable_files = true;
        }
        grand_total.add_file(f);

        let ext_key = extract_extension_key(&f.display_path);
        by_extension.entry(ext_key).or_default().add_file(f);

        let subdir_key = extract_subdir_key(&f.display_path);
        by_subdirectory.entry(subdir_key).or_default().add_file(f);

        for (k, count) in f.group_counts.drain() {
            *combined_groups.entry(k).or_insert(0) += count;
        }

        if config.check_duplicate_control_ids {
            for h in f.control_id_hashes.drain(..) {
                if global_seen_hashes.contains(&h) {
                    duplicate_control_ids_total += 1;
                } else if global_seen_hashes.len() < config.max_tracked_control_ids {
                    global_seen_hashes.insert(h);
                } else {
                    duplicate_control_ids_approximate = true;
                }
            }
        }
    }

    let group_by_headers: Vec<String> = config
        .group_by
        .iter()
        .map(GroupByKey::header_label)
        .collect();
    let group_by_rows: Vec<GroupByRow> = combined_groups
        .into_iter()
        .map(|(key_values, message_count)| GroupByRow {
            key_values,
            message_count,
        })
        .collect();

    DirectoryScanReport {
        files,
        grand_total,
        by_extension,
        by_subdirectory,
        group_by_headers,
        group_by_rows,
        duplicate_control_ids_total,
        duplicate_control_ids_approximate,
        had_unreadable_files,
        canceled,
    }
}

impl DirectoryScanReport {
    /// Format the scan report as a human-readable ASCII table (`SCAN-13`, `SCAN-14`).
    pub fn format_table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{:<36} {:>10} {:>8} {:>6} {:>6} {:>6} {:>6}  {}\r\n",
            "Path", "Bytes", "MSH", "FHS", "BHS", "BTS", "FTS", "Status"
        ));
        out.push_str(&"-".repeat(104));
        out.push_str("\r\n");

        for f in &self.files {
            let mut status_text = f.status.display_label();
            if !f.batch_mismatches.is_empty() {
                let details: Vec<String> = f
                    .batch_mismatches
                    .iter()
                    .map(|m| format!("BTS-1={} vs actual={}", m.declared_bts1, m.actual_count))
                    .collect();
                status_text = format!("{status_text} ({})", details.join(", "));
            }
            out.push_str(&format!(
                "{:<36} {:>10} {:>8} {:>6} {:>6} {:>6} {:>6}  {}\r\n",
                f.display_path,
                f.size_bytes,
                f.msh_count,
                f.fhs_count,
                f.bhs_count,
                f.bts_count,
                f.fts_count,
                status_text
            ));
        }

        out.push_str(&"-".repeat(104));
        out.push_str("\r\n");
        out.push_str(&format!(
            "{:<36} {:>10} {:>8} {:>6} {:>6} {:>6} {:>6}  ({} files)\r\n",
            "GRAND TOTAL",
            self.grand_total.size_bytes,
            self.grand_total.msh_count,
            self.grand_total.fhs_count,
            self.grand_total.bhs_count,
            self.grand_total.bts_count,
            self.grand_total.fts_count,
            self.grand_total.files
        ));

        if !self.group_by_rows.is_empty() {
            out.push_str("\r\nGroup-By Summary (");
            out.push_str(&self.group_by_headers.join(", "));
            out.push_str("):\r\n");
            for row in &self.group_by_rows {
                out.push_str(&format!(
                    "  {} -> {}\r\n",
                    row.key_values.join(" | "),
                    row.message_count
                ));
            }
        }

        out
    }

    /// Export per-file scan results to RFC 4180 CSV (`SCAN-13`, `SCAN-14`).
    pub fn export_csv(&self) -> Result<Vec<u8>, CoreError> {
        let mut wtr = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(Vec::new());

        wtr.write_record([
            "path",
            "size_bytes",
            "msh_count",
            "fhs_count",
            "bhs_count",
            "bts_count",
            "fts_count",
            "status",
            "batch_mismatches",
            "duplicate_control_ids",
        ])
        .map_err(|e| CoreError::Dictionary(e.to_string()))?;

        for f in &self.files {
            let mismatch_str = f
                .batch_mismatches
                .iter()
                .map(|m| format!("BTS-1={} actual={}", m.declared_bts1, m.actual_count))
                .collect::<Vec<_>>()
                .join("; ");

            wtr.write_record([
                &f.display_path,
                &f.size_bytes.to_string(),
                &f.msh_count.to_string(),
                &f.fhs_count.to_string(),
                &f.bhs_count.to_string(),
                &f.bts_count.to_string(),
                &f.fts_count.to_string(),
                &f.status.display_label(),
                &mismatch_str,
                &f.duplicate_control_ids.to_string(),
            ])
            .map_err(|e| CoreError::Dictionary(e.to_string()))?;
        }

        wtr.into_inner()
            .map_err(|e| CoreError::Dictionary(e.to_string()))
    }

    /// Export the complete scan report (files, rollups, group-by) to pretty JSON (`SCAN-13`, `SCAN-14`).
    pub fn export_json(&self) -> Result<Vec<u8>, CoreError> {
        serde_json::to_vec_pretty(self).map_err(|e| CoreError::Dictionary(e.to_string()))
    }
}

fn evaluate_group_by_keys(
    keys: &[GroupByKey],
    delims: Delimiters,
    msh_line: &[u8],
    custom_segments: &HashMap<[u8; 3], Vec<u8>>,
) -> Vec<String> {
    keys.iter()
        .map(|k| match k {
            GroupByKey::MessageType => {
                let raw = extract_msh_field(msh_line, delims, 9).unwrap_or(b"");
                let comps: Vec<&[u8]> = raw.split(|&b| b == delims.component).collect();
                match (comps.first(), comps.get(1)) {
                    (Some(c1), Some(c2)) if !c2.is_empty() => format!(
                        "{}^{}",
                        String::from_utf8_lossy(c1),
                        String::from_utf8_lossy(c2)
                    ),
                    (Some(c1), _) => String::from_utf8_lossy(c1).into_owned(),
                    _ => String::new(),
                }
            }
            GroupByKey::SendingAppFacility => {
                let app = extract_msh_field(msh_line, delims, 3).unwrap_or(b"");
                let fac = extract_msh_field(msh_line, delims, 4).unwrap_or(b"");
                format!(
                    "{}/{}",
                    String::from_utf8_lossy(app),
                    String::from_utf8_lossy(fac)
                )
            }
            GroupByKey::ReceivingAppFacility => {
                let app = extract_msh_field(msh_line, delims, 5).unwrap_or(b"");
                let fac = extract_msh_field(msh_line, delims, 6).unwrap_or(b"");
                format!(
                    "{}/{}",
                    String::from_utf8_lossy(app),
                    String::from_utf8_lossy(fac)
                )
            }
            GroupByKey::MessageDate => {
                let ts = extract_msh_field(msh_line, delims, 7).unwrap_or(b"");
                let day_slice = &ts[..ts.len().min(8)];
                String::from_utf8_lossy(day_slice).into_owned()
            }
            GroupByKey::CustomAddress(addr) => {
                let seg_bytes = if addr.segment == *b"MSH" {
                    Some(msh_line)
                } else {
                    custom_segments.get(&addr.segment).map(Vec::as_slice)
                };
                let Some(seg) = seg_bytes else {
                    return String::new();
                };
                extract_address_from_segment_line(seg, delims, addr)
            }
        })
        .collect()
}

fn extract_msh_field(msh_line: &[u8], delims: Delimiters, field_num: u16) -> Option<&[u8]> {
    if msh_line.len() < 4 {
        return None;
    }
    match field_num {
        1 => msh_line.get(3..4),
        2 => msh_line.get(4..8),
        n if n >= 3 => msh_line
            .get(4..)?
            .split(|&b| b == delims.field)
            .nth(usize::from(n - 2)),
        _ => None,
    }
}

fn extract_regular_field(seg_line: &[u8], delims: Delimiters, field_num: u16) -> Option<&[u8]> {
    if seg_line.len() < 4 || seg_line[3] != delims.field || field_num == 0 {
        return None;
    }
    seg_line[4..]
        .split(|&b| b == delims.field)
        .nth(usize::from(field_num - 1))
}

fn extract_address_from_segment_line(
    seg: &[u8],
    delims: Delimiters,
    addr: &FieldAddress,
) -> String {
    let is_header = addr.segment == *b"MSH" || addr.segment == *b"FHS" || addr.segment == *b"BHS";
    let field_slice = if is_header {
        extract_msh_field(seg, delims, addr.field.get())
    } else {
        extract_regular_field(seg, delims, addr.field.get())
    };
    let Some(f_bytes) = field_slice else {
        return String::new();
    };
    let rep_idx = usize::from(addr.effective_repetition().get() - 1);
    let rep_bytes = if addr.repetition.is_some() || addr.component.is_some() {
        let Some(r) = f_bytes.split(|&b| b == delims.repetition).nth(rep_idx) else {
            return String::new();
        };
        r
    } else {
        f_bytes
    };
    let comp_bytes = match addr.component {
        Some(c) => {
            let Some(cb) = rep_bytes
                .split(|&b| b == delims.component)
                .nth(usize::from(c.get() - 1))
            else {
                return String::new();
            };
            cb
        }
        None => return String::from_utf8_lossy(rep_bytes).into_owned(),
    };
    match addr.subcomponent {
        Some(s) => comp_bytes
            .split(|&b| b == delims.subcomponent)
            .nth(usize::from(s.get() - 1))
            .map_or_else(String::new, |sb| String::from_utf8_lossy(sb).into_owned()),
        None => String::from_utf8_lossy(comp_bytes).into_owned(),
    }
}

fn build_extension_globset(patterns: &[String]) -> Result<GlobSet, CoreError> {
    let mut builder = GlobSetBuilder::new();
    let effective = if patterns.is_empty() {
        vec!["*".to_string()]
    } else {
        patterns
            .iter()
            .map(|p| normalize_extension_glob(p))
            .collect()
    };
    for pat in effective {
        let glob = GlobBuilder::new(&pat)
            .case_insensitive(true)
            .build()
            .map_err(|e| CoreError::Dictionary(format!("invalid extension glob '{pat}': {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| CoreError::Dictionary(format!("globset error: {e}")))
}

fn build_exclude_globset(patterns: &[String]) -> Result<GlobSet, CoreError> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let trimmed = pat.trim();
        if trimmed.is_empty() {
            continue;
        }
        let glob = GlobBuilder::new(trimmed)
            .case_insensitive(true)
            .build()
            .map_err(|e| CoreError::Dictionary(format!("invalid exclude glob '{trimmed}': {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| CoreError::Dictionary(format!("globset error: {e}")))
}

fn matches_extension_filter(file_name: &str, ext_globset: &GlobSet) -> bool {
    if ext_globset.is_match(file_name) {
        return true;
    }
    // For `.gz` files like `msg.hl7.gz`, also match the inner stem `msg.hl7` against `*.hl7`.
    if let Some(stem) = file_name
        .strip_suffix(".gz")
        .or_else(|| file_name.strip_suffix(".GZ"))
    {
        return ext_globset.is_match(stem);
    }
    false
}

fn extract_extension_key(display_path: &str) -> String {
    let target = display_path
        .rsplit_once('!')
        .map_or(display_path, |(_, entry)| entry);
    Path::new(target)
        .extension()
        .and_then(|e| e.to_str())
        .map_or_else(
            || "(none)".to_string(),
            |e| format!(".{}", e.to_ascii_lowercase()),
        )
}

fn extract_subdir_key(display_path: &str) -> String {
    let outer = display_path
        .split_once('!')
        .map_or(display_path, |(arch, _)| arch);
    match outer.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => ".".to_string(),
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn unreadable_result(
    display_path: &str,
    fs_path: &Path,
    size_bytes: u64,
    modified_unix_secs: Option<i64>,
    reason: String,
) -> FileScanResult {
    FileScanResult {
        display_path: display_path.to_string(),
        filesystem_path: fs_path.to_path_buf(),
        zip_entry: None,
        size_bytes,
        modified_unix_secs,
        msh_count: 0,
        fhs_count: 0,
        bhs_count: 0,
        bts_count: 0,
        fts_count: 0,
        status: FileScanStatus::Unreadable(reason),
        saw_non_hl7: false,
        batch_mismatches: Vec::new(),
        duplicate_control_ids: 0,
        group_counts: HashMap::new(),
        control_id_hashes: Vec::new(),
    }
}

fn skipped_result(
    display_path: &str,
    fs_path: &Path,
    size_bytes: u64,
    modified_unix_secs: Option<i64>,
    reason: String,
) -> FileScanResult {
    FileScanResult {
        display_path: display_path.to_string(),
        filesystem_path: fs_path.to_path_buf(),
        zip_entry: None,
        size_bytes,
        modified_unix_secs,
        msh_count: 0,
        fhs_count: 0,
        bhs_count: 0,
        bts_count: 0,
        fts_count: 0,
        status: FileScanStatus::Skipped(reason),
        saw_non_hl7: false,
        batch_mismatches: Vec::new(),
        duplicate_control_ids: 0,
        group_counts: HashMap::new(),
        control_id_hashes: Vec::new(),
    }
}
