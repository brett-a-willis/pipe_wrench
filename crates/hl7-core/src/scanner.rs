//! Shared streaming record-boundary scanner used by both the loader and directory scan
//! (SCAN-3, FO-6..FO-11, FO-14, D-1..D-4).

use std::io::BufRead;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::delimiters::Delimiters;
use crate::error::ScanError;
use crate::record::{Record, RecordFlags, RecordKind, SourceTerminator};

/// MLLP envelope framing bytes (NET-1, FO-7, FS-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MllpEnvelope {
    /// Start Block byte (default `0x0B`).
    pub start_block: u8,
    /// End Data byte (default `0x1C`).
    pub end_data: u8,
    /// End Block byte (default `0x0D`).
    pub end_block: u8,
}

impl Default for MllpEnvelope {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl MllpEnvelope {
    /// Standard MLLP framing bytes (`0x0B`, `0x1C`, `0x0D`).
    pub const DEFAULT: Self = Self {
        start_block: 0x0B,
        end_data: 0x1C,
        end_block: 0x0D,
    };

    /// Clean and normalize a transient outbound HL7 v2 payload for MLLP wire transmission:
    /// - Strips stray envelope framing bytes (`start_block`, `end_data`, and `end_block` when `!= 0x0D`).
    /// - Normalizes any `\r\n` (`0x0D 0x0A`) or standalone `\n` (`0x0A`) segment line endings to strict
    ///   HL7 v2 `\r` (`0x0D`).
    /// - Strips duplicate trailing `\r` bytes so the caller can append a single `<CR><ED><EB>` trailer.
    pub fn normalize_outbound_payload(self, raw: &[u8]) -> Vec<u8> {
        let sb = self.start_block;
        let ed = self.end_data;
        let eb = self.end_block;
        let is_env_byte = |b: u8| b == sb || b == ed || (eb != b'\r' && b == eb);

        let mut cleaned = Vec::with_capacity(raw.len());
        let mut i = 0usize;
        while i < raw.len() {
            let b = raw[i];
            if is_env_byte(b) {
                i += 1;
                continue;
            }
            if b == b'\r' {
                cleaned.push(b'\r');
                // Collapse `\r\n` into a single `\r`
                if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
            } else if b == b'\n' {
                cleaned.push(b'\r');
                i += 1;
            } else {
                cleaned.push(b);
                i += 1;
            }
        }

        // Strip any trailing CRs so the wire frame emits exactly one `<CR>` before `<ED><EB>`.
        while cleaned.last() == Some(&b'\r') {
            cleaned.pop();
        }
        cleaned
    }

    /// Normalize segment terminators to strict `\r` (`0x0D`) and wrap an outbound HL7 payload
    /// in MLLP framing bytes (`<SB><payload><CR><ED><EB>`, e.g. `0x0B ... 0x0D 0x1C 0x0D`).
    pub fn frame_outbound(self, raw: &[u8]) -> Vec<u8> {
        let cleaned = self.normalize_outbound_payload(raw);
        let mut framed = Vec::with_capacity(1 + cleaned.len() + 3);
        framed.push(self.start_block);
        framed.extend_from_slice(&cleaned);
        framed.push(b'\r');
        framed.push(self.end_data);
        framed.push(self.end_block);
        framed
    }
}

/// How the scanner reacts to bytes outside HL7 records (FO-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryMode {
    /// Emit `NonHl7Data` without scanning inside non-HL7 lines (allows caller to prompt FO-11).
    Strict,
    /// Skip non-HL7 bytes and scan forward (including mid-line) for the next valid record start.
    RecoverNonHl7,
}

/// Configuration for [`RecordBoundaryScanner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerConfig {
    /// Configured MLLP envelope bytes (FO-7).
    pub envelope: MllpEnvelope,
    /// Strict or non-HL7 recovery mode (FO-11).
    pub recovery_mode: RecoveryMode,
    /// Maximum buffered bytes for a single segment/line before flushing (guards SCAN-2).
    pub max_line_bytes: usize,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            envelope: MllpEnvelope::DEFAULT,
            recovery_mode: RecoveryMode::Strict,
            max_line_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Streaming events yielded by [`RecordBoundaryScanner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEvent<'a> {
    /// Start of a new top-level record (`MSH`, `FHS`, `BHS`, `BTS`, or `FTS`) (FO-8).
    RecordStart {
        /// Top-level record kind.
        kind: RecordKind,
        /// Active delimiters for this record (FO-9, FO-10).
        delimiters: Delimiters,
        /// Normalized header segment bytes (no trailing CR, no DEL, no envelope bytes).
        header_line: &'a [u8],
        /// Approximate byte offset in the input stream.
        byte_offset: u64,
        /// `true` if the record was preceded by an MLLP Start Block.
        had_mllp_envelope: bool,
    },
    /// Subsequent segment belonging to the current record.
    Segment {
        /// Normalized segment bytes (no trailing CR, no DEL).
        line: &'a [u8],
        /// Approximate byte offset in the input stream.
        byte_offset: u64,
    },
    /// End of the current record (next record start, MLLP trailer, or EOF).
    RecordEnd {
        /// Stream byte offset where the record ended.
        byte_offset: u64,
    },
    /// Bytes found outside any HL7 record (FO-11, D-4).
    NonHl7Data {
        /// Stream byte offset where the non-HL7 span began.
        byte_offset: u64,
        /// Length in bytes of the non-HL7 span.
        len: usize,
    },
}

/// Summary statistics collected during a scan pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerSummary {
    /// First detected segment terminator style (FO-14).
    pub detected_terminator: SourceTerminator,
    /// `true` if any non-HL7 data was encountered outside records (FO-11, SCAN-4).
    pub saw_non_hl7: bool,
    /// `true` if any MLLP envelope byte was encountered (FO-7).
    pub saw_mllp_envelope: bool,
    /// Total bytes read from the underlying stream.
    pub bytes_read: u64,
}

impl Default for ScannerSummary {
    fn default() -> Self {
        Self {
            detected_terminator: SourceTerminator::Unknown,
            saw_non_hl7: false,
            saw_mllp_envelope: false,
            bytes_read: 0,
        }
    }
}

/// Result of loading records via [`load_records_from_reader`].
#[derive(Debug)]
pub struct LoadOutput {
    /// Parsed records in stream order.
    pub records: Vec<Record>,
    /// Summary from the underlying [`RecordBoundaryScanner`].
    pub summary: ScannerSummary,
    /// `true` when stopped early in `RecoveryMode::Strict` due to non-HL7 data (FO-11).
    pub aborted_on_non_hl7: bool,
}

/// Streaming, bounded-memory record-boundary scanner shared by the loader and directory scanner (SCAN-3).
pub struct RecordBoundaryScanner<R: BufRead> {
    reader: R,
    config: ScannerConfig,
    line_buf: Vec<u8>,
    line_start_offset: u64,
    active_delimiters: Delimiters,
    in_record: bool,
    in_mllp_frame: bool,
    pending_cr: bool,
    pending_lf: bool,
    pending_ed: bool,
    summary: ScannerSummary,
}

impl<R: BufRead> RecordBoundaryScanner<R> {
    /// Create a new scanner over `reader` with `config`.
    pub fn new(reader: R, config: ScannerConfig) -> Self {
        Self {
            reader,
            config,
            line_buf: Vec::with_capacity(4096),
            line_start_offset: 0,
            active_delimiters: Delimiters::STANDARD,
            in_record: false,
            in_mllp_frame: false,
            pending_cr: false,
            pending_lf: false,
            pending_ed: false,
            summary: ScannerSummary::default(),
        }
    }

    /// Return the current scan summary.
    pub fn summary(&self) -> ScannerSummary {
        self.summary
    }

    /// Drive the scanner until EOF, cancellation, or `ControlFlow::Break(())`.
    pub fn scan_all<F>(
        &mut self,
        cancel: &AtomicBool,
        mut on_event: F,
    ) -> Result<ScannerSummary, ScanError>
    where
        F: FnMut(ScanEvent<'_>) -> ControlFlow<()>,
    {
        let ed = self.config.envelope.end_data;
        let eb = self.config.envelope.end_block;

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(self.summary);
            }
            let buf = self.reader.fill_buf().map_err(|source| ScanError::Io {
                offset: self.summary.bytes_read,
                source,
            })?;
            if buf.is_empty() {
                break;
            }
            let chunk_len = buf.len();
            // Copy chunk reference by processing bytes while keeping borrow local.
            let chunk: Vec<u8> = buf.to_vec();
            self.reader.consume(chunk_len);

            let mut idx = 0;
            while idx < chunk.len() {
                let b = chunk[idx];
                let cur_offset = self.summary.bytes_read;

                // Resolve pending lookahead states across chunk boundaries.
                if self.pending_ed {
                    self.pending_ed = false;
                    self.in_mllp_frame = false;
                    if self.in_record {
                        self.in_record = false;
                        if on_event(ScanEvent::RecordEnd {
                            byte_offset: cur_offset,
                        })
                        .is_break()
                        {
                            return Ok(self.summary);
                        }
                    }
                    if b == eb {
                        self.summary.bytes_read += 1;
                        idx += 1;
                        continue;
                    }
                }

                if self.pending_cr {
                    self.pending_cr = false;
                    if b == b'\n' {
                        self.record_terminator(SourceTerminator::CrLf);
                        self.summary.bytes_read += 1;
                        idx += 1;
                        continue;
                    } else if b == 0x00 {
                        // FO-6: CR NUL is a single CR segment boundary.
                        self.record_terminator(SourceTerminator::Cr);
                        self.summary.bytes_read += 1;
                        idx += 1;
                        continue;
                    } else {
                        self.record_terminator(SourceTerminator::Cr);
                    }
                }

                if self.pending_lf {
                    if b == b'\n' {
                        // FO-6: LF LF collapses empty line.
                        self.record_terminator(SourceTerminator::Lf);
                        self.summary.bytes_read += 1;
                        idx += 1;
                        continue;
                    } else {
                        self.pending_lf = false;
                        self.record_terminator(SourceTerminator::Lf);
                    }
                }

                // Fast-path scan for next boundary byte (CR, LF, ED).
                if self.line_buf.is_empty() {
                    self.line_start_offset = cur_offset;
                }

                if b == ed {
                    self.summary.saw_mllp_envelope = true;
                    if self.flush_line(&mut on_event).is_break() {
                        return Ok(self.summary);
                    }
                    self.pending_ed = true;
                    self.summary.bytes_read += 1;
                    idx += 1;
                    continue;
                }

                if b == b'\r' {
                    if self.flush_line(&mut on_event).is_break() {
                        return Ok(self.summary);
                    }
                    self.pending_cr = true;
                    self.summary.bytes_read += 1;
                    idx += 1;
                    continue;
                }

                if b == b'\n' {
                    if self.flush_line(&mut on_event).is_break() {
                        return Ok(self.summary);
                    }
                    self.pending_lf = true;
                    self.summary.bytes_read += 1;
                    idx += 1;
                    continue;
                }

                // SIMD scan forward to the next CR, LF, or ED in this chunk.
                let rest = &chunk[idx..];
                let next_special = memchr::memchr3(b'\r', b'\n', ed, rest).unwrap_or(rest.len());
                let span = &rest[..next_special];
                self.push_sanitized_bytes(span);
                self.summary.bytes_read += next_special as u64;
                idx += next_special;

                if self.line_buf.len() >= self.config.max_line_bytes
                    && self.flush_line(&mut on_event).is_break()
                {
                    return Ok(self.summary);
                }
            }
        }

        // Handle EOF lookahead and remaining buffered data.
        if self.pending_cr {
            self.pending_cr = false;
            self.record_terminator(SourceTerminator::Cr);
        }
        if self.pending_lf {
            self.pending_lf = false;
            self.record_terminator(SourceTerminator::Lf);
        }
        if self.flush_line(&mut on_event).is_break() {
            return Ok(self.summary);
        }
        if self.pending_ed {
            self.pending_ed = false;
            self.in_mllp_frame = false;
        }
        if self.in_record {
            self.in_record = false;
            let _ = on_event(ScanEvent::RecordEnd {
                byte_offset: self.summary.bytes_read,
            });
        }

        Ok(self.summary)
    }

    fn push_sanitized_bytes(&mut self, span: &[u8]) {
        // FO-6: Strip stray DEL (0x7F) bytes; fast path when none are present.
        if memchr::memchr(0x7F, span).is_none() {
            self.line_buf.extend_from_slice(span);
        } else {
            self.line_buf
                .extend(span.iter().copied().filter(|&b| b != 0x7F));
        }
    }

    fn record_terminator(&mut self, term: SourceTerminator) {
        if self.summary.detected_terminator == SourceTerminator::Unknown {
            self.summary.detected_terminator = term;
        }
    }

    fn flush_line<F>(&mut self, on_event: &mut F) -> ControlFlow<()>
    where
        F: FnMut(ScanEvent<'_>) -> ControlFlow<()>,
    {
        if self.line_buf.is_empty() {
            return ControlFlow::Continue(());
        }

        let sb = self.config.envelope.start_block;
        let line_offset = self.line_start_offset;
        let mut start = 0;
        let mut had_sb = false;

        // Strip leading MLLP Start Block byte(s) (FO-7).
        while start < self.line_buf.len() && self.line_buf[start] == sb {
            start += 1;
            had_sb = true;
            self.summary.saw_mllp_envelope = true;
            self.in_mllp_frame = true;
        }

        if start >= self.line_buf.len() {
            self.line_buf.clear();
            return ControlFlow::Continue(());
        }

        let slice = &self.line_buf[start..];

        if let Some((kind, delims)) = detect_record_start(slice, self.active_delimiters) {
            if self.in_record {
                on_event(ScanEvent::RecordEnd {
                    byte_offset: line_offset + start as u64,
                })?;
            }
            if matches!(kind, RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs) {
                self.active_delimiters = delims;
            }
            self.in_record = true;
            let res = on_event(ScanEvent::RecordStart {
                kind,
                delimiters: delims,
                header_line: slice,
                byte_offset: line_offset + start as u64,
                had_mllp_envelope: had_sb || self.in_mllp_frame,
            });
            self.line_buf.clear();
            return res;
        }

        if self.in_record {
            let res = on_event(ScanEvent::Segment {
                line: slice,
                byte_offset: line_offset + start as u64,
            });
            self.line_buf.clear();
            return res;
        }

        // Not inside a record and line does not begin with a record start.
        if self.config.recovery_mode == RecoveryMode::RecoverNonHl7 {
            if let Some((embedded_pos, kind, delims, embedded_sb)) =
                find_embedded_record_start(slice, sb, self.active_delimiters)
            {
                self.summary.saw_non_hl7 = true;
                on_event(ScanEvent::NonHl7Data {
                    byte_offset: line_offset + start as u64,
                    len: embedded_pos,
                })?;
                if embedded_sb {
                    self.summary.saw_mllp_envelope = true;
                    self.in_mllp_frame = true;
                }
                if matches!(kind, RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs) {
                    self.active_delimiters = delims;
                }
                self.in_record = true;
                let header_start = embedded_pos + usize::from(embedded_sb);
                let res = on_event(ScanEvent::RecordStart {
                    kind,
                    delimiters: delims,
                    header_line: &slice[header_start..],
                    byte_offset: line_offset + (start + header_start) as u64,
                    had_mllp_envelope: embedded_sb || self.in_mllp_frame,
                });
                self.line_buf.clear();
                return res;
            }
        }

        self.summary.saw_non_hl7 = true;
        let res = on_event(ScanEvent::NonHl7Data {
            byte_offset: line_offset + start as u64,
            len: slice.len(),
        });
        self.line_buf.clear();
        res
    }
}

/// Detect whether `line` begins a new HL7 record (FO-8, FO-9, FO-10, SCAN-3, D-3).
pub fn detect_record_start(
    line: &[u8],
    inherited_delimiters: Delimiters,
) -> Option<(RecordKind, Delimiters)> {
    if line.len() < 3 {
        return None;
    }
    match &line[0..3] {
        b"MSH" => Delimiters::from_header_bytes(line).map(|d| (RecordKind::Msh, d)),
        b"FHS" => Delimiters::from_header_bytes(line).map(|d| (RecordKind::Fhs, d)),
        b"BHS" => Delimiters::from_header_bytes(line).map(|d| (RecordKind::Bhs, d)),
        b"BTS" => {
            if line.len() == 3 || line[3] == inherited_delimiters.field {
                Some((RecordKind::Bts, inherited_delimiters))
            } else {
                None
            }
        }
        b"FTS" => {
            if line.len() == 3 || line[3] == inherited_delimiters.field {
                Some((RecordKind::Fts, inherited_delimiters))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn find_embedded_record_start(
    slice: &[u8],
    sb: u8,
    inherited: Delimiters,
) -> Option<(usize, RecordKind, Delimiters, bool)> {
    for i in 1..slice.len() {
        if slice[i] == sb && i + 1 < slice.len() {
            if let Some((kind, delims)) = detect_record_start(&slice[i + 1..], inherited) {
                return Some((i, kind, delims, true));
            }
        }
        if let Some((kind, delims)) = detect_record_start(&slice[i..], inherited) {
            return Some((i, kind, delims, false));
        }
    }
    None
}

/// Convenience loader built directly on [`RecordBoundaryScanner`] (SCAN-3, FO-6..FO-11).
pub fn load_records_from_reader<R: BufRead>(
    reader: R,
    config: ScannerConfig,
    cancel: &AtomicBool,
) -> Result<LoadOutput, ScanError> {
    load_records_from_reader_with_limit(reader, config, cancel, None)
}

/// Bounded variant of [`load_records_from_reader`] that stops scanning cleanly once `max_messages`
/// records have been parsed, preventing unbounded heap allocation on oversized files.
pub fn load_records_from_reader_with_limit<R: BufRead>(
    mut reader: R,
    config: ScannerConfig,
    cancel: &AtomicBool,
    max_messages: Option<usize>,
) -> Result<LoadOutput, ScanError> {
    if max_messages == Some(0) {
        return Ok(LoadOutput {
            records: Vec::new(),
            summary: ScannerSummary::default(),
            aborted_on_non_hl7: false,
        });
    }
    // Strip leading UTF-8 BOM (0xEF 0xBB 0xBF) if present before scanning.
    if let Ok(buf) = reader.fill_buf() {
        if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
            reader.consume(3);
        }
    }
    let mut scanner = RecordBoundaryScanner::new(reader, config);
    let mut records = Vec::new();
    let mut current: Option<(RecordKind, Delimiters, Vec<u8>)> = None;
    let mut aborted_on_non_hl7 = false;

    let summary = scanner.scan_all(cancel, |event| match event {
        ScanEvent::RecordStart {
            kind,
            delimiters,
            header_line,
            ..
        } => {
            if max_messages.is_some_and(|limit| records.len() >= limit) {
                return ControlFlow::Break(());
            }
            let mut bytes = Vec::with_capacity(header_line.len() + 64);
            bytes.extend_from_slice(header_line);
            current = Some((kind, delimiters, bytes));
            ControlFlow::Continue(())
        }
        ScanEvent::Segment { line, .. } => {
            if let Some((_, _, ref mut bytes)) = current {
                bytes.push(b'\r');
                bytes.extend_from_slice(line);
            }
            ControlFlow::Continue(())
        }
        ScanEvent::RecordEnd { .. } => {
            if let Some((kind, delimiters, bytes)) = current.take() {
                records.push(Record::from_normalized_bytes(
                    bytes,
                    kind,
                    delimiters,
                    SourceTerminator::Unknown,
                    RecordFlags::default(),
                ));
                if max_messages.is_some_and(|limit| records.len() >= limit) {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        }
        ScanEvent::NonHl7Data { .. } => {
            if config.recovery_mode == RecoveryMode::Strict {
                aborted_on_non_hl7 = true;
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    })?;

    // Stamp detected source terminator on all loaded records (FO-14, D-2).
    for record in &mut records {
        record.set_source_terminator(summary.detected_terminator);
    }

    Ok(LoadOutput {
        records,
        summary,
        aborted_on_non_hl7,
    })
}
