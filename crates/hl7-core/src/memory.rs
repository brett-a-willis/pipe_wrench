//! Dynamic system-aware memory protection and safe load budgeting (`Phase 1`).
//!
//! Queries live available physical RAM and current process RSS via `sysinfo` without
//! hardcoded file-size or message-count caps, and stays 100% headless with zero UI dependencies
//! so `pipe_wrench_cli` remains unaffected.

use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::scanner::{RecordBoundaryScanner, ScanEvent, ScannerConfig};

/// Default fraction of currently available physical RAM allocated to a safe in-memory load (`30%`).
pub const DEFAULT_RAM_BUDGET_FRACTION: f64 = 0.30;

/// Minimum safe load budget floor (`256 MiB`) so low-RAM systems can still load normal files.
pub const MIN_SAFE_LOAD_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Conservative in-memory HL7 AST expansion multiplier (`6.0x` raw input byte size,
/// accounting for UTF-8 strings, segment/field vectors, and UI summaries).
pub const AST_EXPANSION_FACTOR: f64 = 6.0;

/// Process RSS warning threshold as a fraction of [`MemoryBudget::safe_load_budget_bytes`] (`80%`).
pub const RSS_WARNING_FRACTION: f64 = 0.80;

/// Maximum total file size (8 MiB) for performing an exact preflight streaming record count;
/// larger files sample the leading chunk to estimate message count instantaneously.
const EXACT_COUNT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Leading byte sample window (512 KiB) used to estimate average message size on huge files.
const HUGE_FILE_SAMPLE_BYTES: usize = 512 * 1024;

/// Fallback average HL7 message byte length when sampling yields zero complete boundaries
/// (e.g., compressed `.gz` / `.zip` archives).
const FALLBACK_AVG_MESSAGE_BYTES: f64 = 1024.0;

/// Dynamic system memory snapshot and safe load budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryBudget {
    /// Live available physical system RAM in bytes.
    pub available_ram_bytes: u64,
    /// Safe in-memory load budget (`max(30% of available RAM, 256 MiB)`).
    pub safe_load_budget_bytes: u64,
    /// Current process resident set size (RSS) in bytes.
    pub process_rss_bytes: u64,
    /// Estimated heap expansion factor relative to raw HL7 file bytes (default `6.0`).
    pub ast_expansion_factor: f64,
}

impl MemoryBudget {
    /// Query live available physical RAM and current process RSS from the OS.
    pub fn from_system() -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let available = match sys.available_memory() {
            0 => sys.free_memory(),
            n => n,
        };
        let rss = current_process_rss_bytes(&mut sys);
        let mut budget = Self::from_available_bytes(available);
        budget.process_rss_bytes = rss;
        budget
    }

    /// Construct a deterministic [`MemoryBudget`] from a simulated available RAM byte count.
    ///
    /// Sets `safe_load_budget_bytes` to `30%` of `available_bytes`, clamped to a minimum floor
    /// of [`MIN_SAFE_LOAD_BUDGET_BYTES`] (`256 MiB`).
    pub fn from_available_bytes(available_bytes: u64) -> Self {
        let proportional = ((available_bytes as f64) * DEFAULT_RAM_BUDGET_FRACTION).round() as u64;
        let safe_load_budget_bytes = proportional.max(MIN_SAFE_LOAD_BUDGET_BYTES);
        Self {
            available_ram_bytes: available_bytes,
            safe_load_budget_bytes,
            process_rss_bytes: 0,
            ast_expansion_factor: AST_EXPANSION_FACTOR,
        }
    }

    /// Return a copy of this budget with a specific `process_rss_bytes` value (useful for tests).
    pub fn with_process_rss(mut self, rss_bytes: u64) -> Self {
        self.process_rss_bytes = rss_bytes;
        self
    }

    /// Estimate the in-memory heap footprint (in bytes) required to load `raw_bytes` of HL7 data.
    pub fn estimated_heap_bytes(&self, raw_bytes: u64) -> u64 {
        let est = (raw_bytes as f64) * self.ast_expansion_factor;
        if est >= u64::MAX as f64 {
            u64::MAX
        } else {
            est.round() as u64
        }
    }

    /// Return `true` when loading `raw_bytes` is projected to exceed [`Self::safe_load_budget_bytes`].
    pub fn exceeds_budget(&self, raw_bytes: u64) -> bool {
        self.estimated_heap_bytes(raw_bytes) > self.safe_load_budget_bytes
    }

    /// Return `true` when current process RSS exceeds `80%` of [`Self::safe_load_budget_bytes`].
    pub fn is_rss_high(&self) -> bool {
        self.safe_load_budget_bytes > 0
            && (self.process_rss_bytes as f64)
                >= (self.safe_load_budget_bytes as f64) * RSS_WARNING_FRACTION
    }

    /// Calculate how many messages fit safely within [`Self::safe_load_budget_bytes`].
    ///
    /// Returns `total_messages` unchanged when the projected heap footprint is within budget;
    /// otherwise scales proportionally to `safe_load_budget_bytes` (returning at least `1` when
    /// `total_messages > 0`).
    pub fn estimate_safe_message_limit(&self, total_bytes: u64, total_messages: usize) -> usize {
        if total_messages == 0 {
            return 0;
        }
        if !self.exceeds_budget(total_bytes) {
            return total_messages;
        }
        let avg_raw_bytes = (total_bytes as f64 / total_messages as f64).max(1.0);
        let max_raw_bytes = (self.safe_load_budget_bytes as f64) / self.ast_expansion_factor;
        let safe = (max_raw_bytes / avg_raw_bytes).floor() as usize;
        safe.clamp(1, total_messages)
    }
}

/// Format a byte count into a compact human-readable string (e.g. `"64 MB"`, `"4.8 GB"`).
pub fn format_memory_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GB", b / GIB)
    } else if b >= MIB {
        let mb = b / MIB;
        if mb >= 100.0 {
            format!("{mb:.0} MB")
        } else {
            format!("{mb:.1} MB")
        }
    } else if b >= KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Inspect one or more file paths on disk *before* loading them into heap ASTs, returning
/// `(total_disk_bytes, estimated_or_exact_message_count, is_exact)`.
///
/// - For small/medium inputs (`<= 8 MiB` total), performs a fast zero-AST streaming count.
/// - For huge files (`> 8 MiB`), samples up to `512 KiB` of the first file to extrapolate message count
///   in under a millisecond without stalling the UI thread.
pub fn estimate_file_set_metrics(paths: &[PathBuf], config: ScannerConfig) -> (u64, usize, bool) {
    let mut total_bytes = 0u64;
    for path in paths {
        if let Ok(meta) = std::fs::metadata(path) {
            total_bytes = total_bytes.saturating_add(meta.len());
        }
    }
    if total_bytes == 0 || paths.is_empty() {
        return (total_bytes, 0, true);
    }

    let any_compressed = paths.iter().any(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("gz") || e.eq_ignore_ascii_case("zip"))
    });

    if !any_compressed && total_bytes <= EXACT_COUNT_MAX_BYTES {
        let cancel = AtomicBool::new(false);
        let mut exact_count = 0usize;
        let mut all_ok = true;
        for path in paths {
            match count_plain_file_records(path, config, &cancel) {
                Ok(n) => exact_count = exact_count.saturating_add(n),
                Err(_) => {
                    all_ok = false;
                    break;
                }
            }
        }
        if all_ok {
            return (total_bytes, exact_count, true);
        }
    }

    // Fast sample-based extrapolation for huge or compressed files
    let avg_bytes = if !any_compressed {
        sample_avg_message_bytes(&paths[0], config).unwrap_or(FALLBACK_AVG_MESSAGE_BYTES)
    } else {
        FALLBACK_AVG_MESSAGE_BYTES
    };
    let est = ((total_bytes as f64) / avg_bytes).round().max(1.0) as usize;
    (total_bytes, est, false)
}

fn count_plain_file_records(
    path: &Path,
    config: ScannerConfig,
    cancel: &AtomicBool,
) -> std::io::Result<usize> {
    let file = File::open(path)?;
    let mut scanner = RecordBoundaryScanner::new(BufReader::new(file), config);
    let mut count = 0usize;
    let _ = scanner.scan_all(cancel, |ev| {
        if matches!(ev, ScanEvent::RecordStart { .. }) {
            count += 1;
        }
        ControlFlow::Continue(())
    });
    Ok(count)
}

fn sample_avg_message_bytes(path: &Path, config: ScannerConfig) -> Option<f64> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; HUGE_FILE_SAMPLE_BYTES];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    buf.truncate(n);
    let cancel = AtomicBool::new(false);
    let mut scanner = RecordBoundaryScanner::new(std::io::Cursor::new(&buf), config);
    let mut count = 0usize;
    let _ = scanner.scan_all(&cancel, |ev| {
        if matches!(ev, ScanEvent::RecordStart { .. }) {
            count += 1;
        }
        ControlFlow::Continue(())
    });
    if count == 0 {
        None
    } else {
        Some((n as f64 / count as f64).max(64.0))
    }
}

fn current_process_rss_bytes(sys: &mut System) -> u64 {
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map_or(0, |p| p.memory())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn memory_budget_scales_dynamically_across_2gb_16gb_and_64gb_systems() {
        // 2 GB available -> 30% = ~614.4 MiB (> 256 MiB floor)
        let small = MemoryBudget::from_available_bytes(2 * GIB);
        assert_eq!(small.available_ram_bytes, 2 * GIB);
        assert!(small.safe_load_budget_bytes > 600 * MIB);
        assert!(small.safe_load_budget_bytes < 630 * MIB);

        // Very low available RAM (512 MB -> 30% = 153.6 MB) clamps to 256 MiB floor
        let tiny = MemoryBudget::from_available_bytes(512 * MIB);
        assert_eq!(tiny.safe_load_budget_bytes, MIN_SAFE_LOAD_BUDGET_BYTES);

        // 16 GB available -> 30% = ~4.8 GB
        let medium = MemoryBudget::from_available_bytes(16 * GIB);
        assert!(medium.safe_load_budget_bytes > 4_800 * MIB);
        assert!(medium.safe_load_budget_bytes < 5_000 * MIB);

        // 64 GB available -> 30% = ~19.2 GB (scales smoothly, zero hardcoded cap)
        let large = MemoryBudget::from_available_bytes(64 * GIB);
        assert!(large.safe_load_budget_bytes > 19 * GIB);
        assert!(large.safe_load_budget_bytes < 20 * GIB);
        assert!(large.safe_load_budget_bytes > medium.safe_load_budget_bytes * 3);

        // RSS high warning triggers at >= 80% of safe budget
        let ok_rss = medium.with_process_rss(medium.safe_load_budget_bytes / 2);
        assert!(!ok_rss.is_rss_high());
        let high_rss = medium.with_process_rss((medium.safe_load_budget_bytes * 85) / 100);
        assert!(high_rss.is_rss_high());
    }

    #[test]
    fn estimate_safe_message_limit_bounds_oversized_files_and_allows_in_budget_files() {
        // 2 GB available -> safe budget ~614.4 MiB -> raw max at 6.0x expansion is ~102.4 MiB
        let budget = MemoryBudget::from_available_bytes(2 * GIB);

        // In-budget: 50 MiB raw -> 300 MiB heap <= 614.4 MiB -> returns all 50,000 messages
        assert!(!budget.exceeds_budget(50 * MIB));
        assert_eq!(budget.estimate_safe_message_limit(50 * MIB, 50_000), 50_000);

        // Over-budget: 400 MiB raw -> 2.4 GiB heap > 614.4 MiB -> ~25.6% of 400,000 messages (~102,400)
        assert!(budget.exceeds_budget(400 * MIB));
        let subset = budget.estimate_safe_message_limit(400 * MIB, 400_000);
        assert!(
            subset > 95_000 && subset < 110_000,
            "expected ~102,400 safe messages, got {subset}"
        );

        // Edge case: 0 messages -> 0; huge single message -> clamped to 1
        assert_eq!(budget.estimate_safe_message_limit(400 * MIB, 0), 0);
        assert_eq!(budget.estimate_safe_message_limit(10 * GIB, 1), 1);
    }

    #[test]
    fn format_memory_bytes_produces_clean_labels() {
        assert_eq!(format_memory_bytes(512), "512 B");
        assert_eq!(format_memory_bytes(64 * MIB), "64.0 MB");
        assert_eq!(format_memory_bytes(256 * MIB), "256 MB");
        assert_eq!(format_memory_bytes(5 * GIB), "5.0 GB");
    }
}
