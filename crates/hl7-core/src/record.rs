//! Core HL7 v2 `Record` representation, lazy `SegmentIndex`, field breakdown, and serialization
//! (G-1, G-2, G-3, §5.1, §5.2, FS-4..FS-6, NFR-2).

use std::io::Write;
use std::num::NonZeroU16;
use std::sync::OnceLock;

use crate::address::FieldAddress;
use crate::delimiters::Delimiters;
use crate::encoding::ResolvedCharset;
use crate::error::CoreError;
use crate::scanner::{detect_record_start, MllpEnvelope};

/// Top-level record type (FO-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordKind {
    /// Standard HL7 message starting with `MSH`.
    Msh,
    /// File Header Segment (`FHS`).
    Fhs,
    /// Batch Header Segment (`BHS`).
    Bhs,
    /// Batch Trailer Segment (`BTS`).
    Bts,
    /// File Trailer Segment (`FTS`).
    Fts,
}

impl RecordKind {
    /// Returns the 3-byte ASCII segment tag for this record kind.
    pub fn tag(self) -> [u8; 3] {
        match self {
            Self::Msh => *b"MSH",
            Self::Fhs => *b"FHS",
            Self::Bhs => *b"BHS",
            Self::Bts => *b"BTS",
            Self::Fts => *b"FTS",
        }
    }

    /// Returns `true` if this record kind is a batch/file envelope segment (`FHS`/`BHS`/`BTS`/`FTS`).
    pub fn is_envelope(self) -> bool {
        !matches!(self, Self::Msh)
    }
}

/// Detected source segment terminator style for `SAVE-TERM = Preserve` (FO-14, FS-5, D-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTerminator {
    /// Carriage return (`0x0D`).
    Cr,
    /// Carriage return + line feed (`0x0D 0x0A`).
    CrLf,
    /// Line feed (`0x0A`).
    Lf,
    /// Clipboard paste, network receive, or single-segment input with no observed terminator.
    Unknown,
}

impl SourceTerminator {
    /// Byte sequence written when serializing under `SaveTerminator::Preserve` (FS-5).
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Cr => b"\r",
            Self::CrLf | Self::Unknown => b"\r\n",
            Self::Lf => b"\n",
        }
    }
}

/// Save terminator policy (FS-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaveTerminator {
    /// Preserve detected source terminator (CRLF for unknown sources).
    #[default]
    Preserve,
    /// Always emit CR (`0x0D`).
    Cr,
    /// Always emit CRLF (`0x0D 0x0A`).
    CrLf,
    /// Always emit LF (`0x0A`).
    Lf,
}

impl SaveTerminator {
    /// Resolve the concrete terminator bytes for a given `SourceTerminator`.
    pub fn resolve(self, source: SourceTerminator) -> &'static [u8] {
        match self {
            Self::Preserve => source.as_bytes(),
            Self::Cr => b"\r",
            Self::CrLf => b"\r\n",
            Self::Lf => b"\n",
        }
    }
}

/// Non-fatal flags captured during load or network receive (NET-4, G-2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordFlags {
    /// Frame arrived without a leading MLLP Start Block byte (NET-4).
    pub missing_start_block: bool,
    /// `MSH-18` specified an unknown character set and fell back to Windows-1252 (G-2).
    pub unknown_charset: bool,
}

/// Lightweight summary displayed per row in the virtualized record list (§5.1, D-14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    /// Top-level record kind.
    pub kind: RecordKind,
    /// Message type (`MSH-9`), or envelope tag (`FHS`/`BHS`/`BTS`/`FTS`).
    pub message_type: String,
    /// Control ID (`MSH-10`) or envelope ID/count (`FHS-11`/`BHS-11`/`BTS-1`/`FTS-1`).
    pub control_id: String,
    /// Timestamp (`MSH-7` / `FHS-7` / `BHS-7`).
    pub timestamp: String,
}

/// Byte span of one segment within [`Record::bytes`] (excluding trailing CR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentSpan {
    /// 3-byte segment tag (padded with `b' '` if malformed and shorter than 3 bytes).
    pub tag: [u8; 3],
    /// Inclusive start offset in `Record::bytes`.
    pub start: usize,
    /// Exclusive end offset in `Record::bytes`.
    pub end: usize,
    /// 1-based occurrence count of this tag within the record.
    pub occurrence: usize,
}

/// Lazily constructed index of segment byte offsets within a [`Record`] (NFR-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentIndex {
    segments: Vec<SegmentSpan>,
}

impl SegmentIndex {
    /// Build a segment index by scanning CR (`0x0D`) boundaries in `record_bytes`.
    pub fn build(record_bytes: &[u8]) -> Self {
        let mut segments = Vec::new();
        let mut start = 0;

        while start <= record_bytes.len() {
            let end = match memchr::memchr(b'\r', &record_bytes[start..]) {
                Some(rel) => start + rel,
                None => record_bytes.len(),
            };
            if end > start {
                let seg = &record_bytes[start..end];
                let tag = [
                    seg.first().copied().unwrap_or(b' '),
                    seg.get(1).copied().unwrap_or(b' '),
                    seg.get(2).copied().unwrap_or(b' '),
                ];
                let occurrence = segments
                    .iter()
                    .filter(|s: &&SegmentSpan| s.tag == tag)
                    .count()
                    + 1;
                segments.push(SegmentSpan {
                    tag,
                    start,
                    end,
                    occurrence,
                });
            }
            if end == record_bytes.len() {
                break;
            }
            start = end + 1;
        }

        Self { segments }
    }

    /// Number of segments in the record.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Returns `true` if the record contains no segments.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Get segment span by 0-based index.
    pub fn get(&self, index: usize) -> Option<&SegmentSpan> {
        self.segments.get(index)
    }

    /// Iterate over all segment spans in order.
    pub fn iter(&self) -> std::slice::Iter<'_, SegmentSpan> {
        self.segments.iter()
    }

    /// Iterate over `(segment_index, &SegmentSpan)` matching `tag`.
    pub fn find_by_tag(&self, tag: &[u8; 3]) -> impl Iterator<Item = (usize, &SegmentSpan)> {
        let target = *tag;
        self.segments
            .iter()
            .enumerate()
            .filter(move |(_, span)| span.tag == target)
    }

    /// Slice the bytes of segment `index` from `record_bytes`.
    pub fn slice<'a>(&self, record_bytes: &'a [u8], index: usize) -> Option<&'a [u8]> {
        let span = self.segments.get(index)?;
        record_bytes.get(span.start..span.end)
    }
}

/// Options controlling field breakdown expansion (§5.2, §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDisplayOptions {
    /// Show empty fields/components (default `false`).
    pub display_empty_fields: bool,
    /// Show compound field header line before its component/subcomponent rows (default `true`).
    pub display_compound_fields: bool,
}

impl Default for FieldDisplayOptions {
    fn default() -> Self {
        Self {
            display_empty_fields: false,
            display_compound_fields: true,
        }
    }
}

/// One row in the field breakdown pane (§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEntry {
    /// Formatted address (e.g. `PID-5(2).1`), or `None` for non-addressable rows.
    pub address: Option<FieldAddress>,
    /// Raw field bytes.
    pub raw_value: Vec<u8>,
    /// Reserved non-editable marker flag (always `false`).
    pub is_parsed_marker: bool,
    /// `false` for `MSH-1/2`, `FHS-1/2`, and `BHS-1/2` (EDIT-3, D-9).
    pub is_editable: bool,
}

/// One top-level HL7 record stored as raw CR-separated bytes (G-1, G-3, NFR-2).
#[derive(Debug)]
pub struct Record {
    bytes: Vec<u8>,
    kind: RecordKind,
    delimiters: Delimiters,
    source_terminator: SourceTerminator,
    edited: bool,
    flags: RecordFlags,
    index: OnceLock<SegmentIndex>,
}

impl Clone for Record {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            kind: self.kind,
            delimiters: self.delimiters,
            source_terminator: self.source_terminator,
            edited: self.edited,
            flags: self.flags,
            index: OnceLock::new(),
        }
    }
}

impl Record {
    /// Create a record from CR-normalized bytes (no trailing CR on the final segment).
    pub fn from_normalized_bytes(
        bytes: Vec<u8>,
        kind: RecordKind,
        delimiters: Delimiters,
        source_terminator: SourceTerminator,
        flags: RecordFlags,
    ) -> Self {
        Self {
            bytes,
            kind,
            delimiters,
            source_terminator,
            edited: false,
            flags,
            index: OnceLock::new(),
        }
    }

    /// Top-level record kind.
    pub fn kind(&self) -> RecordKind {
        self.kind
    }

    /// Delimiters active for this record.
    pub fn delimiters(&self) -> Delimiters {
        self.delimiters
    }

    /// Raw internal CR-separated bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `true` if the record has been modified since load.
    pub fn is_edited(&self) -> bool {
        self.edited
    }

    /// Detected source terminator for this record (FO-14, D-2).
    pub fn source_terminator(&self) -> SourceTerminator {
        self.source_terminator
    }

    /// Update the source terminator (used by the loader after scanning a file).
    pub(crate) fn set_source_terminator(&mut self, term: SourceTerminator) {
        self.source_terminator = term;
    }

    /// Non-fatal record flags.
    pub fn flags(&self) -> RecordFlags {
        self.flags
    }

    /// Lazily builds and caches the [`SegmentIndex`] on first access (NFR-2).
    pub fn segments(&self) -> &SegmentIndex {
        self.index.get_or_init(|| SegmentIndex::build(&self.bytes))
    }

    /// Raw byte slice of segment `seg_index` (without trailing CR).
    pub fn segment_bytes(&self, seg_index: usize) -> Option<&[u8]> {
        self.segments().slice(&self.bytes, seg_index)
    }

    /// Resolve the text encoding declared in `MSH-18` (G-2).
    pub fn charset(&self) -> ResolvedCharset {
        if self.kind != RecordKind::Msh {
            return ResolvedCharset::default();
        }
        let msh18 = NonZeroU16::new(18).and_then(|field| {
            let addr = FieldAddress {
                segment: *b"MSH",
                field,
                repetition: None,
                component: None,
                subcomponent: None,
            };
            self.field_bytes(0, &addr)
        });
        ResolvedCharset::from_msh18(msh18, self.delimiters.repetition)
    }

    /// Build a lightweight summary for the virtualized record list (§5.1, D-14).
    pub fn summary(&self) -> RecordSummary {
        let charset = self.charset();
        let decode_opt = |opt: Option<&[u8]>| -> String {
            opt.map(|b| charset.decode_display(b).into_owned())
                .unwrap_or_default()
        };

        match self.kind {
            RecordKind::Msh => RecordSummary {
                kind: self.kind,
                message_type: decode_opt(self.simple_field(0, *b"MSH", 9)),
                control_id: decode_opt(self.simple_field(0, *b"MSH", 10)),
                timestamp: decode_opt(self.simple_field(0, *b"MSH", 7)),
            },
            RecordKind::Fhs => RecordSummary {
                kind: self.kind,
                message_type: "FHS".to_string(),
                control_id: decode_opt(self.simple_field(0, *b"FHS", 11)),
                timestamp: decode_opt(self.simple_field(0, *b"FHS", 7)),
            },
            RecordKind::Bhs => RecordSummary {
                kind: self.kind,
                message_type: "BHS".to_string(),
                control_id: decode_opt(self.simple_field(0, *b"BHS", 11)),
                timestamp: decode_opt(self.simple_field(0, *b"BHS", 7)),
            },
            RecordKind::Bts => RecordSummary {
                kind: self.kind,
                message_type: "BTS".to_string(),
                control_id: decode_opt(self.simple_field(0, *b"BTS", 1)),
                timestamp: String::new(),
            },
            RecordKind::Fts => RecordSummary {
                kind: self.kind,
                message_type: "FTS".to_string(),
                control_id: decode_opt(self.simple_field(0, *b"FTS", 1)),
                timestamp: String::new(),
            },
        }
    }

    fn simple_field(&self, seg_index: usize, tag: [u8; 3], field_num: u16) -> Option<&[u8]> {
        let field = NonZeroU16::new(field_num)?;
        let addr = FieldAddress {
            segment: tag,
            field,
            repetition: None,
            component: None,
            subcomponent: None,
        };
        self.field_bytes(seg_index, &addr)
    }

    /// Resolve a [`FieldAddress`] against segment `seg_index`, returning its raw byte slice.
    pub fn field_bytes(&self, seg_index: usize, addr: &FieldAddress) -> Option<&[u8]> {
        let seg = self.segment_bytes(seg_index)?;
        if seg.len() < 3 || seg[0..3] != addr.segment {
            return None;
        }
        let delims = self.delimiters;
        let is_header =
            addr.segment == *b"MSH" || addr.segment == *b"FHS" || addr.segment == *b"BHS";

        if is_header {
            match addr.field.get() {
                1 => return seg.get(3..4),
                2 => return seg.get(4..8),
                n => {
                    let body = seg.get(4..)?;
                    let field_slice = body.split(|&b| b == delims.field).nth(usize::from(n - 2))?;
                    return slice_subfields(field_slice, delims, addr);
                }
            }
        }

        if seg.len() < 4 || seg[3] != delims.field {
            return None;
        }
        let body = &seg[4..];
        let field_slice = body
            .split(|&b| b == delims.field)
            .nth(usize::from(addr.field.get() - 1))?;
        slice_subfields(field_slice, delims, addr)
    }

    /// Expand a segment into display rows matching §5.2 field list format.
    pub fn field_entries(&self, seg_index: usize, opts: FieldDisplayOptions) -> Vec<FieldEntry> {
        let Some(seg) = self.segment_bytes(seg_index) else {
            return Vec::new();
        };
        if seg.len() < 3 {
            return Vec::new();
        }
        let tag = [seg[0], seg[1], seg[2]];
        let delims = self.delimiters;
        let is_header = tag == *b"MSH" || tag == *b"FHS" || tag == *b"BHS";
        let mut entries = Vec::new();

        if is_header {
            // MSH-1 / FHS-1 / BHS-1 verbatim, never split (§5.2, D-9).
            if let (Some(f1), Some(sep)) = (NonZeroU16::new(1), seg.get(3..4)) {
                entries.push(FieldEntry {
                    address: Some(FieldAddress {
                        segment: tag,
                        field: f1,
                        repetition: None,
                        component: None,
                        subcomponent: None,
                    }),
                    raw_value: sep.to_vec(),
                    is_parsed_marker: false,
                    is_editable: false,
                });
            }
            // MSH-2 / FHS-2 / BHS-2 verbatim, never split (§5.2, D-9).
            if let (Some(f2), Some(enc)) = (NonZeroU16::new(2), seg.get(4..8)) {
                entries.push(FieldEntry {
                    address: Some(FieldAddress {
                        segment: tag,
                        field: f2,
                        repetition: None,
                        component: None,
                        subcomponent: None,
                    }),
                    raw_value: enc.to_vec(),
                    is_parsed_marker: false,
                    is_editable: false,
                });
            }
            if let Some(body) = seg.get(4..) {
                for (i, field_bytes) in body.split(|&b| b == delims.field).enumerate().skip(1) {
                    if let Some(field_num) = NonZeroU16::new((i + 2) as u16) {
                        push_field_entries(&mut entries, tag, field_num, field_bytes, delims, opts);
                    }
                }
            }
            return entries;
        }

        if seg.len() < 4 || seg[3] != delims.field {
            return entries;
        }
        for (i, field_bytes) in seg[4..].split(|&b| b == delims.field).enumerate() {
            if let Some(field_num) = NonZeroU16::new((i + 1) as u16) {
                push_field_entries(&mut entries, tag, field_num, field_bytes, delims, opts);
            }
        }
        entries
    }

    /// Splice replacement bytes for a single segment; invalidates the cached index and returns
    /// the prior segment bytes for undo (G-1, EDIT-3).
    pub fn splice_segment(
        &mut self,
        seg_index: usize,
        new_segment: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let span = {
            let idx = self.segments();
            let len = idx.len();
            *idx.get(seg_index).ok_or(CoreError::SegmentOutOfBounds {
                index: seg_index,
                len,
            })?
        };
        let old_segment = self.bytes[span.start..span.end].to_vec();
        self.bytes
            .splice(span.start..span.end, new_segment.iter().copied());
        self.index = OnceLock::new();
        self.edited = true;
        if seg_index == 0 {
            if let Some((kind, delims)) = detect_record_start(new_segment, self.delimiters) {
                self.kind = kind;
                self.delimiters = delims;
            }
        }
        Ok(old_segment)
    }

    /// Replace the entire raw record buffer (EDIT-2); re-detects kind and delimiters.
    pub fn replace_all_bytes(&mut self, new_bytes: Vec<u8>) -> Result<Vec<u8>, CoreError> {
        let old = std::mem::replace(&mut self.bytes, new_bytes);
        self.index = OnceLock::new();
        self.edited = true;
        if let Some(first_seg) = self.segment_bytes(0) {
            if let Some((kind, delims)) = detect_record_start(first_seg, self.delimiters) {
                self.kind = kind;
                self.delimiters = delims;
            }
        }
        Ok(old)
    }

    /// Restore exact raw bytes, delimiters, kind, and edited flag (used by undo commands and live raw-edit coalescing).
    pub fn restore_raw_state(
        &mut self,
        bytes: Vec<u8>,
        kind: RecordKind,
        delimiters: Delimiters,
        edited: bool,
    ) {
        self.bytes = bytes;
        self.kind = kind;
        self.delimiters = delimiters;
        self.edited = edited;
        self.index = OnceLock::new();
    }

    /// Update the `edited` flag (used when undoing a single-segment splice back to clean state).
    pub(crate) fn set_edited(&mut self, edited: bool) {
        self.edited = edited;
    }

    /// Update active delimiters without touching bytes (used by `ChangeDelimitersCmd` for BTS/FTS).
    pub(crate) fn set_delimiters(&mut self, delimiters: Delimiters) {
        self.delimiters = delimiters;
    }

    /// Serialize record with chosen terminator and optional MLLP disk envelope (G-1, FS-4, FS-5, FS-6).
    pub fn write_to<W: Write>(
        &self,
        writer: &mut W,
        term_mode: SaveTerminator,
        envelope: Option<MllpEnvelope>,
    ) -> std::io::Result<()> {
        let term = term_mode.resolve(self.source_terminator);
        if let Some(env) = envelope {
            writer.write_all(&[env.start_block])?;
        }

        if term == b"\r" {
            // Fast path: internal representation is already CR-separated.
            writer.write_all(&self.bytes)?;
            writer.write_all(b"\r")?;
        } else {
            let mut start = 0;
            while start <= self.bytes.len() {
                let end = match memchr::memchr(b'\r', &self.bytes[start..]) {
                    Some(rel) => start + rel,
                    None => self.bytes.len(),
                };
                writer.write_all(&self.bytes[start..end])?;
                writer.write_all(term)?;
                if end == self.bytes.len() {
                    break;
                }
                start = end + 1;
            }
        }

        if let Some(env) = envelope {
            writer.write_all(&[env.end_data, env.end_block])?;
        }
        Ok(())
    }
}

fn slice_subfields<'a>(
    field_slice: &'a [u8],
    delims: Delimiters,
    addr: &FieldAddress,
) -> Option<&'a [u8]> {
    let rep_slice = match addr.repetition {
        Some(r) => field_slice
            .split(|&b| b == delims.repetition)
            .nth(usize::from(r.get() - 1))?,
        None if addr.component.is_some() => {
            field_slice.split(|&b| b == delims.repetition).next()?
        }
        None => return Some(field_slice),
    };

    let comp_slice = match addr.component {
        Some(c) => rep_slice
            .split(|&b| b == delims.component)
            .nth(usize::from(c.get() - 1))?,
        None => return Some(rep_slice),
    };

    match addr.subcomponent {
        Some(s) => comp_slice
            .split(|&b| b == delims.subcomponent)
            .nth(usize::from(s.get() - 1)),
        None => Some(comp_slice),
    }
}

fn push_field_entries(
    entries: &mut Vec<FieldEntry>,
    tag: [u8; 3],
    field: NonZeroU16,
    field_bytes: &[u8],
    delims: Delimiters,
    opts: FieldDisplayOptions,
) {
    if field_bytes.is_empty() && !opts.display_empty_fields {
        return;
    }

    let has_rep = memchr::memchr(delims.repetition, field_bytes).is_some();
    let has_comp = memchr::memchr(delims.component, field_bytes).is_some();
    let has_sub = memchr::memchr(delims.subcomponent, field_bytes).is_some();
    let is_compound = has_rep || has_comp || has_sub;

    if !is_compound {
        entries.push(FieldEntry {
            address: Some(FieldAddress {
                segment: tag,
                field,
                repetition: None,
                component: None,
                subcomponent: None,
            }),
            raw_value: field_bytes.to_vec(),
            is_parsed_marker: false,
            is_editable: true,
        });
        return;
    }

    if opts.display_compound_fields {
        entries.push(FieldEntry {
            address: Some(FieldAddress {
                segment: tag,
                field,
                repetition: None,
                component: None,
                subcomponent: None,
            }),
            raw_value: field_bytes.to_vec(),
            is_parsed_marker: false,
            is_editable: true,
        });
    }

    for (rep_idx, rep_bytes) in field_bytes.split(|&b| b == delims.repetition).enumerate() {
        let repetition = if has_rep {
            NonZeroU16::new((rep_idx + 1) as u16)
        } else {
            None
        };
        let rep_has_comp = memchr::memchr(delims.component, rep_bytes).is_some();
        let rep_has_sub = memchr::memchr(delims.subcomponent, rep_bytes).is_some();

        if !rep_has_comp && !rep_has_sub {
            if !rep_bytes.is_empty() || opts.display_empty_fields {
                entries.push(FieldEntry {
                    address: Some(FieldAddress {
                        segment: tag,
                        field,
                        repetition,
                        component: None,
                        subcomponent: None,
                    }),
                    raw_value: rep_bytes.to_vec(),
                    is_parsed_marker: false,
                    is_editable: true,
                });
            }
            continue;
        }

        for (comp_idx, comp_bytes) in rep_bytes.split(|&b| b == delims.component).enumerate() {
            let Some(component) = NonZeroU16::new((comp_idx + 1) as u16) else {
                continue;
            };
            let comp_has_sub = memchr::memchr(delims.subcomponent, comp_bytes).is_some();
            if !comp_has_sub {
                if !comp_bytes.is_empty() || opts.display_empty_fields {
                    entries.push(FieldEntry {
                        address: Some(FieldAddress {
                            segment: tag,
                            field,
                            repetition,
                            component: Some(component),
                            subcomponent: None,
                        }),
                        raw_value: comp_bytes.to_vec(),
                        is_parsed_marker: false,
                        is_editable: true,
                    });
                }
            } else {
                for (sub_idx, sub_bytes) in
                    comp_bytes.split(|&b| b == delims.subcomponent).enumerate()
                {
                    if sub_bytes.is_empty() && !opts.display_empty_fields {
                        continue;
                    }
                    if let Some(subcomponent) = NonZeroU16::new((sub_idx + 1) as u16) {
                        entries.push(FieldEntry {
                            address: Some(FieldAddress {
                                segment: tag,
                                field,
                                repetition,
                                component: Some(component),
                                subcomponent: Some(subcomponent),
                            }),
                            raw_value: sub_bytes.to_vec(),
                            is_parsed_marker: false,
                            is_editable: true,
                        });
                    }
                }
            }
        }
    }
}
