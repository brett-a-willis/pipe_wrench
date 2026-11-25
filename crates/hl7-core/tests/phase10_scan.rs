//! Phase 10 integration tests and 5 GB streaming throughput benchmark for `hl7-core::scan`
//! (`SCAN-1..SCAN-13`, `D-3`, `D-5`, `D-20`, `D-21`).

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use hl7_core::{
    load_file, scan_directory, scan_reader, DirectoryScanConfig, FileScanStatus, GroupByKey,
    RecoveryMode, ScannerConfig, SCAN_WORKER_MAX_LINE_BYTES,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn scan_fixtures_root() -> PathBuf {
    fixtures_root().join("scan")
}

// SCAN-3: Single source of truth assertion — scan total_records equals loaded record count
// for every single fixture in `tests/fixtures/` and `tests/fixtures/scan/`.
#[test]
fn scan3_counts_match_loaded_record_counts_for_all_fixtures() {
    let cancel = AtomicBool::new(false);
    let load_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };
    let scan_cfg = DirectoryScanConfig::default();

    let all_fixture_files = [
        "adt_a01_basic.hl7",
        "adt_a01_crlf.hl7",
        "adt_a01_lf.hl7",
        "adt_a01_lflf.hl7",
        "adt_a01_cr_nul.hl7",
        "batch_fhs_bhs.hl7",
        "mllp_framed.dat",
        "nonhl7_mixed.txt",
        "delims_alt.hl7",
        "escapes.hl7",
        "repeats_components.hl7",
        "utf8_msh18.hl7",
        "empty_fields.hl7",
        "scan/a.txt",
        "scan/b.dat",
        "scan/sub/c.hl7",
        "scan/sub/deep/d.txt",
        "scan/mismatch.txt",
        "scan/mixed.txt",
        "scan/notes.txt",
        "scan/binary.dat",
        "scan/empty.txt",
        "scan/arch.zip",
    ];

    for rel in all_fixture_files {
        let path = fixtures_root().join(rel);
        let loaded = load_file(&path, load_cfg, &cancel).unwrap();

        let scanned = if rel.ends_with(".zip") {
            let cfg = DirectoryScanConfig {
                root_dir: scan_fixtures_root(),
                extension_patterns: vec!["*.txt".to_string()],
                recursive: false,
                ..DirectoryScanConfig::default()
            };
            let rep = scan_directory(&cfg, &cancel, |_| {}).unwrap();
            rep.files
                .into_iter()
                .find(|f| f.display_path == "arch.zip!e.txt")
                .expect("arch.zip!e.txt present")
        } else {
            let f = std::fs::File::open(&path).unwrap();
            scan_reader(
                BufReader::new(f),
                rel.to_string(),
                path.clone(),
                None,
                0,
                None,
                &scan_cfg,
                &cancel,
            )
            .unwrap()
        };

        assert_eq!(
            scanned.total_records() as usize,
            loaded.records.len(),
            "SCAN-3 mismatch between scan and loader for {rel}"
        );
    }
}

// SCAN-1, SCAN-4, SCAN-5, SCAN-6, SCAN-7, SCAN-8, SCAN-9, SCAN-10, SCAN-12, SCAN-13, D-20, D-21
#[test]
fn directory_scan_matches_expected_md_totals_rollups_group_by_and_batch_mismatch() {
    let cancel = AtomicBool::new(false);
    let cfg = DirectoryScanConfig {
        root_dir: scan_fixtures_root(),
        // D-21: Mix bare extensions and globs to verify normalization
        extension_patterns: vec!["txt".to_string(), "*.dat".to_string(), ".hl7".to_string()],
        recursive: true,
        group_by: vec![
            GroupByKey::MessageType,
            GroupByKey::SendingAppFacility,
            GroupByKey::MessageDate,
            GroupByKey::CustomAddress("PID-3.1".parse().unwrap()),
        ],
        check_duplicate_control_ids: true,
        ..DirectoryScanConfig::default()
    };

    let report = scan_directory(&cfg, &cancel, |_| {}).unwrap();

    // Verify deterministic sorted order and expected file count (10 entries including arch.zip!e.txt)
    let paths: Vec<&str> = report
        .files
        .iter()
        .map(|f| f.display_path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![
            "a.txt",
            "arch.zip!e.txt",
            "b.dat",
            "binary.dat",
            "empty.txt",
            "mismatch.txt",
            "mixed.txt",
            "notes.txt",
            "sub/c.hl7",
            "sub/deep/d.txt",
        ]
    );

    // Grand totals matching tests/fixtures/scan/EXPECTED.md
    assert_eq!(report.grand_total.files, 10);
    assert_eq!(report.grand_total.msh_count, 18);
    assert_eq!(report.grand_total.fhs_count, 1);
    assert_eq!(report.grand_total.bhs_count, 2);
    assert_eq!(report.grand_total.bts_count, 2);
    assert_eq!(report.grand_total.fts_count, 1);
    assert_eq!(report.grand_total.batch_mismatch_files, 1);

    // Per-file status assertions (SCAN-4, SCAN-5)
    let by_path = |p: &str| report.files.iter().find(|f| f.display_path == p).unwrap();
    assert_eq!(by_path("a.txt").status, FileScanStatus::Ok);
    assert_eq!(by_path("arch.zip!e.txt").msh_count, 2);
    assert_eq!(by_path("b.dat").msh_count, 2);
    assert_eq!(by_path("binary.dat").status, FileScanStatus::NoHl7Found);
    assert_eq!(by_path("empty.txt").status, FileScanStatus::NoHl7Found);
    assert_eq!(by_path("notes.txt").status, FileScanStatus::NoHl7Found);
    assert_eq!(
        by_path("mixed.txt").status,
        FileScanStatus::ContainsNonHl7Data
    );
    assert_eq!(by_path("mixed.txt").msh_count, 2);

    let mm = by_path("mismatch.txt");
    assert_eq!(mm.status, FileScanStatus::BatchCountMismatch);
    assert_eq!(mm.batch_mismatches.len(), 1);
    assert_eq!(mm.batch_mismatches[0].declared_count, Some(5));
    assert_eq!(mm.batch_mismatches[0].actual_count, 3);

    // Rollups by extension and subdirectory (SCAN-6)
    assert_eq!(report.by_extension[".txt"].msh_count, 12);
    assert_eq!(report.by_extension[".hl7"].msh_count, 4);
    assert_eq!(report.by_extension[".dat"].msh_count, 2);

    assert_eq!(report.by_subdirectory["."].msh_count, 12);
    assert_eq!(report.by_subdirectory["sub"].msh_count, 4);
    assert_eq!(report.by_subdirectory["sub/deep"].msh_count, 2);

    // Group-by total messages equals grand total MSH count (18) (SCAN-7, SCAN-8)
    let group_sum: u64 = report.group_by_rows.iter().map(|r| r.message_count).sum();
    assert_eq!(group_sum, 18);

    // Exporters (table, CSV, JSON) (SCAN-13)
    let table_txt = report.format_table();
    assert!(table_txt.contains("GRAND TOTAL") && table_txt.contains("Batch count mismatch"));
    let csv_bytes = report.export_csv().unwrap();
    assert!(String::from_utf8(csv_bytes)
        .unwrap()
        .contains("arch.zip!e.txt"));
    let json_bytes = report.export_json().unwrap();
    assert!(String::from_utf8(json_bytes)
        .unwrap()
        .contains("\"msh_count\": 18"));
}

// §13 Non-functional requirement: 5 GB synthetic streaming benchmark (SCAN-2 bounded memory <= 64 MiB).
struct SyntheticHl7Stream {
    pattern: Vec<u8>,
    offset: usize,
    remaining: u64,
}

impl SyntheticHl7Stream {
    fn new(total_bytes: u64) -> Self {
        let msg = b"MSH|^~\\&|BENCH_APP|BENCH_FAC|RCV_APP|RCV_FAC|20260115120000||ADT^A01|MSG000001|P|2.5.1\rPID|1||PAT000001^^^HOSP^MR||BENCH^PATIENT||19800101|U\rPV1|1|I|WARD1^101^A\r";
        // Repeat message into a 64 KiB block so read() copies fast
        let mut pattern = Vec::with_capacity(65536 + msg.len());
        while pattern.len() < 65536 {
            pattern.extend_from_slice(msg);
        }
        Self {
            pattern,
            offset: 0,
            remaining: total_bytes,
        }
    }
}

impl Read for SyntheticHl7Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let avail_in_pat = self.pattern.len() - self.offset;
        let to_copy = buf
            .len()
            .min(avail_in_pat)
            .min(self.remaining.min(usize::MAX as u64) as usize);
        buf[..to_copy].copy_from_slice(&self.pattern[self.offset..self.offset + to_copy]);
        self.offset = (self.offset + to_copy) % self.pattern.len();
        self.remaining -= to_copy as u64;
        Ok(to_copy)
    }
}

#[test]
#[ignore]
fn bench_scan_5gb_streaming_throughput_and_bounded_memory() {
    let total_bytes: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB
    let stream = SyntheticHl7Stream::new(total_bytes);
    let reader = BufReader::with_capacity(256 * 1024, stream);
    let cancel = AtomicBool::new(false);
    let cfg = DirectoryScanConfig::default();

    const {
        assert!(
            SCAN_WORKER_MAX_LINE_BYTES <= 64 * 1024 * 1024,
            "worker line buffer must stay <= 64 MiB (SCAN-2)"
        );
    }

    let start = Instant::now();
    let res = scan_reader(
        reader,
        "synthetic_5gb.hl7".to_string(),
        PathBuf::from("synthetic_5gb.hl7"),
        None,
        total_bytes,
        None,
        &cfg,
        &cancel,
    )
    .unwrap();
    let elapsed = start.elapsed();
    let gbps = (total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)) / elapsed.as_secs_f64();

    println!(
        "5 GiB scan complete in {:.2}s ({:.2} GiB/s): {} MSH messages counted",
        elapsed.as_secs_f64(),
        gbps,
        res.msh_count
    );
    assert!(res.msh_count > 30_000_000);
}
