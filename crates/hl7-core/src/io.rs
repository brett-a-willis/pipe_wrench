//! File loading, multi-file concatenation, transparent `.gz`/`.zip` decompression,
//! clipboard byte loading, and atomic file saving (FO-1..FO-5, FO-13..FO-15, FS-1..FS-8).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::GzDecoder;
use tempfile::NamedTempFile;
use zip::ZipArchive;

use crate::error::IoError;
use crate::record::{Record, SaveTerminator, SourceTerminator};
use crate::scanner::{
    load_records_from_reader, load_records_from_reader_with_limit, LoadOutput, MllpEnvelope,
    ScannerConfig,
};

/// User-editable list of file open/save dialog filter patterns (FO-1, FO-2, FS-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilterList {
    patterns: Vec<String>,
    active_index: usize,
}

impl Default for FileFilterList {
    fn default() -> Self {
        Self {
            patterns: vec![
                "*.*".to_string(),
                "*.dat".to_string(),
                "*.txt".to_string(),
                "*.hl7".to_string(),
            ],
            active_index: 0,
        }
    }
}

impl FileFilterList {
    /// Return all configured filter patterns.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Return the currently active filter pattern.
    pub fn active_pattern(&self) -> &str {
        self.patterns
            .get(self.active_index)
            .map(String::as_str)
            .unwrap_or("*.*")
    }

    /// Select the active filter by index.
    pub fn set_active_index(&mut self, index: usize) -> bool {
        if index < self.patterns.len() {
            self.active_index = index;
            true
        } else {
            false
        }
    }

    /// Add a new filter pattern. Unlimited depth; rejects empty or duplicate patterns (FO-2).
    pub fn add_pattern(&mut self, pattern: impl Into<String>) -> Result<(), IoError> {
        let trimmed = pattern.into().trim().to_string();
        if trimmed.is_empty()
            || self
                .patterns
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&trimmed))
        {
            return Err(IoError::InvalidFilterPattern { pattern: trimmed });
        }
        self.patterns.push(trimmed);
        Ok(())
    }

    /// Remove a filter pattern by value. Returns `true` if removed.
    pub fn remove_pattern(&mut self, pattern: &str) -> bool {
        if let Some(pos) = self
            .patterns
            .iter()
            .position(|existing| existing.eq_ignore_ascii_case(pattern))
        {
            self.patterns.remove(pos);
            if self.active_index >= self.patterns.len() && !self.patterns.is_empty() {
                self.active_index = self.patterns.len() - 1;
            }
            true
        } else {
            false
        }
    }
}

/// Progress update emitted during multi-file or large-file loads (FO-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadProgress {
    /// Number of files completed so far.
    pub files_done: usize,
    /// Total number of files to load.
    pub total_files: usize,
    /// Cumulative bytes read across completed files.
    pub bytes_read: u64,
}

/// Output of loading one or more files in memory (FO-3, FO-14, D-2).
#[derive(Debug)]
pub struct MultiLoadOutput {
    /// All records concatenated in lexicographic file order.
    pub records: Vec<Record>,
    /// Sorted list of source file paths actually loaded.
    pub loaded_paths: Vec<PathBuf>,
    /// `true` if any file contained non-HL7 data.
    pub saw_non_hl7: bool,
    /// `true` if loading aborted early on non-HL7 data in `Strict` mode (FO-11).
    pub aborted_on_non_hl7: bool,
    /// `true` if loading was canceled via the cancel flag (FO-13).
    pub canceled: bool,
    /// Total uncompressed/stream bytes read.
    pub total_bytes_read: u64,
}

/// Options controlling record serialization on save (FS-4, FS-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveOptions {
    /// Segment terminator policy (`Preserve`, `Cr`, `CrLf`, `Lf`) (FS-5).
    pub term_mode: SaveTerminator,
    /// Optional MLLP disk envelope wrapping (`SB + record + ED + EB`) (FS-4).
    pub disk_envelope: Option<MllpEnvelope>,
}

/// List non-directory entry names inside a `.zip` archive (FO-4, D-5).
pub fn list_zip_entries(path: &Path) -> Result<Vec<String>, IoError> {
    let file = File::open(path).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|e| IoError::Zip {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| IoError::Zip {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if !entry.is_dir() {
            entries.push(entry.name().to_string());
        }
    }
    Ok(entries)
}

/// Load specific entries (or the first non-directory entry if `entry_names` is `None`)
/// from a `.zip` archive by streaming decompression (FO-4, D-5).
pub fn load_zip_entries(
    path: &Path,
    entry_names: Option<&[String]>,
    config: ScannerConfig,
    cancel: &AtomicBool,
) -> Result<LoadOutput, IoError> {
    load_zip_entries_with_limit(path, entry_names, config, cancel, None)
}

/// Bounded variant of [`load_zip_entries`] that stops after parsing `max_messages` total records.
pub fn load_zip_entries_with_limit(
    path: &Path,
    entry_names: Option<&[String]>,
    config: ScannerConfig,
    cancel: &AtomicBool,
    max_messages: Option<usize>,
) -> Result<LoadOutput, IoError> {
    let file = File::open(path).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|e| IoError::Zip {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let targets: Vec<String> = match entry_names {
        Some(names) => names.to_vec(),
        None => {
            let mut first = None;
            for i in 0..archive.len() {
                let entry = archive.by_index(i).map_err(|e| IoError::Zip {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
                if !entry.is_dir() {
                    first = Some(entry.name().to_string());
                    break;
                }
            }
            first.into_iter().collect()
        }
    };

    let mut all_records = Vec::new();
    let mut combined_summary = crate::scanner::ScannerSummary::default();
    let mut aborted_on_non_hl7 = false;

    for name in targets {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let remaining = match max_messages {
            Some(limit) => {
                let rem = limit.saturating_sub(all_records.len());
                if rem == 0 {
                    break;
                }
                Some(rem)
            }
            None => None,
        };
        let entry = archive.by_name(&name).map_err(|e| IoError::Zip {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let out =
            load_records_from_reader_with_limit(BufReader::new(entry), config, cancel, remaining)?;
        combined_summary.bytes_read += out.summary.bytes_read;
        combined_summary.saw_non_hl7 |= out.summary.saw_non_hl7;
        combined_summary.saw_mllp_envelope |= out.summary.saw_mllp_envelope;
        if combined_summary.detected_terminator == SourceTerminator::Unknown {
            combined_summary.detected_terminator = out.summary.detected_terminator;
        }
        all_records.extend(out.records);
        if out.aborted_on_non_hl7 {
            aborted_on_non_hl7 = true;
            break;
        }
    }

    Ok(LoadOutput {
        records: all_records,
        summary: combined_summary,
        aborted_on_non_hl7,
    })
}

/// Load a single file from disk with transparent `.gz` and `.zip` decompression and `.Z` rejection
/// (FO-4, FO-5, FO-6..FO-11, FO-14, D-5, D-6).
pub fn load_file(
    path: &Path,
    config: ScannerConfig,
    cancel: &AtomicBool,
) -> Result<LoadOutput, IoError> {
    load_file_with_limit(path, config, cancel, None)
}

/// Bounded variant of [`load_file`] that stops scanning once `max_messages` records are parsed.
pub fn load_file_with_limit(
    path: &Path,
    config: ScannerConfig,
    cancel: &AtomicBool,
    max_messages: Option<usize>,
) -> Result<LoadOutput, IoError> {
    let magic = peek_magic_bytes(path)?;
    let ext = path.extension().and_then(|e| e.to_str());

    // FO-5, D-6: Reject .Z (LZW compress) with a clear message.
    if ext == Some("Z") || magic.starts_with(&[0x1F, 0x9D]) {
        return Err(IoError::UnsupportedCompressFormat {
            path: path.to_path_buf(),
        });
    }

    // FO-4: Transparent .gz streaming decompression.
    if ext.is_some_and(|e| e.eq_ignore_ascii_case("gz")) || magic.starts_with(&[0x1F, 0x8B]) {
        let file = File::open(path).map_err(|source| IoError::FileIo {
            path: path.to_path_buf(),
            source,
        })?;
        let decoder = GzDecoder::new(file);
        return Ok(load_records_from_reader_with_limit(
            BufReader::new(decoder),
            config,
            cancel,
            max_messages,
        )?);
    }

    // FO-4: Transparent .zip streaming decompression (first entry by default).
    if ext.is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        || magic.starts_with(&[0x50, 0x4B, 0x03, 0x04])
    {
        return load_zip_entries_with_limit(path, None, config, cancel, max_messages);
    }

    let file = File::open(path).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(load_records_from_reader_with_limit(
        BufReader::new(file),
        config,
        cancel,
        max_messages,
    )?)
}

/// Parse an in-memory HL7 byte batch with an optional upper bound on the number of parsed messages.
pub fn parse_batch_with_limit(
    bytes: &[u8],
    config: ScannerConfig,
    max_messages: Option<usize>,
) -> Result<LoadOutput, IoError> {
    let cancel = AtomicBool::new(false);
    Ok(load_records_from_reader_with_limit(
        BufReader::new(std::io::Cursor::new(bytes)),
        config,
        &cancel,
        max_messages,
    )?)
}

/// Multi-select open: sort paths lexicographically by name, concatenate all records in memory,
/// report progress, and respect cancellation (FO-3, FO-13, FO-14, G-4, D-2).
pub fn load_files<F>(
    paths: &[PathBuf],
    config: ScannerConfig,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<MultiLoadOutput, IoError>
where
    F: FnMut(LoadProgress),
{
    let mut sorted = paths.to_vec();
    sorted.sort_by(|a, b| {
        let name_a = a.file_name().unwrap_or_else(|| a.as_os_str());
        let name_b = b.file_name().unwrap_or_else(|| b.as_os_str());
        name_a.cmp(name_b).then_with(|| a.cmp(b))
    });

    let total_files = sorted.len();
    let mut records = Vec::new();
    let mut loaded_paths = Vec::with_capacity(total_files);
    let mut saw_non_hl7 = false;
    let mut aborted_on_non_hl7 = false;
    let mut total_bytes_read = 0u64;

    for (idx, path) in sorted.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(MultiLoadOutput {
                records,
                loaded_paths,
                saw_non_hl7,
                aborted_on_non_hl7,
                canceled: true,
                total_bytes_read,
            });
        }

        let out = load_file(&path, config, cancel)?;
        total_bytes_read += out.summary.bytes_read;
        saw_non_hl7 |= out.summary.saw_non_hl7;
        records.extend(out.records);
        loaded_paths.push(path);

        on_progress(LoadProgress {
            files_done: idx + 1,
            total_files,
            bytes_read: total_bytes_read,
        });

        if out.aborted_on_non_hl7 {
            aborted_on_non_hl7 = true;
            break;
        }
    }

    Ok(MultiLoadOutput {
        records,
        loaded_paths,
        saw_non_hl7,
        aborted_on_non_hl7,
        canceled: cancel.load(Ordering::Relaxed),
        total_bytes_read,
    })
}

/// Load records from raw in-memory bytes (e.g. clipboard paste, FO-15).
///
/// Marks records with `SourceTerminator::Unknown` so `SAVE-TERM = Preserve` emits CRLF (FS-5).
pub fn load_from_bytes(bytes: &[u8], config: ScannerConfig) -> Result<LoadOutput, IoError> {
    let cancel = AtomicBool::new(false);
    let mut out = load_records_from_reader(BufReader::new(bytes), config, &cancel)?;
    for rec in &mut out.records {
        rec.set_source_terminator(SourceTerminator::Unknown);
    }
    Ok(out)
}

/// Atomically save all `records` to `path` via a temp file in the destination directory + rename
/// (FS-1, FS-4, FS-5, FS-6, FS-7).
pub fn save_records_atomic(
    path: &Path,
    records: &[Record],
    opts: SaveOptions,
) -> Result<(), IoError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let temp = NamedTempFile::new_in(parent).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;

    {
        let mut writer = BufWriter::new(temp.as_file());
        for rec in records {
            rec.write_to(&mut writer, opts.term_mode, opts.disk_envelope)
                .map_err(|source| IoError::FileIo {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        writer.flush().map_err(|source| IoError::FileIo {
            path: path.to_path_buf(),
            source,
        })?;
    }

    temp.persist(path).map_err(|err| IoError::AtomicPersist {
        path: path.to_path_buf(),
        source: err.error,
    })?;

    Ok(())
}

/// Atomically save only the records at `indices` (preserving selection order, skipping out-of-bounds)
/// (FS-1, FS-7).
pub fn save_selected_records_atomic(
    path: &Path,
    records: &[Record],
    indices: &[usize],
    opts: SaveOptions,
) -> Result<(), IoError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let temp = NamedTempFile::new_in(parent).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;

    {
        let mut writer = BufWriter::new(temp.as_file());
        for &idx in indices {
            if let Some(rec) = records.get(idx) {
                rec.write_to(&mut writer, opts.term_mode, opts.disk_envelope)
                    .map_err(|source| IoError::FileIo {
                        path: path.to_path_buf(),
                        source,
                    })?;
            }
        }
        writer.flush().map_err(|source| IoError::FileIo {
            path: path.to_path_buf(),
            source,
        })?;
    }

    temp.persist(path).map_err(|err| IoError::AtomicPersist {
        path: path.to_path_buf(),
        source: err.error,
    })?;

    Ok(())
}

fn peek_magic_bytes(path: &Path) -> Result<Vec<u8>, IoError> {
    let mut file = File::open(path).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    let mut buf = [0u8; 4];
    let n = file.read(&mut buf).map_err(|source| IoError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(buf[..n].to_vec())
}
