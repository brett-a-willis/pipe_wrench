//! WinMerge-style structure-aware and inline character-level HL7 v2 message diff engine.
//!
//! Aligns segments side-by-side by `(segment_tag, occurrence_index)` using each message's own
//! dynamic `MSH-1`/`MSH-2` delimiters, compares matched segments field-by-field and
//! component-by-component with HL7 dictionary descriptions, and computes inline character-level
//! LCS diff spans (`Equal`, `Deleted`, `Inserted`) for every modified field/component.

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use crate::address::FieldAddress;
use crate::delimiters::Delimiters;
use crate::dictionary::Hl7Dictionary;

/// Options controlling [`diff_hl7_messages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// Ignore volatile message timestamps and control IDs (`MSH-7`, `MSH-10`, `EVN-2`).
    /// Defaults to `true`.
    pub ignore_volatile_ids_and_timestamps: bool,
    /// When `true`, omit unchanged rows/segments and show only differences.
    /// Defaults to `false` (full side-by-side context).
    pub differences_only: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            ignore_volatile_ids_and_timestamps: true,
            differences_only: false,
        }
    }
}

/// High-level change classification for a segment block or field/component row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffChangeKind {
    /// Identical on left and right (or ignored as volatile).
    Unchanged,
    /// Present on both sides with different values.
    Modified,
    /// Present only on the right (current) side.
    Added,
    /// Present only on the left (baseline) side.
    Removed,
}

/// Classification of an inline character-level diff span inside a modified field/component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharDiffKind {
    /// Character run identical on both sides.
    Equal,
    /// Character run removed from the left (baseline) value.
    Deleted,
    /// Character run inserted into the right (current) value.
    Inserted,
}

/// One contiguous inline character-level diff span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharDiffSpan {
    /// Span classification (`Equal`, `Deleted`, or `Inserted`).
    pub kind: CharDiffKind,
    /// Contiguous UTF-8 text for this span.
    pub text: String,
}

/// One aligned side-by-side field or component row inside a [`SegmentDiffBlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiffRow {
    /// Canonical HL7 path (e.g. `"PID-3.1"`, `"OBX(2)-5"`).
    pub path: String,
    /// Optional HL7 dictionary field/component description.
    pub description: Option<String>,
    /// Change classification for this row.
    pub kind: DiffChangeKind,
    /// Whether this row is a `Prev Diff` / `Next Diff` navigation stop (only the first row of an
    /// entirely added/removed segment is marked navigable so stepping jumps cleanly once per segment).
    pub is_navigable_diff: bool,
    /// Left (baseline) text, or `None` when absent on the left side.
    pub left_text: Option<String>,
    /// Right (current) text, or `None` when absent on the right side.
    pub right_text: Option<String>,
    /// Inline character-level diff spans for the left pane (`Equal` + `Deleted`).
    pub left_spans: Vec<CharDiffSpan>,
    /// Inline character-level diff spans for the right pane (`Equal` + `Inserted`).
    pub right_spans: Vec<CharDiffSpan>,
}

/// One aligned side-by-side segment block (`(segment_tag, occurrence)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDiffBlock {
    /// 3-character segment tag (e.g. `"PID"`, `"OBX"`).
    pub segment_tag: String,
    /// 1-based occurrence index of this segment tag in the message.
    pub occurrence: usize,
    /// Optional HL7 dictionary segment description.
    pub description: Option<String>,
    /// Aggregate change classification for this segment.
    pub kind: DiffChangeKind,
    /// Raw left segment line (or `None` when `Added`).
    pub left_raw: Option<String>,
    /// Raw right segment line (or `None` when `Removed`).
    pub right_raw: Option<String>,
    /// Aligned field/component diff rows for this segment.
    pub rows: Vec<FieldDiffRow>,
}

/// Complete WinMerge-style side-by-side diff result for two HL7 v2 messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDiffResult {
    /// Aligned segment blocks in display order.
    pub blocks: Vec<SegmentDiffBlock>,
    /// Total count of navigable difference rows (`Modified`, `Added`, `Removed`).
    pub total_diff_rows: usize,
    /// Active diff options that produced this result.
    pub options: DiffOptions,
}

impl MessageDiffResult {
    /// Format a concise plain-text diff summary suitable for clipboard export (`Copy Diff Summary`).
    pub fn format_summary(&self, left_label: &str, right_label: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "HL7 Diff Summary: {left_label} vs {right_label} ({} difference(s))\n",
            self.total_diff_rows
        ));
        if self.total_diff_rows == 0 {
            out.push_str("No differences found.\n");
            return out;
        }
        for block in &self.blocks {
            for row in &block.rows {
                if row.kind == DiffChangeKind::Unchanged {
                    continue;
                }
                let marker = match row.kind {
                    DiffChangeKind::Modified => "!=",
                    DiffChangeKind::Added => "+",
                    DiffChangeKind::Removed => "-",
                    DiffChangeKind::Unchanged => "==",
                };
                let desc = row
                    .description
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                let left = row.left_text.as_deref().unwrap_or("<missing>");
                let right = row.right_text.as_deref().unwrap_or("<missing>");
                out.push_str(&format!(
                    "[{marker}] {}{desc}: left=\"{left}\" | right=\"{right}\"\n",
                    row.path
                ));
            }
        }
        out
    }
}

/// Compute a structure-aware, inline character-level WinMerge-style diff between two HL7 v2 messages.
pub fn diff_hl7_messages(
    left_raw: &str,
    right_raw: &str,
    options: DiffOptions,
) -> MessageDiffResult {
    let dict = Hl7Dictionary::builtin();
    let left_parsed = parse_message_segments(left_raw);
    let right_parsed = parse_message_segments(right_raw);

    let left_delims = left_parsed.delims;
    let right_delims = right_parsed.delims;

    // Index right segments by (tag, occurrence) for O(1) alignment lookup.
    let mut right_by_key: BTreeMap<([u8; 3], usize), usize> = BTreeMap::new();
    for (idx, seg) in right_parsed.segments.iter().enumerate() {
        right_by_key.insert((seg.tag, seg.occurrence), idx);
    }

    // Build aligned segment pairs: preserve left order and interleave right-only segments
    // right before their next matched neighbor (or at the end).
    let mut aligned: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let mut right_emitted = vec![false; right_parsed.segments.len()];
    let mut next_right_unmatched = 0usize;

    for (l_idx, l_seg) in left_parsed.segments.iter().enumerate() {
        if let Some(&r_idx) = right_by_key.get(&(l_seg.tag, l_seg.occurrence)) {
            // Emit any preceding right-only segments whose keys don't appear in left
            while next_right_unmatched < r_idx {
                if !right_emitted[next_right_unmatched] {
                    let r_cand = &right_parsed.segments[next_right_unmatched];
                    let in_left = left_parsed
                        .segments
                        .iter()
                        .any(|ls| ls.tag == r_cand.tag && ls.occurrence == r_cand.occurrence);
                    if !in_left {
                        aligned.push((None, Some(next_right_unmatched)));
                        right_emitted[next_right_unmatched] = true;
                    }
                }
                next_right_unmatched += 1;
            }
            aligned.push((Some(l_idx), Some(r_idx)));
            right_emitted[r_idx] = true;
            if r_idx >= next_right_unmatched {
                next_right_unmatched = r_idx + 1;
            }
        } else {
            aligned.push((Some(l_idx), None));
        }
    }
    for (r_idx, &emitted) in right_emitted.iter().enumerate() {
        if !emitted {
            aligned.push((None, Some(r_idx)));
        }
    }

    let mut blocks: Vec<SegmentDiffBlock> = Vec::new();
    let mut total_diff_rows = 0usize;

    for (l_opt, r_opt) in aligned {
        match (l_opt, r_opt) {
            (Some(li), Some(ri)) => {
                let l_seg = &left_parsed.segments[li];
                let r_seg = &right_parsed.segments[ri];
                let tag_str = String::from_utf8_lossy(&l_seg.tag).into_owned();
                let seg_desc = dict.segment_description(&l_seg.tag).map(str::to_string);
                let mut rows = diff_matched_segment_fields(
                    l_seg,
                    r_seg,
                    left_delims,
                    right_delims,
                    &dict,
                    options,
                );
                let any_diff = rows.iter().any(|r| r.kind != DiffChangeKind::Unchanged);
                if options.differences_only {
                    rows.retain(|r| r.kind != DiffChangeKind::Unchanged);
                    if rows.is_empty() {
                        continue;
                    }
                }
                total_diff_rows += rows.iter().filter(|r| r.is_navigable_diff).count();
                blocks.push(SegmentDiffBlock {
                    segment_tag: tag_str,
                    occurrence: l_seg.occurrence,
                    description: seg_desc,
                    kind: if any_diff {
                        DiffChangeKind::Modified
                    } else {
                        DiffChangeKind::Unchanged
                    },
                    left_raw: Some(l_seg.raw.clone()),
                    right_raw: Some(r_seg.raw.clone()),
                    rows,
                });
            }
            (Some(li), None) => {
                let l_seg = &left_parsed.segments[li];
                let tag_str = String::from_utf8_lossy(&l_seg.tag).into_owned();
                let seg_desc = dict.segment_description(&l_seg.tag).map(str::to_string);
                let rows = expand_single_side_segment_rows(
                    l_seg,
                    left_delims,
                    DiffChangeKind::Removed,
                    &dict,
                    seg_desc.as_deref(),
                );
                total_diff_rows += rows.iter().filter(|r| r.is_navigable_diff).count();
                blocks.push(SegmentDiffBlock {
                    segment_tag: tag_str,
                    occurrence: l_seg.occurrence,
                    description: seg_desc,
                    kind: DiffChangeKind::Removed,
                    left_raw: Some(l_seg.raw.clone()),
                    right_raw: None,
                    rows,
                });
            }
            (None, Some(ri)) => {
                let r_seg = &right_parsed.segments[ri];
                let tag_str = String::from_utf8_lossy(&r_seg.tag).into_owned();
                let seg_desc = dict.segment_description(&r_seg.tag).map(str::to_string);
                let rows = expand_single_side_segment_rows(
                    r_seg,
                    right_delims,
                    DiffChangeKind::Added,
                    &dict,
                    seg_desc.as_deref(),
                );
                total_diff_rows += rows.iter().filter(|r| r.is_navigable_diff).count();
                blocks.push(SegmentDiffBlock {
                    segment_tag: tag_str,
                    occurrence: r_seg.occurrence,
                    description: seg_desc,
                    kind: DiffChangeKind::Added,
                    left_raw: None,
                    right_raw: Some(r_seg.raw.clone()),
                    rows,
                });
            }
            (None, None) => {}
        }
    }

    MessageDiffResult {
        blocks,
        total_diff_rows,
        options,
    }
}

/// Compute inline character-level diff spans (`(left_spans, right_spans)`) between two strings
/// using an LCS dynamic programming table (bounded for very long values).
pub fn compute_char_diff_spans(left: &str, right: &str) -> (Vec<CharDiffSpan>, Vec<CharDiffSpan>) {
    if left == right {
        let eq = if left.is_empty() {
            Vec::new()
        } else {
            vec![CharDiffSpan {
                kind: CharDiffKind::Equal,
                text: left.to_string(),
            }]
        };
        return (eq.clone(), eq);
    }

    let a: Vec<char> = left.chars().collect();
    let b: Vec<char> = right.chars().collect();

    // Trim common prefix & suffix first so even huge strings with localized edits run in O(1) memory.
    let mut prefix = 0usize;
    let min_len = a.len().min(b.len());
    while prefix < min_len && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < (a.len() - prefix).min(b.len() - prefix)
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mid_a = &a[prefix..a.len() - suffix];
    let mid_b = &b[prefix..b.len() - suffix];

    let mut left_spans: Vec<CharDiffSpan> = Vec::new();
    let mut right_spans: Vec<CharDiffSpan> = Vec::new();

    if prefix > 0 {
        let p: String = a[..prefix].iter().collect();
        push_span(&mut left_spans, CharDiffKind::Equal, &p);
        push_span(&mut right_spans, CharDiffKind::Equal, &p);
    }

    // Bound LCS matrix to ~1M cells (~4 MiB) so Base64 blobs never stall the UI.
    if mid_a.len().saturating_mul(mid_b.len()) <= 250_000 {
        let m = mid_a.len();
        let n = mid_b.len();
        let mut dp = vec![vec![0u32; n + 1]; m + 1];
        for i in (0..m).rev() {
            for j in (0..n).rev() {
                if mid_a[i] == mid_b[j] {
                    dp[i][j] = dp[i + 1][j + 1] + 1;
                } else {
                    dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
                }
            }
        }

        let mut i = 0usize;
        let mut j = 0usize;
        while i < m && j < n {
            if mid_a[i] == mid_b[j] {
                let mut buf = [0u8; 4];
                let s = mid_a[i].encode_utf8(&mut buf);
                push_span(&mut left_spans, CharDiffKind::Equal, s);
                push_span(&mut right_spans, CharDiffKind::Equal, s);
                i += 1;
                j += 1;
            } else if dp[i + 1][j] >= dp[i][j + 1] {
                let mut buf = [0u8; 4];
                let s = mid_a[i].encode_utf8(&mut buf);
                push_span(&mut left_spans, CharDiffKind::Deleted, s);
                i += 1;
            } else {
                let mut buf = [0u8; 4];
                let s = mid_b[j].encode_utf8(&mut buf);
                push_span(&mut right_spans, CharDiffKind::Inserted, s);
                j += 1;
            }
        }
        while i < m {
            let mut buf = [0u8; 4];
            let s = mid_a[i].encode_utf8(&mut buf);
            push_span(&mut left_spans, CharDiffKind::Deleted, s);
            i += 1;
        }
        while j < n {
            let mut buf = [0u8; 4];
            let s = mid_b[j].encode_utf8(&mut buf);
            push_span(&mut right_spans, CharDiffKind::Inserted, s);
            j += 1;
        }
    } else {
        if !mid_a.is_empty() {
            let s: String = mid_a.iter().collect();
            push_span(&mut left_spans, CharDiffKind::Deleted, &s);
        }
        if !mid_b.is_empty() {
            let s: String = mid_b.iter().collect();
            push_span(&mut right_spans, CharDiffKind::Inserted, &s);
        }
    }

    if suffix > 0 {
        let s: String = a[a.len() - suffix..].iter().collect();
        push_span(&mut left_spans, CharDiffKind::Equal, &s);
        push_span(&mut right_spans, CharDiffKind::Equal, &s);
    }

    (left_spans, right_spans)
}

fn push_span(spans: &mut Vec<CharDiffSpan>, kind: CharDiffKind, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.kind == kind {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(CharDiffSpan {
        kind,
        text: text.to_string(),
    });
}

struct ParsedSegment {
    tag: [u8; 3],
    occurrence: usize,
    raw: String,
    /// 1-based HL7 fields: `(field_number, raw_value)` (handles `MSH-1` and `MSH-2` properly).
    fields: Vec<(u16, String)>,
}

struct ParsedMessage {
    delims: Delimiters,
    segments: Vec<ParsedSegment>,
}

fn parse_message_segments(raw: &str) -> ParsedMessage {
    let trimmed = raw.trim_matches(|c| c == '\r' || c == '\n' || c == '\x0B' || c == '\x1C');
    let delims = Delimiters::from_header_bytes(trimmed.as_bytes()).unwrap_or(Delimiters::STANDARD);
    let mut counts: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    let mut segments = Vec::new();

    for line in trimmed.split(['\r', '\n']) {
        let line = line.trim_matches(|c| c == '\x0B' || c == '\x1C');
        if line.len() < 3 {
            continue;
        }
        let b = line.as_bytes();
        let tag = [b[0], b[1], b[2]];
        let occ = counts.entry(tag).or_insert(0);
        *occ += 1;
        let occurrence = *occ;

        let is_header = &tag == b"MSH" || &tag == b"FHS" || &tag == b"BHS";
        let mut fields = Vec::new();
        let fs_ch = char::from(delims.field);

        if is_header && line.len() >= 4 && line.is_char_boundary(4) {
            // Field 1 = field separator, Field 2 = encoding chars, Field 3+ = split after second fs
            fields.push((1, fs_ch.to_string()));
            let rest = &line[4..];
            let mut parts = rest.split(fs_ch);
            if let Some(enc) = parts.next() {
                fields.push((2, enc.to_string()));
            }
            for (idx, part) in parts.enumerate() {
                fields.push(((idx + 3) as u16, part.to_string()));
            }
        } else if line.len() >= 4 && b[3] == delims.field && line.is_char_boundary(4) {
            for (idx, part) in line[4..].split(fs_ch).enumerate() {
                fields.push(((idx + 1) as u16, part.to_string()));
            }
        }

        segments.push(ParsedSegment {
            tag,
            occurrence,
            raw: line.to_string(),
            fields,
        });
    }

    ParsedMessage { delims, segments }
}

fn is_volatile_field(tag: &[u8; 3], field_num: u16) -> bool {
    (tag == b"MSH" && (field_num == 7 || field_num == 10)) || (tag == b"EVN" && field_num == 2)
}

fn format_seg_prefix(tag: &str, occ: usize) -> String {
    if occ > 1 {
        format!("{tag}({occ})")
    } else {
        tag.to_string()
    }
}

fn diff_matched_segment_fields(
    left: &ParsedSegment,
    right: &ParsedSegment,
    left_delims: Delimiters,
    right_delims: Delimiters,
    dict: &Hl7Dictionary,
    options: DiffOptions,
) -> Vec<FieldDiffRow> {
    let tag_str = String::from_utf8_lossy(&left.tag).into_owned();
    let seg_prefix = format_seg_prefix(&tag_str, left.occurrence);
    let max_field = left
        .fields
        .iter()
        .map(|(f, _)| *f)
        .chain(right.fields.iter().map(|(f, _)| *f))
        .max()
        .unwrap_or(0);

    let mut rows = Vec::new();
    for fnum in 1..=max_field {
        let l_val = left
            .fields
            .iter()
            .find(|(f, _)| *f == fnum)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let r_val = right
            .fields
            .iter()
            .find(|(f, _)| *f == fnum)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");

        if l_val.is_empty() && r_val.is_empty() {
            continue;
        }

        let is_header_delim =
            (&left.tag == b"MSH" || &left.tag == b"FHS" || &left.tag == b"BHS") && fnum <= 2;
        let ignored_volatile =
            options.ignore_volatile_ids_and_timestamps && is_volatile_field(&left.tag, fnum);

        let l_comp = char::from(left_delims.component);
        let r_comp = char::from(right_delims.component);
        let has_components = !is_header_delim && (l_val.contains(l_comp) || r_val.contains(r_comp));

        if !ignored_volatile && has_components && l_val != r_val {
            // Component-by-component breakdown when a composite field differs
            let l_parts: Vec<&str> = l_val.split(l_comp).collect();
            let r_parts: Vec<&str> = r_val.split(r_comp).collect();
            let max_c = l_parts.len().max(r_parts.len());
            for c_idx in 0..max_c {
                let lc = l_parts.get(c_idx).copied().unwrap_or("");
                let rc = r_parts.get(c_idx).copied().unwrap_or("");
                if lc.is_empty() && rc.is_empty() {
                    continue;
                }
                let cnum = (c_idx + 1) as u16;
                let path = format!("{seg_prefix}-{fnum}.{cnum}");
                let desc = NonZeroU16::new(fnum).and_then(|nz_f| {
                    let addr = FieldAddress {
                        segment: left.tag,
                        field: nz_f,
                        repetition: None,
                        component: NonZeroU16::new(cnum),
                        subcomponent: None,
                    };
                    dict.field_description(&addr).map(str::to_string)
                });
                rows.push(build_field_diff_row(path, desc, lc, rc, false));
            }
        } else {
            let path = format!("{seg_prefix}-{fnum}");
            let desc = NonZeroU16::new(fnum).and_then(|nz_f| {
                let addr = FieldAddress {
                    segment: left.tag,
                    field: nz_f,
                    repetition: None,
                    component: None,
                    subcomponent: None,
                };
                dict.field_description(&addr).map(str::to_string)
            });
            rows.push(build_field_diff_row(
                path,
                desc,
                l_val,
                r_val,
                ignored_volatile,
            ));
        }
    }
    rows
}

fn expand_single_side_segment_rows(
    seg: &ParsedSegment,
    delims: Delimiters,
    kind: DiffChangeKind,
    dict: &Hl7Dictionary,
    seg_desc: Option<&str>,
) -> Vec<FieldDiffRow> {
    let tag_str = String::from_utf8_lossy(&seg.tag).into_owned();
    let seg_prefix = format_seg_prefix(&tag_str, seg.occurrence);
    let comp_sep = char::from(delims.component);
    let is_header = &seg.tag == b"MSH" || &seg.tag == b"FHS" || &seg.tag == b"BHS";
    let mut rows = Vec::new();

    for (fnum, val) in &seg.fields {
        if val.is_empty() {
            continue;
        }
        let is_header_delim = is_header && *fnum <= 2;
        if !is_header_delim && val.contains(comp_sep) {
            for (c_idx, comp) in val.split(comp_sep).enumerate() {
                if comp.is_empty() {
                    continue;
                }
                let cnum = (c_idx + 1) as u16;
                let path = format!("{seg_prefix}-{fnum}.{cnum}");
                let desc = NonZeroU16::new(*fnum).and_then(|nz_f| {
                    let addr = FieldAddress {
                        segment: seg.tag,
                        field: nz_f,
                        repetition: None,
                        component: NonZeroU16::new(cnum),
                        subcomponent: None,
                    };
                    dict.field_description(&addr).map(str::to_string)
                });
                let (l, r) = match kind {
                    DiffChangeKind::Removed => (comp, ""),
                    _ => ("", comp),
                };
                let mut row = build_field_diff_row(path, desc, l, r, false);
                row.is_navigable_diff = rows.is_empty();
                rows.push(row);
            }
        } else {
            let path = format!("{seg_prefix}-{fnum}");
            let desc = NonZeroU16::new(*fnum).and_then(|nz_f| {
                let addr = FieldAddress {
                    segment: seg.tag,
                    field: nz_f,
                    repetition: None,
                    component: None,
                    subcomponent: None,
                };
                dict.field_description(&addr).map(str::to_string)
            });
            let (l, r) = match kind {
                DiffChangeKind::Removed => (val.as_str(), ""),
                _ => ("", val.as_str()),
            };
            let mut row = build_field_diff_row(path, desc, l, r, false);
            row.is_navigable_diff = rows.is_empty();
            rows.push(row);
        }
    }

    if rows.is_empty() {
        let (l, r) = match kind {
            DiffChangeKind::Removed => (seg.raw.as_str(), ""),
            _ => ("", seg.raw.as_str()),
        };
        let mut row = build_field_diff_row(seg_prefix, seg_desc.map(str::to_string), l, r, false);
        row.is_navigable_diff = true;
        rows.push(row);
    }
    rows
}

fn build_field_diff_row(
    path: String,
    description: Option<String>,
    left: &str,
    right: &str,
    force_unchanged: bool,
) -> FieldDiffRow {
    if force_unchanged || left == right {
        let l_span = if left.is_empty() {
            Vec::new()
        } else {
            vec![CharDiffSpan {
                kind: CharDiffKind::Equal,
                text: left.to_string(),
            }]
        };
        let r_span = if right.is_empty() {
            Vec::new()
        } else {
            vec![CharDiffSpan {
                kind: CharDiffKind::Equal,
                text: right.to_string(),
            }]
        };
        return FieldDiffRow {
            path,
            description,
            kind: DiffChangeKind::Unchanged,
            is_navigable_diff: false,
            left_text: (!left.is_empty()).then(|| left.to_string()),
            right_text: (!right.is_empty()).then(|| right.to_string()),
            left_spans: l_span,
            right_spans: r_span,
        };
    }

    if left.is_empty() {
        return FieldDiffRow {
            path,
            description,
            kind: DiffChangeKind::Added,
            is_navigable_diff: true,
            left_text: None,
            right_text: Some(right.to_string()),
            left_spans: Vec::new(),
            right_spans: vec![CharDiffSpan {
                kind: CharDiffKind::Inserted,
                text: right.to_string(),
            }],
        };
    }

    if right.is_empty() {
        return FieldDiffRow {
            path,
            description,
            kind: DiffChangeKind::Removed,
            is_navigable_diff: true,
            left_text: Some(left.to_string()),
            right_text: None,
            left_spans: vec![CharDiffSpan {
                kind: CharDiffKind::Deleted,
                text: left.to_string(),
            }],
            right_spans: Vec::new(),
        };
    }

    let (left_spans, right_spans) = compute_char_diff_spans(left, right);
    FieldDiffRow {
        path,
        description,
        kind: DiffChangeKind::Modified,
        is_navigable_diff: true,
        left_text: Some(left.to_string()),
        right_text: Some(right.to_string()),
        left_spans,
        right_spans,
    }
}

/// One target message cell inside a [`MultiFieldDiffRow`], compared independently against Baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFieldDiffCell {
    /// Change classification of this target cell relative to the Baseline.
    pub kind: DiffChangeKind,
    /// Target field/component text, or `None` when absent in this target message.
    pub text: Option<String>,
    /// Inline character-level diff spans for the Baseline when compared against this target.
    pub baseline_spans: Vec<CharDiffSpan>,
    /// Inline character-level diff spans for this target (`Equal` + `Inserted`).
    pub target_spans: Vec<CharDiffSpan>,
}

/// One aligned multi-column field or component row inside a [`MultiSegmentDiffBlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiFieldDiffRow {
    /// Canonical HL7 path (e.g. `"PID-3.1"`, `"OBX(2)-5"`).
    pub path: String,
    /// Optional HL7 dictionary field/component description.
    pub description: Option<String>,
    /// `true` when at least one target column differs from the Baseline.
    pub has_any_diff: bool,
    /// Whether this row is a `Prev Diff` / `Next Diff` navigation stop (only the first differing
    /// row of a segment absent in one or more messages is marked navigable).
    pub is_navigable_diff: bool,
    /// Baseline text, or `None` when absent in the Baseline message.
    pub baseline_text: Option<String>,
    /// One diff cell per target message column (in target order).
    pub cells: Vec<TargetFieldDiffCell>,
}

/// One aligned multi-column segment block (`(segment_tag, occurrence)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiSegmentDiffBlock {
    /// 3-character segment tag (e.g. `"PID"`, `"OBX"`).
    pub segment_tag: String,
    /// 1-based occurrence index of this segment tag.
    pub occurrence: usize,
    /// Optional HL7 dictionary segment description.
    pub description: Option<String>,
    /// Aggregate change classification (`Added`, `Removed`, `Modified`, or `Unchanged`).
    pub kind: DiffChangeKind,
    /// `true` when any row in this segment block differs from Baseline in any target.
    pub has_any_diff: bool,
    /// Raw Baseline segment line (or `None` when absent in Baseline).
    pub baseline_raw: Option<String>,
    /// Raw segment line for each target column (or `None` when absent in that target).
    pub target_raws: Vec<Option<String>>,
    /// Aligned field/component diff rows for this segment.
    pub rows: Vec<MultiFieldDiffRow>,
}

/// Complete multi-message diff result comparing 1 Baseline message against 1..N target messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiMessageDiffResult {
    /// Display labels for each target column.
    pub target_labels: Vec<String>,
    /// Aligned segment blocks in display order.
    pub blocks: Vec<MultiSegmentDiffBlock>,
    /// Total count of navigable difference rows (`has_any_diff == true`).
    pub total_diff_rows: usize,
    /// Active diff options that produced this result.
    pub options: DiffOptions,
}

impl MultiMessageDiffResult {
    /// Format a concise plain-text diff summary suitable for clipboard export (`Copy Diff Summary`).
    pub fn format_summary(&self, baseline_label: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "HL7 Multi-Message Diff Summary: Baseline={baseline_label} vs {} target(s) ({} difference(s))\n",
            self.target_labels.len(),
            self.total_diff_rows
        ));
        if self.total_diff_rows == 0 {
            out.push_str("No differences found.\n");
            return out;
        }
        for block in &self.blocks {
            for row in &block.rows {
                if !row.has_any_diff {
                    continue;
                }
                let desc = row
                    .description
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                let base = row.baseline_text.as_deref().unwrap_or("<missing>");
                out.push_str(&format!("[!=] {}{desc}: baseline=\"{base}\"", row.path));
                for (idx, cell) in row.cells.iter().enumerate() {
                    if cell.kind == DiffChangeKind::Unchanged {
                        continue;
                    }
                    let label = self
                        .target_labels
                        .get(idx)
                        .map(String::as_str)
                        .unwrap_or("Target");
                    let val = cell.text.as_deref().unwrap_or("<missing>");
                    let tag = match cell.kind {
                        DiffChangeKind::Modified => "!=",
                        DiffChangeKind::Added => "+",
                        DiffChangeKind::Removed => "-",
                        DiffChangeKind::Unchanged => "==",
                    };
                    out.push_str(&format!(" | {label} [{tag}]=\"{val}\""));
                }
                out.push('\n');
            }
        }
        out
    }
}

/// Compare a Baseline HL7 v2 message against one or more `(label, raw_hl7)` target messages.
///
/// Segments are aligned across the Baseline and all targets by `(segment_tag, occurrence_index)`
/// so every column stays row-synchronized even when one message adds or omits segments.
pub fn diff_hl7_message_set(
    baseline_raw: &str,
    targets: &[(&str, &str)],
    options: DiffOptions,
) -> MultiMessageDiffResult {
    let dict = Hl7Dictionary::builtin();
    let base_parsed = parse_message_segments(baseline_raw);
    let target_parsed: Vec<ParsedMessage> = targets
        .iter()
        .map(|(_, raw)| parse_message_segments(raw))
        .collect();
    let target_labels: Vec<String> = targets.iter().map(|(lbl, _)| (*lbl).to_string()).collect();

    // Build an index for O(1) lookup of (tag, occurrence) -> segment index in each message.
    let mut base_by_key: BTreeMap<([u8; 3], usize), usize> = BTreeMap::new();
    for (idx, seg) in base_parsed.segments.iter().enumerate() {
        base_by_key.insert((seg.tag, seg.occurrence), idx);
    }
    let target_by_key: Vec<BTreeMap<([u8; 3], usize), usize>> = target_parsed
        .iter()
        .map(|pm| {
            pm.segments
                .iter()
                .enumerate()
                .map(|(idx, seg)| ((seg.tag, seg.occurrence), idx))
                .collect()
        })
        .collect();

    // Build synchronized segment key order: start with Baseline order, then interleave
    // target-only segments right before their next already-placed neighbor (or at the end).
    let mut ordered_keys: Vec<([u8; 3], usize)> = base_parsed
        .segments
        .iter()
        .map(|s| (s.tag, s.occurrence))
        .collect();

    for pm in &target_parsed {
        for (idx, seg) in pm.segments.iter().enumerate() {
            let key = (seg.tag, seg.occurrence);
            if ordered_keys.contains(&key) {
                continue;
            }
            // Find the earliest subsequent segment in `pm` that is already in `ordered_keys`
            let insert_pos = pm.segments[idx + 1..]
                .iter()
                .find_map(|next_seg| {
                    let nk = (next_seg.tag, next_seg.occurrence);
                    ordered_keys.iter().position(|k| *k == nk)
                })
                .unwrap_or(ordered_keys.len());
            ordered_keys.insert(insert_pos, key);
        }
    }

    let mut blocks: Vec<MultiSegmentDiffBlock> = Vec::new();
    let mut total_diff_rows = 0usize;

    for (tag, occ) in ordered_keys {
        let tag_str = String::from_utf8_lossy(&tag).into_owned();
        let seg_desc = dict.segment_description(&tag).map(str::to_string);
        let base_seg = base_by_key
            .get(&(tag, occ))
            .map(|&i| &base_parsed.segments[i]);
        let target_segs: Vec<Option<&ParsedSegment>> = target_parsed
            .iter()
            .enumerate()
            .map(|(ti, pm)| target_by_key[ti].get(&(tag, occ)).map(|&i| &pm.segments[i]))
            .collect();

        let target_raws: Vec<Option<String>> = target_segs
            .iter()
            .map(|opt| opt.map(|s| s.raw.clone()))
            .collect();

        let any_target_present = target_segs.iter().any(Option::is_some);
        let target_delims: Vec<Delimiters> = target_parsed.iter().map(|pm| pm.delims).collect();
        let segment_absent_in_some = base_seg.is_none() || target_segs.iter().any(Option::is_none);

        let mut rows = diff_segment_fields_multi(
            tag,
            occ,
            base_seg,
            &target_segs,
            base_parsed.delims,
            &target_delims,
            &dict,
            options,
            seg_desc.as_deref(),
        );
        let has_any_diff = rows.iter().any(|r| r.has_any_diff);
        if options.differences_only {
            rows.retain(|r| r.has_any_diff);
            if rows.is_empty() {
                continue;
            }
        }
        // Smart Diff Stepping: when an entire segment is absent in one or more messages,
        // count the entire absent segment as a single `Prev Diff` / `Next Diff` stop.
        if segment_absent_in_some {
            let mut marked_first = false;
            for row in &mut rows {
                if row.has_any_diff && !marked_first {
                    row.is_navigable_diff = true;
                    marked_first = true;
                } else {
                    row.is_navigable_diff = false;
                }
            }
        }
        let block_kind = match (base_seg.is_some(), any_target_present, has_any_diff) {
            (false, true, _) => DiffChangeKind::Added,
            (true, false, _) => DiffChangeKind::Removed,
            (_, _, true) => DiffChangeKind::Modified,
            _ => DiffChangeKind::Unchanged,
        };
        total_diff_rows += rows.iter().filter(|r| r.is_navigable_diff).count();
        blocks.push(MultiSegmentDiffBlock {
            segment_tag: tag_str,
            occurrence: occ,
            description: seg_desc,
            kind: block_kind,
            has_any_diff,
            baseline_raw: base_seg.map(|s| s.raw.clone()),
            target_raws,
            rows,
        });
    }

    MultiMessageDiffResult {
        target_labels,
        blocks,
        total_diff_rows,
        options,
    }
}

#[allow(clippy::too_many_arguments)]
fn diff_segment_fields_multi(
    tag: [u8; 3],
    occ: usize,
    base: Option<&ParsedSegment>,
    targets: &[Option<&ParsedSegment>],
    base_delims: Delimiters,
    target_delims: &[Delimiters],
    dict: &Hl7Dictionary,
    options: DiffOptions,
    seg_desc: Option<&str>,
) -> Vec<MultiFieldDiffRow> {
    let tag_str = String::from_utf8_lossy(&tag).into_owned();
    let seg_prefix = format_seg_prefix(&tag_str, occ);

    let mut max_field = base
        .and_then(|b| b.fields.iter().map(|(f, _)| *f).max())
        .unwrap_or(0);
    for tseg in targets.iter().flatten() {
        if let Some(m) = tseg.fields.iter().map(|(f, _)| *f).max() {
            max_field = max_field.max(m);
        }
    }

    let mut rows = Vec::new();
    for fnum in 1..=max_field {
        let b_val = base
            .and_then(|b| b.fields.iter().find(|(f, _)| *f == fnum))
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let t_vals: Vec<&str> = targets
            .iter()
            .map(|opt| {
                opt.and_then(|s| s.fields.iter().find(|(f, _)| *f == fnum))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("")
            })
            .collect();

        if b_val.is_empty() && t_vals.iter().all(|v| v.is_empty()) {
            continue;
        }

        let is_header_delim = (&tag == b"MSH" || &tag == b"FHS" || &tag == b"BHS") && fnum <= 2;
        let segment_absent_in_some = base.is_none() || targets.iter().any(Option::is_none);
        let ignored_volatile = !segment_absent_in_some
            && options.ignore_volatile_ids_and_timestamps
            && is_volatile_field(&tag, fnum);

        let b_comp = char::from(base_delims.component);
        let any_has_components = !is_header_delim
            && (b_val.contains(b_comp)
                || t_vals
                    .iter()
                    .enumerate()
                    .any(|(i, v)| v.contains(char::from(target_delims[i].component))));
        let any_differs = t_vals.iter().any(|&tv| tv != b_val);

        if !ignored_volatile && any_has_components && any_differs {
            let b_parts: Vec<&str> = b_val.split(b_comp).collect();
            let t_parts: Vec<Vec<&str>> = t_vals
                .iter()
                .enumerate()
                .map(|(i, v)| v.split(char::from(target_delims[i].component)).collect())
                .collect();
            let mut max_c = b_parts.len();
            for tp in &t_parts {
                max_c = max_c.max(tp.len());
            }
            for c_idx in 0..max_c {
                let bc = b_parts.get(c_idx).copied().unwrap_or("");
                let tcs: Vec<&str> = t_parts
                    .iter()
                    .map(|tp| tp.get(c_idx).copied().unwrap_or(""))
                    .collect();
                if bc.is_empty() && tcs.iter().all(|v| v.is_empty()) {
                    continue;
                }
                let cnum = (c_idx + 1) as u16;
                let path = format!("{seg_prefix}-{fnum}.{cnum}");
                let desc = NonZeroU16::new(fnum).and_then(|nz_f| {
                    let addr = FieldAddress {
                        segment: tag,
                        field: nz_f,
                        repetition: None,
                        component: NonZeroU16::new(cnum),
                        subcomponent: None,
                    };
                    dict.field_description(&addr).map(str::to_string)
                });
                let cells: Vec<TargetFieldDiffCell> = tcs
                    .iter()
                    .map(|&tc| build_target_diff_cell(bc, tc, false))
                    .collect();
                let has_any_diff = cells.iter().any(|c| c.kind != DiffChangeKind::Unchanged);
                rows.push(MultiFieldDiffRow {
                    path,
                    description: desc,
                    has_any_diff,
                    is_navigable_diff: has_any_diff,
                    baseline_text: (!bc.is_empty()).then(|| bc.to_string()),
                    cells,
                });
            }
        } else {
            let path = format!("{seg_prefix}-{fnum}");
            let desc = NonZeroU16::new(fnum).and_then(|nz_f| {
                let addr = FieldAddress {
                    segment: tag,
                    field: nz_f,
                    repetition: None,
                    component: None,
                    subcomponent: None,
                };
                dict.field_description(&addr).map(str::to_string)
            });
            let cells: Vec<TargetFieldDiffCell> = t_vals
                .iter()
                .map(|&tv| build_target_diff_cell(b_val, tv, ignored_volatile))
                .collect();
            let has_any_diff = cells.iter().any(|c| c.kind != DiffChangeKind::Unchanged);
            rows.push(MultiFieldDiffRow {
                path,
                description: desc,
                has_any_diff,
                is_navigable_diff: has_any_diff,
                baseline_text: (!b_val.is_empty()).then(|| b_val.to_string()),
                cells,
            });
        }
    }

    if rows.is_empty() {
        let b_raw = base.map(|s| s.raw.as_str()).unwrap_or("");
        let cells: Vec<TargetFieldDiffCell> = targets
            .iter()
            .map(|opt| {
                build_target_diff_cell(b_raw, opt.map(|s| s.raw.as_str()).unwrap_or(""), false)
            })
            .collect();
        let has_any_diff = cells.iter().any(|c| c.kind != DiffChangeKind::Unchanged);
        rows.push(MultiFieldDiffRow {
            path: seg_prefix,
            description: seg_desc.map(str::to_string),
            has_any_diff,
            is_navigable_diff: has_any_diff,
            baseline_text: (!b_raw.is_empty()).then(|| b_raw.to_string()),
            cells,
        });
    }

    rows
}

fn build_target_diff_cell(
    baseline: &str,
    target: &str,
    force_unchanged: bool,
) -> TargetFieldDiffCell {
    if force_unchanged || baseline == target {
        let b_span = if baseline.is_empty() {
            Vec::new()
        } else {
            vec![CharDiffSpan {
                kind: CharDiffKind::Equal,
                text: baseline.to_string(),
            }]
        };
        let t_span = if target.is_empty() {
            Vec::new()
        } else {
            vec![CharDiffSpan {
                kind: CharDiffKind::Equal,
                text: target.to_string(),
            }]
        };
        return TargetFieldDiffCell {
            kind: DiffChangeKind::Unchanged,
            text: (!target.is_empty()).then(|| target.to_string()),
            baseline_spans: b_span,
            target_spans: t_span,
        };
    }

    if baseline.is_empty() {
        return TargetFieldDiffCell {
            kind: DiffChangeKind::Added,
            text: Some(target.to_string()),
            baseline_spans: Vec::new(),
            target_spans: vec![CharDiffSpan {
                kind: CharDiffKind::Inserted,
                text: target.to_string(),
            }],
        };
    }

    if target.is_empty() {
        return TargetFieldDiffCell {
            kind: DiffChangeKind::Removed,
            text: None,
            baseline_spans: vec![CharDiffSpan {
                kind: CharDiffKind::Deleted,
                text: baseline.to_string(),
            }],
            target_spans: Vec::new(),
        };
    }

    let (baseline_spans, target_spans) = compute_char_diff_spans(baseline, target);
    TargetFieldDiffCell {
        kind: DiffChangeKind::Modified,
        text: Some(target.to_string()),
        baseline_spans,
        target_spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_segment_occurrences_highlights_inline_chars_and_filters_volatile_fields() {
        let left = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ORU^R01|CTRL_OLD|P|2.5.1\r",
            "EVN|A01|20261123080000\r",
            "PID|1||MRN12345^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "OBX|2|NM|8310-5^Body Temp^LN||98.6|F\r",
            "NTE|1|L|Left-only note\r",
        );
        let right = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261124091500||ORU^R01|CTRL_NEW|P|2.5.1\r",
            "EVN|A01|20261124091500\r",
            "PID|1||MRN12349^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "OBX|2|NM|8310-5^Body Temp^LN||99.1|F\r",
            "ZDS|1|Right-only Z segment\r",
        );

        // 1. Default options: volatile MSH-7, MSH-10, EVN-2 are ignored!
        let res = diff_hl7_messages(left, right, DiffOptions::default());
        assert_eq!(res.blocks[0].segment_tag, "MSH");
        assert_eq!(res.blocks[0].kind, DiffChangeKind::Unchanged);
        assert_eq!(res.blocks[1].segment_tag, "EVN");
        assert_eq!(res.blocks[1].kind, DiffChangeKind::Unchanged);

        // PID-3.1 single-character diff: "MRN1234" Equal + "5" Deleted vs "9" Inserted
        let pid_block = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "PID")
            .expect("PID block");
        let pid3_1 = pid_block
            .rows
            .iter()
            .find(|r| r.path == "PID-3.1")
            .expect("PID-3.1 row");
        assert_eq!(pid3_1.kind, DiffChangeKind::Modified);
        assert_eq!(
            pid3_1.left_spans,
            vec![
                CharDiffSpan {
                    kind: CharDiffKind::Equal,
                    text: "MRN1234".to_string()
                },
                CharDiffSpan {
                    kind: CharDiffKind::Deleted,
                    text: "5".to_string()
                },
            ]
        );
        assert_eq!(
            pid3_1.right_spans,
            vec![
                CharDiffSpan {
                    kind: CharDiffKind::Equal,
                    text: "MRN1234".to_string()
                },
                CharDiffSpan {
                    kind: CharDiffKind::Inserted,
                    text: "9".to_string()
                },
            ]
        );

        // OBX #1 unchanged, OBX #2 modified at OBX(2)-5
        let obx1 = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "OBX" && b.occurrence == 1)
            .expect("OBX #1");
        assert_eq!(obx1.kind, DiffChangeKind::Unchanged);
        let obx2 = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "OBX" && b.occurrence == 2)
            .expect("OBX #2");
        assert_eq!(obx2.kind, DiffChangeKind::Modified);
        assert!(obx2
            .rows
            .iter()
            .any(|r| r.path == "OBX(2)-5" && r.kind == DiffChangeKind::Modified));

        // NTE removed (left-only placeholder) and ZDS added (right-only placeholder)
        let nte = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "NTE")
            .expect("NTE removed");
        assert_eq!(nte.kind, DiffChangeKind::Removed);
        assert!(nte.left_raw.is_some() && nte.right_raw.is_none());

        let zds = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "ZDS")
            .expect("ZDS added");
        assert_eq!(zds.kind, DiffChangeKind::Added);
        assert!(zds.left_raw.is_none() && zds.right_raw.is_some());

        // 2. With ignore_volatile_ids_and_timestamps = false & differences_only = true:
        //    MSH-7, MSH-10, and EVN-2 now appear as Modified rows!
        let res_strict = diff_hl7_messages(
            left,
            right,
            DiffOptions {
                ignore_volatile_ids_and_timestamps: false,
                differences_only: true,
            },
        );
        let all_paths: Vec<&str> = res_strict
            .blocks
            .iter()
            .flat_map(|b| b.rows.iter().map(|r| r.path.as_str()))
            .collect();
        assert!(all_paths.contains(&"MSH-7"));
        assert!(all_paths.contains(&"MSH-10"));
        assert!(all_paths.contains(&"EVN-2"));
        assert!(all_paths.contains(&"PID-3.1"));
        assert!(all_paths.contains(&"OBX(2)-5"));
        assert!(!all_paths.contains(&"OBX-5"));
    }

    #[test]
    fn multi_message_diff_aligns_up_to_five_messages_against_baseline() {
        let base = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ORU^R01|BASE1|P|2.5.1\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "NTE|1|L|Baseline note\r",
        );
        // Target 1: Identical clinical payload (only MSH-7 & MSH-10 differ -> ignored by default)
        let t1 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123090000||ORU^R01|TGT1|P|2.5.1\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "NTE|1|L|Baseline note\r",
        );
        // Target 2: Modified PID-3.1 ("MRN1009") + extra OBX(2)
        let t2 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123100000||ORU^R01|TGT2|P|2.5.1\r",
            "PID|1||MRN1009^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "OBX|2|NM|8310-5^Body Temp^LN||99.1|F\r",
            "NTE|1|L|Baseline note\r",
        );
        // Target 3: Omitted NTE + added ZCUSTOM
        let t3 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123110000||ORU^R01|TGT3|P|2.5.1\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||85|/min\r",
            "ZCS|1|CustomZ\r",
        );
        // Target 4: Modified PID-5.2 ("JANET")
        let t4 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123120000||ORU^R01|TGT4|P|2.5.1\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANET||19800101|F\r",
            "OBX|1|NM|8867-4^Heart Rate^LN||72|/min\r",
            "NTE|1|L|Baseline note\r",
        );

        let targets = [
            ("Msg #2", t1),
            ("Msg #3", t2),
            ("Msg #4", t3),
            ("Msg #5", t4),
        ];
        let res = diff_hl7_message_set(base, &targets, DiffOptions::default());
        assert_eq!(res.target_labels.len(), 4);

        // MSH should be unchanged across all 4 targets when ignoring volatile IDs/timestamps
        let msh = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "MSH")
            .expect("MSH block");
        assert!(!msh.has_any_diff);

        // PID-3.1: Target 2 modified ("MRN100" Equal + "9" Inserted), others Unchanged
        let pid = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "PID")
            .expect("PID block");
        let pid3_1 = pid
            .rows
            .iter()
            .find(|r| r.path == "PID-3.1")
            .expect("PID-3.1");
        assert!(pid3_1.has_any_diff);
        assert_eq!(pid3_1.cells[0].kind, DiffChangeKind::Unchanged);
        assert_eq!(pid3_1.cells[1].kind, DiffChangeKind::Modified);
        assert_eq!(
            pid3_1.cells[1].target_spans,
            vec![
                CharDiffSpan {
                    kind: CharDiffKind::Equal,
                    text: "MRN100".to_string()
                },
                CharDiffSpan {
                    kind: CharDiffKind::Inserted,
                    text: "9".to_string()
                },
            ]
        );
        assert_eq!(pid3_1.cells[2].kind, DiffChangeKind::Unchanged);
        assert_eq!(pid3_1.cells[3].kind, DiffChangeKind::Unchanged);

        // OBX(2) only present in Target 2 -> Added in cell[1], Unchanged/None in others
        let obx2 = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "OBX" && b.occurrence == 2)
            .expect("OBX(2) block");
        assert!(obx2.has_any_diff);
        assert_eq!(obx2.rows[0].cells[0].kind, DiffChangeKind::Unchanged);
        assert_eq!(obx2.rows[0].cells[1].kind, DiffChangeKind::Added);
        assert_eq!(obx2.rows[0].cells[2].kind, DiffChangeKind::Unchanged);
        assert_eq!(obx2.rows[0].cells[3].kind, DiffChangeKind::Unchanged);

        // ZCS only present in Target 3 -> Added in cell[2]
        let zcs = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "ZCS")
            .expect("ZCS block");
        assert!(zcs.has_any_diff);
        assert_eq!(zcs.rows[0].cells[2].kind, DiffChangeKind::Added);

        // NTE omitted in Target 3 -> Removed in cell[2] for NTE-3
        let nte = res
            .blocks
            .iter()
            .find(|b| b.segment_tag == "NTE")
            .expect("NTE block");
        assert!(nte.has_any_diff);
        let nte3 = nte.rows.iter().find(|r| r.path == "NTE-3").expect("NTE-3");
        assert_eq!(nte3.cells[0].kind, DiffChangeKind::Unchanged);
        assert_eq!(nte3.cells[2].kind, DiffChangeKind::Removed);

        // differences_only omits MSH and retains only diff rows
        let res_only = diff_hl7_message_set(
            base,
            &targets,
            DiffOptions {
                ignore_volatile_ids_and_timestamps: true,
                differences_only: true,
            },
        );
        assert!(!res_only.blocks.iter().any(|b| b.segment_tag == "MSH"));
        assert!(res_only.total_diff_rows >= 5);
        let summary = res_only.format_summary("Msg #1");
        assert!(summary.contains("PID-3.1"));
        assert!(summary.contains("ZCS"));
    }

    #[test]
    fn absent_segments_expand_into_field_rows_with_single_navigation_stop() {
        // 1. 2-message diff: EVN only in left (Removed), ZCS only in right (Added),
        //    both with empty middle/trailing fields that should be skipped.
        let left = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "EVN|A01|20260115120000||01|OP1^SMITH|||\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANE||19800101|F\r",
        );
        let right = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "PID|1||MRN1000^^^HOSP^MR||DOE^JANE||19800101|F\r",
            "ZCS|1|Alpha^Beta||\r",
        );

        let res2 = diff_hl7_messages(
            left,
            right,
            DiffOptions {
                ignore_volatile_ids_and_timestamps: false,
                differences_only: true,
            },
        );

        let evn = res2
            .blocks
            .iter()
            .find(|b| b.segment_tag == "EVN")
            .expect("EVN block");
        assert_eq!(evn.kind, DiffChangeKind::Removed);
        let evn_paths: Vec<&str> = evn.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            evn_paths,
            vec!["EVN-1", "EVN-2", "EVN-4", "EVN-5.1", "EVN-5.2"]
        );
        assert!(evn.rows.iter().all(|r| r.right_text.is_none()));
        assert_eq!(evn.rows.iter().filter(|r| r.is_navigable_diff).count(), 1);
        assert!(evn.rows[0].is_navigable_diff);
        assert!(evn.rows[1..].iter().all(|r| !r.is_navigable_diff));

        let zcs = res2
            .blocks
            .iter()
            .find(|b| b.segment_tag == "ZCS")
            .expect("ZCS block");
        assert_eq!(zcs.kind, DiffChangeKind::Added);
        let zcs_paths: Vec<&str> = zcs.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(zcs_paths, vec!["ZCS-1", "ZCS-2.1", "ZCS-2.2"]);
        assert!(zcs.rows.iter().all(|r| r.left_text.is_none()));
        assert_eq!(zcs.rows.iter().filter(|r| r.is_navigable_diff).count(), 1);
        assert!(zcs.rows[0].is_navigable_diff);

        // Entire EVN + entire ZCS = 2 navigation stops total despite 8 expanded rows.
        assert_eq!(res2.total_diff_rows, 2);

        // 2. Non-standard MSH-1 (`*`) and MSH-2 (`@~\&`) delimiters on an absent segment.
        let left_custom = concat!(
            "MSH*@~\\&*EHR*FAC*LAB*FAC*20261123080000**ADT@A01*M1*P*2.5.1\r",
            "EVN*A01*20260115120000**OP1@JONES**\r",
            "PID*1**MRN1000\r",
        );
        let right_custom = concat!(
            "MSH*@~\\&*EHR*FAC*LAB*FAC*20261123080000**ADT@A01*M1*P*2.5.1\r",
            "PID*1**MRN1000\r",
        );
        let res_custom = diff_hl7_messages(
            left_custom,
            right_custom,
            DiffOptions {
                ignore_volatile_ids_and_timestamps: false,
                differences_only: true,
            },
        );
        let evn_custom = res_custom
            .blocks
            .iter()
            .find(|b| b.segment_tag == "EVN")
            .expect("custom-delim EVN block");
        let custom_paths: Vec<&str> = evn_custom.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(custom_paths, vec!["EVN-1", "EVN-2", "EVN-4.1", "EVN-4.2"]);
        assert_eq!(evn_custom.rows[3].left_text.as_deref(), Some("JONES"));
        assert_eq!(res_custom.total_diff_rows, 1);

        // 3. Multi-message (4 messages: Baseline + 3 targets):
        //    - EVN present only in Baseline -> Removed banner, expanded rows, 1 nav stop.
        //    - ZCS absent in Baseline, present in Targets 2 & 3 -> Added banner, expanded rows, 1 nav stop.
        let base_m = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "EVN|A01|20260115120000||01|\r",
            "PID|1||MRN1000\r",
        );
        let t1 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "PID|1||MRN1000\r",
        );
        let t2 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "PID|1||MRN1000\r",
            "ZCS|1|Beta^Two||\r",
        );
        let t3 = concat!(
            "MSH|^~\\&|EHR|FAC|LAB|FAC|20261123080000||ADT^A01|M1|P|2.5.1\r",
            "PID|1||MRN1000\r",
            "ZCS|1|Gamma^Two\r",
        );

        let res_multi = diff_hl7_message_set(
            base_m,
            &[("Msg #2", t1), ("Msg #3", t2), ("Msg #4", t3)],
            DiffOptions {
                ignore_volatile_ids_and_timestamps: false,
                differences_only: true,
            },
        );

        let m_evn = res_multi
            .blocks
            .iter()
            .find(|b| b.segment_tag == "EVN")
            .expect("multi EVN block");
        assert_eq!(m_evn.kind, DiffChangeKind::Removed);
        let m_evn_paths: Vec<&str> = m_evn.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(m_evn_paths, vec!["EVN-1", "EVN-2", "EVN-4"]);
        assert_eq!(m_evn.rows.iter().filter(|r| r.is_navigable_diff).count(), 1);
        assert!(m_evn.rows[0].is_navigable_diff);
        assert!(m_evn.rows[0]
            .cells
            .iter()
            .all(|c| c.kind == DiffChangeKind::Removed && c.text.is_none()));

        let m_zcs = res_multi
            .blocks
            .iter()
            .find(|b| b.segment_tag == "ZCS")
            .expect("multi ZCS block");
        assert_eq!(m_zcs.kind, DiffChangeKind::Added);
        let m_zcs_paths: Vec<&str> = m_zcs.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(m_zcs_paths, vec!["ZCS-1", "ZCS-2.1", "ZCS-2.2"]);
        assert!(m_zcs.rows.iter().all(|r| r.baseline_text.is_none()));
        assert_eq!(m_zcs.rows.iter().filter(|r| r.is_navigable_diff).count(), 1);
        assert!(m_zcs.rows[0].is_navigable_diff);
        // Target 1 omitted ZCS (Unchanged/None), Targets 2 & 3 added it with distinct values on ZCS-2.1
        let zcs_2_1 = m_zcs
            .rows
            .iter()
            .find(|r| r.path == "ZCS-2.1")
            .expect("ZCS-2.1");
        assert_eq!(zcs_2_1.cells[0].text, None);
        assert_eq!(zcs_2_1.cells[1].text.as_deref(), Some("Beta"));
        assert_eq!(zcs_2_1.cells[2].text.as_deref(), Some("Gamma"));

        // 1 nav stop for EVN + 1 nav stop for ZCS = 2 total navigation stops.
        assert_eq!(res_multi.total_diff_rows, 2);
    }
}
