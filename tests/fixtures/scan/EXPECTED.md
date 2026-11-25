# Expected Directory Scan Results (`tests/fixtures/scan/`)

Generated with default settings (`extension_patterns = ["*.txt", "*.dat", "*.hl7"]`, `recursive = true`, `scan_archives = true`).

## Per-File Results (Deterministic Path Order, `SCAN-4`, `SCAN-5`, `SCAN-9`, `SCAN-10`)

| Path | MSH | FHS | BHS | BTS | FTS | Status | Notes |
|---|---:|---:|---:|---:|---:|---|---|
| `a.txt` | 3 | 0 | 0 | 0 | 0 | `OK` | 3 CR-terminated messages |
| `arch.zip!e.txt` | 2 | 0 | 0 | 0 | 0 | `OK` | Streaming zip entry (`SCAN-9`) |
| `b.dat` | 2 | 0 | 0 | 0 | 0 | `OK` | 2 MLLP-framed messages |
| `binary.dat` | 0 | 0 | 0 | 0 | 0 | `No HL7 found` | 256 non-HL7 binary bytes, no crash |
| `empty.txt` | 0 | 0 | 0 | 0 | 0 | `No HL7 found` | 0 bytes |
| `mismatch.txt` | 3 | 0 | 1 | 1 | 0 | `Batch count mismatch` | `BTS-1 = 5` vs. actual `3` (`SCAN-5`) |
| `mixed.txt` | 2 | 0 | 0 | 0 | 0 | `Contains non-HL7 data` | Leading junk + 2 valid messages |
| `notes.txt` | 0 | 0 | 0 | 0 | 0 | `No HL7 found` | Prose text only |
| `sub/c.hl7` | 4 | 1 | 1 | 1 | 1 | `OK` | Valid batch (`BTS-1 = 4`) |
| `sub/deep/d.txt` | 2 | 0 | 0 | 0 | 0 | `OK` | 2 LF-terminated messages |

## Grand Totals (`SCAN-6`)

- **Total Files/Entries:** `10`
- **Total `MSH` Messages:** `18`
- **Total `FHS` Records:** `1`
- **Total `BHS` Records:** `2`
- **Total `BTS` Records:** `2`
- **Total `FTS` Records:** `1`
- **Batch Mismatch Files:** `1` (`mismatch.txt`: declared `5`, actual `3`)

## Rollup by Extension (`SCAN-6`)

| Extension | Files | MSH | FHS | BHS | BTS | FTS |
|---|---:|---:|---:|---:|---:|---:|
| `.dat` | 2 | 2 | 0 | 0 | 0 | 0 |
| `.hl7` | 1 | 4 | 1 | 1 | 1 | 1 |
| `.txt` | 7 | 12 | 0 | 1 | 1 | 0 |

## Rollup by Subdirectory (`SCAN-6`)

| Subdirectory | Files | MSH |
|---|---:|---:|
| `.` | 8 | 12 |
| `sub` | 1 | 4 |
| `sub/deep` | 1 | 2 |
