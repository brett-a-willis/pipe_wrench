//! Non-destructive record filtering and jump-to-next / HL7-aware grep search across HL7 records
//! (`§5.1`, `§14`, `D-26`).
//!
//! Supports:
//! - Plain-text substring or regular-expression predicates (`--regex`, `--ignore-case`, `--invert-match` / `-v`)
//! - Canonical HL7 v2 field-path predicates (`SEG-F[(R)][.C[.S]]` with `=`, `!=`, `~=`, `contains`, `>`, `>=`, `<`, `<=`)
//! - Optional field-path restriction (`--field <PATH>`) on search
//! - Hit-count breakdown grouped by canonical field path (`FieldHitCount`)

use std::cmp::Ordering;
use std::collections::BTreeMap;

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::address::{parse_filter_field_path, FieldAddress};
use crate::delimiters::Delimiters;
use crate::record::{FieldDisplayOptions, Record};
use crate::timestamp::Hl7Timestamp;

/// Comparison operator for a field-path filter predicate (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldFilterOp {
    /// Equality match (`PATH=VALUE`).
    Equals,
    /// Inequality match (`PATH!=VALUE`).
    NotEquals,
    /// Substring or regex match (`PATH contains VALUE` or `PATH~=VALUE`).
    Contains,
    /// Regular expression match (`PATH=~PATTERN`).
    Regex,
    /// Presence check (`PATH exists`): at least one addressed segment/repetition/component is present and non-empty.
    Exists,
    /// Strictly greater than (`PATH>VALUE`, numeric/timestamp/lexicographic).
    GreaterThan,
    /// Greater than or equal (`PATH>=VALUE`, numeric/timestamp/lexicographic).
    GreaterOrEqual,
    /// Strictly less than (`PATH<VALUE`, numeric/timestamp/lexicographic).
    LessThan,
    /// Less than or equal (`PATH<=VALUE`, numeric/timestamp/lexicographic).
    LessOrEqual,
}

/// Options controlling filter and search parsing/matching (`--ignore-case`, `--regex`, `--invert-match`) (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FilterMatchOptions {
    /// Case-sensitive comparison (default `false` in GUI; configurable via `--ignore-case` in CLI).
    pub case_sensitive: bool,
    /// Interpret text query or `Contains`/`Equals` right-hand side as a regular expression (`--regex`).
    pub use_regex: bool,
    /// Invert the final record match result (`--invert-match` / `-v`).
    pub invert_match: bool,
}

/// Non-destructive filter predicate evaluated against a [`Record`] or raw segment lines (`D-26`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordFilter {
    /// Plain-text substring or regex across the entire record.
    TextQuery {
        /// Needle or regex pattern.
        pattern: String,
        /// Case sensitivity.
        case_sensitive: bool,
        /// Regex toggle.
        use_regex: bool,
    },
    /// Presence check against the entire message (`* exists`): matches any non-empty message.
    WholeMessageExists,
    /// Field-path comparison against any occurrence of `address.segment` in the record.
    FieldPredicate {
        /// Canonical HL7 v2 field address (`SEG-F[(R)][.C[.S]]`).
        address: FieldAddress,
        /// When `true` (e.g. `PID-3(*).1`), evaluate across all `~`-separated repetitions of the field.
        #[serde(default)]
        wildcard_repetition: bool,
        /// Comparison operator (`=`, `!=`, `~=`, `contains`, `exists`, `>`, `>=`, `<`, `<=`).
        op: FieldFilterOp,
        /// Target comparison value or regex pattern (unused for `Exists`).
        value: String,
        /// Case sensitivity.
        case_sensitive: bool,
        /// Regex toggle.
        use_regex: bool,
    },
    /// Logical negation (`NOT`, `--invert-match` / `-v`).
    Not(Box<RecordFilter>),
    /// Logical conjunction (`AND`) of multiple filters.
    All(Vec<RecordFilter>),
}

impl RecordFilter {
    /// Parse a user-entered filter expression with default options (case-insensitive, literal) (`D-26`).
    pub fn parse(input: &str) -> Result<Self, String> {
        Self::parse_with_options(input, FilterMatchOptions::default())
    }

    /// Parse a user-entered filter expression honoring [`FilterMatchOptions`] (`D-26`):
    /// - `MSH-9.1=ADT`, `MSH-9.1!=ORU`, `PID-5.1~=DOE`, `PID-5.1 contains DOE`
    /// - `OBX-5>200`, `OBX-5>=100`, `OBX-5<50`, `MSH-7>=20260115`
    /// - Otherwise falls back to `TextQuery`.
    pub fn parse_with_options(input: &str, opts: FilterMatchOptions) -> Result<Self, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("Filter expression cannot be empty.".to_string());
        }

        if let Some(pred) = try_parse_field_operator(trimmed, opts)? {
            return Ok(Self::apply_invert(pred, opts.invert_match));
        }

        if opts.use_regex {
            validate_regex(trimmed, opts.case_sensitive)?;
        }

        let base = Self::TextQuery {
            pattern: trimmed.to_string(),
            case_sensitive: opts.case_sensitive,
            use_regex: opts.use_regex,
        };
        Ok(Self::apply_invert(base, opts.invert_match))
    }

    /// Strict parser for CLI `--filter-field <EXPR>` that requires a valid [`FieldAddress`] on the left-hand side.
    pub fn parse_field_predicate(input: &str) -> Result<Self, String> {
        Self::parse_field_predicate_with_options(input, FilterMatchOptions::default())
    }

    /// Strict parser for CLI `--filter-field <EXPR>` honoring [`FilterMatchOptions`].
    pub fn parse_field_predicate_with_options(
        input: &str,
        opts: FilterMatchOptions,
    ) -> Result<Self, String> {
        let trimmed = input.trim();
        if let Some(pred) = try_parse_field_operator_strict(trimmed, opts)? {
            return Ok(Self::apply_invert(pred, opts.invert_match));
        }
        Err(format!(
            "Invalid --filter-field '{trimmed}': expected <PATH><OP><VALUE> or <PATH> exists where OP is =, !=, ~=, >, >=, <, <=, or 'contains'."
        ))
    }

    /// Build a combined [`RecordFilter`] from optional plain-text `--filter` and repeatable `--filter-field` expressions
    /// with default options.
    pub fn from_cli_specs(
        text_filter: Option<&str>,
        field_filters: &[String],
    ) -> Result<Option<Self>, String> {
        Self::from_cli_specs_with_options(text_filter, field_filters, FilterMatchOptions::default())
    }

    /// Build a combined [`RecordFilter`] from optional plain-text `--filter` and repeatable `--filter-field` expressions
    /// honoring [`FilterMatchOptions`].
    pub fn from_cli_specs_with_options(
        text_filter: Option<&str>,
        field_filters: &[String],
        opts: FilterMatchOptions,
    ) -> Result<Option<Self>, String> {
        let inner_opts = FilterMatchOptions {
            invert_match: false,
            ..opts
        };
        let mut preds = Vec::new();
        if let Some(txt) = text_filter {
            let trimmed = txt.trim();
            if !trimmed.is_empty() {
                preds.push(Self::parse_with_options(trimmed, inner_opts)?);
            }
        }
        for expr in field_filters {
            let trimmed = expr.trim();
            if !trimmed.is_empty() {
                preds.push(Self::parse_field_predicate_with_options(
                    trimmed, inner_opts,
                )?);
            }
        }
        let combined = match preds.len() {
            0 => return Ok(None),
            1 => preds.into_iter().next(),
            _ => Some(Self::All(preds)),
        };
        Ok(combined.map(|f| Self::apply_invert(f, opts.invert_match)))
    }

    fn apply_invert(filter: Self, invert: bool) -> Self {
        if invert {
            Self::Not(Box::new(filter))
        } else {
            filter
        }
    }

    /// Evaluate whether `record` matches this filter predicate (`D-26`).
    pub fn matches_record(&self, record: &Record) -> bool {
        match self {
            Self::TextQuery {
                pattern,
                case_sensitive,
                use_regex,
            } => {
                if pattern.is_empty() {
                    return true;
                }
                let decoded = record.charset().decode_display(record.bytes());
                text_matches(&decoded, pattern, *case_sensitive, *use_regex)
            }
            Self::WholeMessageExists => !record.bytes().is_empty(),
            Self::FieldPredicate {
                address,
                wildcard_repetition,
                op,
                value,
                case_sensitive,
                use_regex,
            } => {
                let charset = record.charset();
                let seg_indices: Vec<usize> = record
                    .segments()
                    .find_by_tag(&address.segment)
                    .map(|(i, _)| i)
                    .collect();
                if seg_indices.is_empty() {
                    return evaluate_field_op("", *op, value, *case_sensitive, *use_regex);
                }
                match op {
                    FieldFilterOp::NotEquals if !value.is_empty() => {
                        seg_indices.into_iter().all(|seg_idx| {
                            let candidates = extract_decoded_field_candidates(
                                record,
                                seg_idx,
                                address,
                                *wildcard_repetition,
                                &charset,
                            );
                            candidates.into_iter().all(|(_, actual)| {
                                evaluate_field_op(&actual, *op, value, *case_sensitive, *use_regex)
                            })
                        })
                    }
                    _ => seg_indices.into_iter().any(|seg_idx| {
                        let candidates = extract_decoded_field_candidates(
                            record,
                            seg_idx,
                            address,
                            *wildcard_repetition,
                            &charset,
                        );
                        candidates.into_iter().any(|(_, actual)| {
                            evaluate_field_op(&actual, *op, value, *case_sensitive, *use_regex)
                        })
                    }),
                }
            }
            Self::Not(inner) => !inner.matches_record(record),
            Self::All(list) => list.iter().all(|f| f.matches_record(record)),
        }
    }

    /// Evaluate whether a streaming record represented by raw segment byte lines matches this filter
    /// (used by `hl7_core::scan` so directory scanning remains streaming without building full `Record`s).
    pub fn matches_raw_segments(&self, delimiters: Delimiters, segments: &[Vec<u8>]) -> bool {
        if segments.is_empty() {
            return false;
        }
        let joined = segments.join(&b'\r');
        let kind = crate::scanner::detect_record_start(&segments[0], delimiters)
            .map_or(crate::record::RecordKind::Msh, |(k, _)| k);
        let temp_rec = Record::from_normalized_bytes(
            joined,
            kind,
            delimiters,
            crate::record::SourceTerminator::Cr,
            crate::record::RecordFlags::default(),
        );
        self.matches_record(&temp_rec)
    }
}

/// Options for Find (`Ctrl+F`) and `pipe_wrench_cli search` / `find` (`D-26`, `D-27`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQueryOptions {
    /// Text query, regex pattern, or field predicate (`OBX-5>200`).
    pub query: String,
    /// Optional field-path restriction (e.g. `Some(OBX-5)` or `Some(PID-3.1.4)`).
    pub field_restriction: Option<FieldAddress>,
    /// Case-sensitive search toggle (`Aa`).
    pub case_sensitive: bool,
    /// Regex search toggle (`.*`).
    pub use_regex: bool,
    /// Match whole field value (when `field_restriction` is set) or whole word (when unrestricted) (`D-27`).
    pub match_whole: bool,
}

/// Options for HL7-safe Find & Replace (`Ctrl+H`) and `pipe_wrench_cli replace` (`D-27`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hl7ReplaceOptions {
    /// Text or regex pattern to find. May be empty when `field_restriction` is `Some(...)`
    /// to replace/set the entire value of the targeted field, component, or subcomponent.
    pub find: String,
    /// Replacement string (supports regex capture groups `$1`, `$2`, `${name}` when `use_regex` is `true`).
    pub replace: String,
    /// Optional canonical HL7 v2 target path (`SEG-F[(R)][.C[.S]]`, e.g. `PID-3.1.4`).
    pub field_restriction: Option<FieldAddress>,
    /// Case-sensitive matching (`Aa`).
    pub case_sensitive: bool,
    /// Regular expression mode (`.*`).
    pub use_regex: bool,
    /// Match whole value (when `field_restriction` is set) or whole word (when unrestricted).
    pub match_whole: bool,
}

/// Summary returned by batch HL7-aware replace operations (`D-27`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaceSummary {
    /// Total number of occurrences replaced.
    pub occurrences: usize,
    /// Number of records modified.
    pub records_modified: usize,
}

/// One matching location returned by Jump-to-Next Search (`F3` / `Shift+F3`) or `pipe_wrench_cli search` (`D-26`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    /// 0-based record index in the document.
    pub record_index: usize,
    /// 0-based segment index within `record_index`.
    pub segment_index: usize,
    /// Specific field address if the match occurred inside a parsed field.
    pub field_address: Option<FieldAddress>,
    /// Matched field or segment display value.
    pub matched_value: String,
}

/// Hit count for a single canonical field path across a search result set (`D-26`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldHitCount {
    /// Canonical HL7 v2 field path (e.g. `"OBX-5"`), or segment tag if unaddressed.
    pub field_path: String,
    /// Number of search hits at this field path.
    pub count: usize,
}

/// Collect all `(record_index, segment_index, Option<FieldAddress>)` matches for `query` across `records`
/// using default case-insensitive literal options (`D-26`).
pub fn find_all_search_matches(records: &[Record], query: &str) -> Vec<SearchMatch> {
    let opts = SearchQueryOptions {
        query: query.to_string(),
        field_restriction: None,
        case_sensitive: false,
        use_regex: false,
        match_whole: false,
    };
    find_search_matches_with_options(records, &opts).unwrap_or_default()
}

/// Collect all search matches across `records` honoring [`SearchQueryOptions`] (field restriction,
/// case sensitivity, regex, and field comparison operators `=`, `!=`, `~=`, `>`, `>=`, `<`, `<=`) (`D-26`).
pub fn find_search_matches_with_options(
    records: &[Record],
    opts: &SearchQueryOptions,
) -> Result<Vec<SearchMatch>, String> {
    let trimmed = opts.query.trim();
    if trimmed.is_empty() && opts.field_restriction.is_none() {
        return Ok(Vec::new());
    }

    let filter_opts = FilterMatchOptions {
        case_sensitive: opts.case_sensitive,
        use_regex: opts.use_regex,
        invert_match: false,
    };

    if opts.use_regex && !trimmed.is_empty() {
        // Validate regex early if query is not a field comparison expression
        if try_parse_field_operator(trimmed, filter_opts)?.is_none() {
            validate_regex(trimmed, opts.case_sensitive)?;
        }
    }

    let parsed_field_pred = if !trimmed.is_empty() {
        try_parse_field_operator(trimmed, filter_opts)?
    } else {
        None
    };

    let mut matches = Vec::new();
    let display_opts = FieldDisplayOptions {
        display_empty_fields: false,
        display_compound_fields: false,
    };

    for (rec_idx, rec) in records.iter().enumerate() {
        let charset = rec.charset();
        let seg_count = rec.segments().len();

        // Case 1: The query itself is a field-operator predicate like `OBX-5>200`, `PID-3(*).1=123`, `PID-5.1 exists`, or `PID-3=""`
        if let Some(RecordFilter::FieldPredicate {
            ref address,
            wildcard_repetition,
            op,
            ref value,
            case_sensitive,
            use_regex,
        }) = parsed_field_pred
        {
            if let Some(ref restricted) = opts.field_restriction {
                if !addresses_compatible(restricted, address) {
                    continue;
                }
            }
            let is_empty_check = op == FieldFilterOp::Equals && !use_regex && value.is_empty();
            let mut saw_segment = false;
            for seg_idx in 0..seg_count {
                let Some(seg_bytes) = rec.segment_bytes(seg_idx) else {
                    continue;
                };
                if seg_bytes.len() < 3 || seg_bytes[0..3] != address.segment {
                    continue;
                }
                saw_segment = true;
                for (cand_addr, actual) in extract_decoded_field_candidates(
                    rec,
                    seg_idx,
                    address,
                    wildcard_repetition,
                    &charset,
                ) {
                    if actual.is_empty() && !is_empty_check && op != FieldFilterOp::NotEquals {
                        continue;
                    }
                    let hit = evaluate_field_op(&actual, op, value, case_sensitive, use_regex);
                    if hit {
                        matches.push(SearchMatch {
                            record_index: rec_idx,
                            segment_index: seg_idx,
                            field_address: Some(cand_addr),
                            matched_value: actual,
                        });
                    }
                }
            }
            if !saw_segment && is_empty_check {
                matches.push(SearchMatch {
                    record_index: rec_idx,
                    segment_index: 0,
                    field_address: Some(*address),
                    matched_value: String::new(),
                });
            }
            continue;
        }

        // Case 2: Field restriction is set (e.g. `--field PID-3.4`), optionally combined with text/regex query.
        // When `query` is empty, locates messages where the exact target field/component/subcomponent is empty/missing.
        if let Some(ref target_addr) = opts.field_restriction {
            let mut saw_segment = false;
            for seg_idx in 0..seg_count {
                let Some(seg_bytes) = rec.segment_bytes(seg_idx) else {
                    continue;
                };
                if seg_bytes.len() < 3 || seg_bytes[0..3] != target_addr.segment {
                    continue;
                }
                saw_segment = true;
                let actual = extract_decoded_field(rec, seg_idx, target_addr, &charset);
                let hit = if trimmed.is_empty() {
                    is_empty_or_hl7_null(&actual)
                } else {
                    !actual.is_empty()
                        && text_matches_with_whole(
                            &actual,
                            trimmed,
                            opts.case_sensitive,
                            opts.use_regex,
                            opts.match_whole,
                            true,
                        )
                };
                if hit {
                    matches.push(SearchMatch {
                        record_index: rec_idx,
                        segment_index: seg_idx,
                        field_address: Some(*target_addr),
                        matched_value: actual,
                    });
                }
            }
            if !saw_segment && trimmed.is_empty() {
                matches.push(SearchMatch {
                    record_index: rec_idx,
                    segment_index: 0,
                    field_address: Some(*target_addr),
                    matched_value: String::new(),
                });
            }
            continue;
        }

        // Case 3: General text/regex search across all segments and fields
        for seg_idx in 0..seg_count {
            let Some(seg_bytes) = rec.segment_bytes(seg_idx) else {
                continue;
            };
            let seg_text = charset.decode_display(seg_bytes);
            if !text_matches_with_whole(
                &seg_text,
                trimmed,
                opts.case_sensitive,
                opts.use_regex,
                opts.match_whole,
                false,
            ) {
                continue;
            }
            let mut found_field_in_seg = false;
            for entry in rec.field_entries(seg_idx, display_opts) {
                let val_str = charset.decode_display(&entry.raw_value).into_owned();
                let addr_str = entry.address.map(|a| a.to_string()).unwrap_or_default();
                if text_matches_with_whole(
                    &val_str,
                    trimmed,
                    opts.case_sensitive,
                    opts.use_regex,
                    opts.match_whole,
                    false,
                ) || text_matches_with_whole(
                    &addr_str,
                    trimmed,
                    opts.case_sensitive,
                    opts.use_regex,
                    opts.match_whole,
                    false,
                ) {
                    matches.push(SearchMatch {
                        record_index: rec_idx,
                        segment_index: seg_idx,
                        field_address: entry.address,
                        matched_value: val_str,
                    });
                    found_field_in_seg = true;
                }
            }
            if !found_field_in_seg {
                matches.push(SearchMatch {
                    record_index: rec_idx,
                    segment_index: seg_idx,
                    field_address: None,
                    matched_value: seg_text.into_owned(),
                });
            }
        }
    }

    Ok(matches)
}

/// Group search matches by canonical field path, sorted by descending hit count then field path (`D-26`).
pub fn summarize_matches_by_field(matches: &[SearchMatch]) -> Vec<FieldHitCount> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in matches {
        let key = m
            .field_address
            .map_or_else(|| "(segment)".to_string(), |a| a.to_string());
        *counts.entry(key).or_insert(0) += 1;
    }
    let mut list: Vec<FieldHitCount> = counts
        .into_iter()
        .map(|(field_path, count)| FieldHitCount { field_path, count })
        .collect();
    list.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.field_path.cmp(&b.field_path))
    });
    list
}

fn addresses_compatible(a: &FieldAddress, b: &FieldAddress) -> bool {
    a.segment == b.segment
        && a.field == b.field
        && (a.component.is_none() || b.component.is_none() || a.component == b.component)
        && (a.subcomponent.is_none()
            || b.subcomponent.is_none()
            || a.subcomponent == b.subcomponent)
}

fn extract_decoded_field(
    record: &Record,
    seg_idx: usize,
    address: &FieldAddress,
    charset: &crate::encoding::ResolvedCharset,
) -> String {
    record
        .field_bytes(seg_idx, address)
        .map(|b| charset.decode_display(b).into_owned())
        .unwrap_or_default()
}

/// Extract one or more `(concrete_address, decoded_value)` pairs for `address` in `seg_idx`.
/// When `wildcard_repetition == true`, splits the addressed field on `record.delimiters().repetition`
/// and resolves the component/subcomponent for every repetition (`1..=N`).
fn extract_decoded_field_candidates(
    record: &Record,
    seg_idx: usize,
    address: &FieldAddress,
    wildcard_repetition: bool,
    charset: &crate::encoding::ResolvedCharset,
) -> Vec<(FieldAddress, String)> {
    if !wildcard_repetition || address.is_header_delimiter_field() {
        return vec![(
            *address,
            extract_decoded_field(record, seg_idx, address, charset),
        )];
    }

    let raw_field_addr = FieldAddress {
        segment: address.segment,
        field: address.field,
        repetition: None,
        component: None,
        subcomponent: None,
    };
    let Some(raw_field) = record.field_bytes(seg_idx, &raw_field_addr) else {
        return vec![(*address, String::new())];
    };

    let delims = record.delimiters();
    let mut out = Vec::new();
    for (rep_idx, rep_slice) in raw_field.split(|&b| b == delims.repetition).enumerate() {
        let rep_num = u16::try_from(rep_idx + 1)
            .ok()
            .and_then(std::num::NonZeroU16::new);
        let concrete_addr = FieldAddress {
            segment: address.segment,
            field: address.field,
            repetition: rep_num,
            component: address.component,
            subcomponent: address.subcomponent,
        };
        let comp_slice = match address.component {
            Some(c) => rep_slice
                .split(|&b| b == delims.component)
                .nth(usize::from(c.get() - 1))
                .unwrap_or(b""),
            None => rep_slice,
        };
        let leaf_slice = match address.subcomponent {
            Some(s) => comp_slice
                .split(|&b| b == delims.subcomponent)
                .nth(usize::from(s.get() - 1))
                .unwrap_or(b""),
            None => comp_slice,
        };
        out.push((
            concrete_addr,
            charset.decode_display(leaf_slice).into_owned(),
        ));
    }
    if out.is_empty() {
        out.push((*address, String::new()));
    }
    out
}

fn try_parse_whole_message_wildcard(
    trimmed: &str,
    opts: FilterMatchOptions,
) -> Result<Option<RecordFilter>, String> {
    for (op_token, op) in [
        ("!=", FieldFilterOp::NotEquals),
        ("~=", FieldFilterOp::Contains),
        ("=~", FieldFilterOp::Regex),
        ("=", FieldFilterOp::Equals),
    ] {
        if let Some((lhs, rhs)) = trimmed.split_once(op_token) {
            if lhs.trim() == "*" {
                let value = unquote(rhs.trim());
                let effective_regex = opts.use_regex || op == FieldFilterOp::Regex;
                if effective_regex {
                    validate_regex(&value, opts.case_sensitive)?;
                }
                let base = RecordFilter::TextQuery {
                    pattern: value,
                    case_sensitive: opts.case_sensitive,
                    use_regex: effective_regex,
                };
                return Ok(Some(if op == FieldFilterOp::NotEquals {
                    RecordFilter::Not(Box::new(base))
                } else {
                    base
                }));
            }
        }
    }

    let tokens: Vec<&str> = trimmed.splitn(3, char::is_whitespace).collect();
    if tokens.first().copied() == Some("*") {
        if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("exists") {
            return Ok(Some(RecordFilter::WholeMessageExists));
        }
        if tokens.len() >= 3 && tokens[1].eq_ignore_ascii_case("contains") {
            let rest = trimmed[tokens[0].len()..].trim_start();
            let value = unquote(rest["contains".len()..].trim());
            if opts.use_regex {
                validate_regex(&value, opts.case_sensitive)?;
            }
            return Ok(Some(RecordFilter::TextQuery {
                pattern: value,
                case_sensitive: opts.case_sensitive,
                use_regex: opts.use_regex,
            }));
        }
    }
    Ok(None)
}

fn try_parse_field_operator(
    trimmed: &str,
    opts: FilterMatchOptions,
) -> Result<Option<RecordFilter>, String> {
    if let Some(wild) = try_parse_whole_message_wildcard(trimmed, opts)? {
        return Ok(Some(wild));
    }
    for (op_token, op) in [
        ("!=", FieldFilterOp::NotEquals),
        ("~=", FieldFilterOp::Contains),
        ("=~", FieldFilterOp::Regex),
        (">=", FieldFilterOp::GreaterOrEqual),
        ("<=", FieldFilterOp::LessOrEqual),
        ("=", FieldFilterOp::Equals),
        (">", FieldFilterOp::GreaterThan),
        ("<", FieldFilterOp::LessThan),
    ] {
        if let Some((lhs, rhs)) = trimmed.split_once(op_token) {
            if let Ok((address, wildcard_repetition)) = parse_filter_field_path(lhs.trim()) {
                let value = unquote(rhs.trim());
                let effective_regex = opts.use_regex || op == FieldFilterOp::Regex;
                if effective_regex
                    && matches!(
                        op,
                        FieldFilterOp::Equals
                            | FieldFilterOp::NotEquals
                            | FieldFilterOp::Contains
                            | FieldFilterOp::Regex
                    )
                {
                    validate_regex(&value, opts.case_sensitive)?;
                }
                return Ok(Some(RecordFilter::FieldPredicate {
                    address,
                    wildcard_repetition,
                    op,
                    value,
                    case_sensitive: opts.case_sensitive,
                    use_regex: effective_regex,
                }));
            }
        }
    }

    let tokens: Vec<&str> = trimmed.splitn(3, char::is_whitespace).collect();
    if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("exists") {
        if let Ok((address, wildcard_repetition)) = parse_filter_field_path(tokens[0].trim()) {
            return Ok(Some(RecordFilter::FieldPredicate {
                address,
                wildcard_repetition,
                op: FieldFilterOp::Exists,
                value: String::new(),
                case_sensitive: opts.case_sensitive,
                use_regex: false,
            }));
        }
    }
    if tokens.len() >= 3 && tokens[1].eq_ignore_ascii_case("contains") {
        if let Ok((address, wildcard_repetition)) = parse_filter_field_path(tokens[0].trim()) {
            let rest = trimmed[tokens[0].len()..].trim_start();
            let value = unquote(rest["contains".len()..].trim());
            if opts.use_regex {
                validate_regex(&value, opts.case_sensitive)?;
            }
            return Ok(Some(RecordFilter::FieldPredicate {
                address,
                wildcard_repetition,
                op: FieldFilterOp::Contains,
                value,
                case_sensitive: opts.case_sensitive,
                use_regex: opts.use_regex,
            }));
        }
    }

    Ok(None)
}

fn try_parse_field_operator_strict(
    trimmed: &str,
    opts: FilterMatchOptions,
) -> Result<Option<RecordFilter>, String> {
    for (op_token, op) in [
        ("!=", FieldFilterOp::NotEquals),
        ("~=", FieldFilterOp::Contains),
        ("=~", FieldFilterOp::Regex),
        (">=", FieldFilterOp::GreaterOrEqual),
        ("<=", FieldFilterOp::LessOrEqual),
        ("=", FieldFilterOp::Equals),
        (">", FieldFilterOp::GreaterThan),
        ("<", FieldFilterOp::LessThan),
    ] {
        if let Some((lhs, rhs)) = trimmed.split_once(op_token) {
            let (address, wildcard_repetition) = parse_filter_field_path(lhs.trim())
                .map_err(|e| format!("Invalid field address in '{trimmed}': {e}"))?;
            let value = unquote(rhs.trim());
            let effective_regex = opts.use_regex || op == FieldFilterOp::Regex;
            if effective_regex
                && matches!(
                    op,
                    FieldFilterOp::Equals
                        | FieldFilterOp::NotEquals
                        | FieldFilterOp::Contains
                        | FieldFilterOp::Regex
                )
            {
                validate_regex(&value, opts.case_sensitive)?;
            }
            return Ok(Some(RecordFilter::FieldPredicate {
                address,
                wildcard_repetition,
                op,
                value,
                case_sensitive: opts.case_sensitive,
                use_regex: effective_regex,
            }));
        }
    }

    let tokens: Vec<&str> = trimmed.splitn(3, char::is_whitespace).collect();
    if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("exists") {
        let (address, wildcard_repetition) = parse_filter_field_path(tokens[0].trim())
            .map_err(|e| format!("Invalid field address in '{trimmed}': {e}"))?;
        return Ok(Some(RecordFilter::FieldPredicate {
            address,
            wildcard_repetition,
            op: FieldFilterOp::Exists,
            value: String::new(),
            case_sensitive: opts.case_sensitive,
            use_regex: false,
        }));
    }
    if tokens.len() >= 3 && tokens[1].eq_ignore_ascii_case("contains") {
        let (address, wildcard_repetition) = parse_filter_field_path(tokens[0].trim())
            .map_err(|e| format!("Invalid field address in '{trimmed}': {e}"))?;
        let rest = trimmed[tokens[0].len()..].trim_start();
        let value = unquote(rest["contains".len()..].trim());
        if opts.use_regex {
            validate_regex(&value, opts.case_sensitive)?;
        }
        return Ok(Some(RecordFilter::FieldPredicate {
            address,
            wildcard_repetition,
            op: FieldFilterOp::Contains,
            value,
            case_sensitive: opts.case_sensitive,
            use_regex: opts.use_regex,
        }));
    }

    Ok(None)
}

fn validate_regex(pattern: &str, case_sensitive: bool) -> Result<(), String> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map(|_| ())
        .map_err(|e| format!("Invalid regular expression '{pattern}': {e}"))
}

fn text_matches(haystack: &str, pattern: &str, case_sensitive: bool, use_regex: bool) -> bool {
    text_matches_with_whole(haystack, pattern, case_sensitive, use_regex, false, false)
}

fn text_matches_with_whole(
    haystack: &str,
    pattern: &str,
    case_sensitive: bool,
    use_regex: bool,
    match_whole: bool,
    whole_value_mode: bool,
) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if use_regex {
        let eff = if match_whole {
            if whole_value_mode {
                format!("^(?:{pattern})$")
            } else {
                format!(r"\b(?:{pattern})\b")
            }
        } else {
            pattern.to_string()
        };
        RegexBuilder::new(&eff)
            .case_insensitive(!case_sensitive)
            .build()
            .is_ok_and(|re| re.is_match(haystack))
    } else if match_whole {
        if whole_value_mode {
            if case_sensitive {
                haystack == pattern
            } else {
                haystack.eq_ignore_ascii_case(pattern)
            }
        } else {
            find_literal_spans(haystack, pattern, case_sensitive, true)
                .next()
                .is_some()
        }
    } else if case_sensitive {
        haystack.contains(pattern)
    } else {
        contains_ignore_ascii_case(haystack, pattern)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn find_literal_spans<'a>(
    haystack: &'a str,
    needle: &'a str,
    case_sensitive: bool,
    whole_word: bool,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut spans = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return spans.into_iter();
    }

    let (hay_cmp, ned_cmp);
    let (h_str, n_str) = if case_sensitive {
        (haystack, needle)
    } else {
        hay_cmp = haystack.to_ascii_lowercase();
        ned_cmp = needle.to_ascii_lowercase();
        (hay_cmp.as_str(), ned_cmp.as_str())
    };

    let mut pos = 0usize;
    while pos <= h_str.len().saturating_sub(n_str.len()) {
        if let Some(rel) = h_str[pos..].find(n_str) {
            let start = pos + rel;
            let end = start + n_str.len();
            let boundary_ok = if whole_word {
                let prev_ok = haystack[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !is_word_char(c));
                let next_ok = haystack[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_word_char(c));
                prev_ok && next_ok
            } else {
                true
            };
            if boundary_ok {
                spans.push((start, end));
                pos = end;
            } else {
                let step = h_str[start..].chars().next().map_or(1, char::len_utf8);
                pos = start + step;
            }
        } else {
            break;
        }
    }
    spans.into_iter()
}

/// Perform string or regex replacement inside `haystack` honoring [`Hl7ReplaceOptions`] (`D-27`).
///
/// When `whole_value_mode` is `true` (a specific [`FieldAddress`] is targeted):
/// - An empty `opts.find` replaces the entire value with `opts.replace` (if different).
/// - `opts.match_whole` matches the entire field/component/subcomponent value.
pub fn replace_in_string(
    haystack: &str,
    opts: &Hl7ReplaceOptions,
    whole_value_mode: bool,
) -> Result<(String, usize), String> {
    if opts.find.is_empty() {
        if whole_value_mode {
            if opts.match_whole && !is_empty_or_hl7_null(haystack) {
                return Ok((haystack.to_string(), 0));
            }
            if haystack == opts.replace {
                return Ok((haystack.to_string(), 0));
            }
            return Ok((opts.replace.clone(), 1));
        }
        return Err(
            "Find text cannot be empty when no target field path is specified.".to_string(),
        );
    }

    if opts.use_regex {
        let eff = if opts.match_whole {
            if whole_value_mode {
                format!("^(?:{})$", opts.find)
            } else {
                format!(r"\b(?:{})\b", opts.find)
            }
        } else {
            opts.find.clone()
        };
        let re = RegexBuilder::new(&eff)
            .case_insensitive(!opts.case_sensitive)
            .build()
            .map_err(|e| format!("Invalid regular expression '{}': {e}", opts.find))?;
        let count = re.find_iter(haystack).count();
        if count == 0 {
            return Ok((haystack.to_string(), 0));
        }
        let replaced = re.replace_all(haystack, opts.replace.as_str()).into_owned();
        return Ok((replaced, count));
    }

    if opts.match_whole && whole_value_mode {
        let eq = if opts.case_sensitive {
            haystack == opts.find
        } else {
            haystack.eq_ignore_ascii_case(&opts.find)
        };
        if eq {
            return Ok((opts.replace.clone(), 1));
        }
        return Ok((haystack.to_string(), 0));
    }

    let spans: Vec<(usize, usize)> =
        find_literal_spans(haystack, &opts.find, opts.case_sensitive, opts.match_whole).collect();
    if spans.is_empty() {
        return Ok((haystack.to_string(), 0));
    }

    let mut out = String::with_capacity(haystack.len());
    let mut last = 0usize;
    for (start, end) in &spans {
        out.push_str(&haystack[last..*start]);
        out.push_str(&opts.replace);
        last = *end;
    }
    out.push_str(&haystack[last..]);
    Ok((out, spans.len()))
}

/// Replace matching text inside a single addressed field, repetition, component, or subcomponent
/// of `rec` at `seg_index`, returning the updated CR-joined record bytes and occurrence count (`D-27`).
///
/// Preserves sibling components, subcomponents, repetitions, active delimiters, and `MSH-1`/`MSH-2`.
pub fn replace_in_single_field(
    rec: &Record,
    seg_index: usize,
    address: &FieldAddress,
    opts: &Hl7ReplaceOptions,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if address.is_header_delimiter_field() {
        return Err(format!(
            "Cannot replace inside header delimiter field {address}."
        ));
    }
    let Some(old_seg) = rec.segment_bytes(seg_index) else {
        return Ok(None);
    };
    if old_seg.len() < 3 || old_seg[0..3] != address.segment {
        return Ok(None);
    }

    let charset = rec.charset();
    let delims = rec.delimiters();
    let current_val = extract_decoded_field(rec, seg_index, address, &charset);
    let (replaced_val, count) = replace_in_string(&current_val, opts, true)?;
    if count == 0 {
        return Ok(None);
    }

    let new_seg =
        crate::edit::rewrite_segment_field(old_seg, delims, address, replaced_val.as_bytes())
            .map_err(|e| e.to_string())?;

    let seg_len = rec.segments().len();
    let mut new_bytes = Vec::with_capacity(rec.bytes().len() + new_seg.len());
    for i in 0..seg_len {
        if i > 0 {
            new_bytes.push(b'\r');
        }
        if i == seg_index {
            new_bytes.extend_from_slice(&new_seg);
        } else if let Some(existing) = rec.segment_bytes(i) {
            new_bytes.extend_from_slice(existing);
        }
    }
    Ok(Some((new_bytes, count)))
}

/// Replace all matching occurrences inside `rec` honoring [`Hl7ReplaceOptions`] (`D-27`).
///
/// - When `opts.field_restriction` is `Some(addr)`, modifies **only** that exact field, repetition,
///   component, or subcomponent across matching segments in `rec`.
/// - When `opts.field_restriction` is `None`, replaces across all editable leaf tokens of every
///   segment while preserving segment tags, active delimiters, and `MSH-1`/`MSH-2` structural integrity.
///
/// Returns `Ok(Some((new_cr_bytes, occurrence_count)))` when at least one replacement occurred,
/// or `Ok(None)` when nothing changed.
pub fn replace_in_record(
    rec: &Record,
    opts: &Hl7ReplaceOptions,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if opts.find.is_empty() && opts.field_restriction.is_none() {
        return Err(
            "Find text cannot be empty when no target field path is specified.".to_string(),
        );
    }
    if opts.use_regex && !opts.find.is_empty() {
        validate_regex(&opts.find, opts.case_sensitive)?;
    }

    let charset = rec.charset();
    let delims = rec.delimiters();
    let seg_len = rec.segments().len();

    if let Some(ref target_addr) = opts.field_restriction {
        if target_addr.is_header_delimiter_field() {
            return Err(format!(
                "Cannot replace inside header delimiter field {target_addr}."
            ));
        }
        let mut total = 0usize;
        let mut new_segments: Vec<Vec<u8>> = Vec::with_capacity(seg_len);
        for seg_idx in 0..seg_len {
            let Some(old_seg) = rec.segment_bytes(seg_idx) else {
                continue;
            };
            if old_seg.len() >= 3 && old_seg[0..3] == target_addr.segment {
                let current_val = extract_decoded_field(rec, seg_idx, target_addr, &charset);
                let (replaced_val, count) = replace_in_string(&current_val, opts, true)?;
                if count > 0 {
                    let new_seg = crate::edit::rewrite_segment_field(
                        old_seg,
                        delims,
                        target_addr,
                        replaced_val.as_bytes(),
                    )
                    .map_err(|e| e.to_string())?;
                    new_segments.push(new_seg);
                    total += count;
                    continue;
                }
            }
            new_segments.push(old_seg.to_vec());
        }
        if total == 0 {
            return Ok(None);
        }
        let mut new_bytes = Vec::with_capacity(rec.bytes().len() + 32);
        for (i, seg) in new_segments.iter().enumerate() {
            if i > 0 {
                new_bytes.push(b'\r');
            }
            new_bytes.extend_from_slice(seg);
        }
        return Ok(Some((new_bytes, total)));
    }

    // Unrestricted HL7-safe replace across all editable leaf tokens in every segment.
    let mut total = 0usize;
    let mut new_segments: Vec<Vec<u8>> = Vec::with_capacity(seg_len);
    for seg_idx in 0..seg_len {
        let Some(old_seg) = rec.segment_bytes(seg_idx) else {
            continue;
        };
        if old_seg.len() < 4 {
            new_segments.push(old_seg.to_vec());
            continue;
        }
        let is_header =
            &old_seg[0..3] == b"MSH" || &old_seg[0..3] == b"FHS" || &old_seg[0..3] == b"BHS";
        let start_offset = if is_header { old_seg.len().min(8) } else { 4 };

        let mut out_seg = Vec::with_capacity(old_seg.len());
        out_seg.extend_from_slice(&old_seg[..start_offset]);

        let mut seg_count = 0usize;
        let mut token_start = start_offset;
        for i in start_offset..=old_seg.len() {
            let is_boundary = i == old_seg.len() || delims.contains(old_seg[i]);
            if is_boundary {
                let token = &old_seg[token_start..i];
                if !token.is_empty() {
                    let token_str = charset.decode_display(token);
                    let (replaced, count) = replace_in_string(&token_str, opts, false)?;
                    if count > 0 {
                        out_seg.extend_from_slice(replaced.as_bytes());
                        seg_count += count;
                    } else {
                        out_seg.extend_from_slice(token);
                    }
                }
                if i < old_seg.len() {
                    out_seg.push(old_seg[i]);
                }
                token_start = i + 1;
            }
        }

        if seg_count > 0 {
            total += seg_count;
            new_segments.push(out_seg);
        } else {
            new_segments.push(old_seg.to_vec());
        }
    }

    if total == 0 {
        return Ok(None);
    }

    let mut new_bytes = Vec::with_capacity(rec.bytes().len() + 32);
    for (i, seg) in new_segments.iter().enumerate() {
        if i > 0 {
            new_bytes.push(b'\r');
        }
        new_bytes.extend_from_slice(seg);
    }
    Ok(Some((new_bytes, total)))
}

fn is_empty_or_hl7_null(value: &str) -> bool {
    value.is_empty() || value == "\"\""
}

fn evaluate_field_op(
    actual: &str,
    op: FieldFilterOp,
    target: &str,
    case_sensitive: bool,
    use_regex: bool,
) -> bool {
    match op {
        FieldFilterOp::Equals => {
            if use_regex {
                text_matches(actual, target, case_sensitive, true)
            } else if target.is_empty() {
                is_empty_or_hl7_null(actual)
            } else if case_sensitive {
                actual == target
            } else {
                actual.eq_ignore_ascii_case(target)
            }
        }
        FieldFilterOp::NotEquals => {
            if use_regex {
                !text_matches(actual, target, case_sensitive, true)
            } else if target.is_empty() {
                !is_empty_or_hl7_null(actual)
            } else if case_sensitive {
                actual != target
            } else {
                !actual.eq_ignore_ascii_case(target)
            }
        }
        FieldFilterOp::Contains => text_matches(actual, target, case_sensitive, use_regex),
        FieldFilterOp::Regex => text_matches(actual, target, case_sensitive, true),
        FieldFilterOp::Exists => !is_empty_or_hl7_null(actual),
        FieldFilterOp::GreaterThan
        | FieldFilterOp::GreaterOrEqual
        | FieldFilterOp::LessThan
        | FieldFilterOp::LessOrEqual => {
            let Some(ord) = compare_field_scalar(actual, target, case_sensitive) else {
                return false;
            };
            match op {
                FieldFilterOp::GreaterThan => ord == Ordering::Greater,
                FieldFilterOp::GreaterOrEqual => ord == Ordering::Greater || ord == Ordering::Equal,
                FieldFilterOp::LessThan => ord == Ordering::Less,
                FieldFilterOp::LessOrEqual => ord == Ordering::Less || ord == Ordering::Equal,
                _ => false,
            }
        }
    }
}

/// Compare `actual` against `target` numerically (`f64`) if both parse as numbers,
/// chronologically (`Hl7Timestamp`) if both parse as HL7 timestamps (and length >= 8 or non-float),
/// or lexicographically as strings.
fn compare_field_scalar(actual: &str, target: &str, case_sensitive: bool) -> Option<Ordering> {
    let a = actual.trim();
    let b = target.trim();
    if a.is_empty() || b.is_empty() {
        return None;
    }

    // If both look like HL7 date/timestamps (8+ digits or timezone) and parse as Hl7Timestamp, compare instants.
    if a.len() >= 8 && b.len() >= 8 {
        if let (Some(ta), Some(tb)) = (Hl7Timestamp::parse(a), Hl7Timestamp::parse(b)) {
            return Some(ta.cmp(&tb));
        }
    }

    // If both parse as floating-point numbers, compare numerically.
    if let (Ok(na), Ok(nb)) = (a.parse::<f64>(), b.parse::<f64>()) {
        return na.partial_cmp(&nb);
    }

    // Try HL7 timestamp even for 4- or 6-digit year/month if valid.
    if let (Some(ta), Some(tb)) = (Hl7Timestamp::parse(a), Hl7Timestamp::parse(b)) {
        return Some(ta.cmp(&tb));
    }

    if case_sensitive {
        Some(a.cmp(b))
    } else {
        Some(a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
    }
}

fn unquote(s: &str) -> String {
    if s == r#"\"\""# {
        return "\"\"".to_string();
    }
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::load_from_bytes;
    use crate::scanner::ScannerConfig;

    #[test]
    fn filter_by_plain_text_and_canonical_field_paths() {
        let raw = concat!(
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
            "PID|1||12345^^^HOSP^MR||DOE^JOHN\r",
            "OBX|1|NM|GLU||105|mg/dL\r",
            "MSH|^~\\&|SND|FAC|RCV|FAC|20261120120001||ORU^R01|MSG2|P|2.5.1\r",
            "PID|1||67890^^^HOSP^MR||SMITH^JANE\r",
            "OBX|1|NM|GLU||245|mg/dL\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        let f_adt = RecordFilter::parse("MSH-9.1=ADT").unwrap();
        assert!(f_adt.matches_record(&records[0]));
        assert!(!f_adt.matches_record(&records[1]));

        let f_contains = RecordFilter::parse("PID-5.1 contains DOE").unwrap();
        assert!(f_contains.matches_record(&records[0]));
        assert!(!f_contains.matches_record(&records[1]));

        // Numeric comparison: OBX-5>200
        let f_gt = RecordFilter::parse("OBX-5>200").unwrap();
        assert!(!f_gt.matches_record(&records[0]));
        assert!(f_gt.matches_record(&records[1]));

        // Timestamp comparison: MSH-7>=20261101
        let f_date = RecordFilter::parse("MSH-7>=20261101").unwrap();
        assert!(!f_date.matches_record(&records[0]));
        assert!(f_date.matches_record(&records[1]));

        // Regex + Invert match
        let f_regex_inv = RecordFilter::parse_with_options(
            "^SM[A-Z]+$",
            FilterMatchOptions {
                case_sensitive: true,
                use_regex: true,
                invert_match: true,
            },
        )
        .unwrap();
        assert!(f_regex_inv.matches_record(&records[0]));

        let matches = find_all_search_matches(&records, "SMITH");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].record_index, 1);
        assert_eq!(matches[0].segment_index, 1);
        assert_eq!(matches[0].field_address, Some("PID-5.1".parse().unwrap()));

        let breakdown = summarize_matches_by_field(&find_all_search_matches(&records, "GLU"));
        assert_eq!(breakdown[0].field_path, "OBX-3");
        assert_eq!(breakdown[0].count, 2);
    }

    #[test]
    fn filter_and_find_replace_empty_null_and_missing_values() {
        let raw = concat!(
            // 0: Populated PID-3 (no component 4), populated PID-5.2
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
            "PID|1||12345||DOE^JOHN\r",
            // 1: Empty PID-3, missing PID-5.2 (trailing omitted)
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120100||ADT^A08|MSG2|P|2.5.1\r",
            "PID|1||||SMITH\r",
            // 2: HL7 explicit null PID-3 (""), populated PID-3.4 absent, empty PID-5.2
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120200||ORU^R01|MSG3|P|2.5.1\r",
            "PID|1||\"\"||JONES^\r",
            // 3: Missing PID segment completely
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120300||ACK|MSG4|P|2.5.1\r",
            "MSA|AA|MSG1\r",
            // 4: Populated PID-3.4 = HOSP
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120400||ADT^A04|MSG5|P|2.5.1\r",
            "PID|1||99999^^^HOSP||LEE^AMY\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        // PID-3= and PID-3="" match empty (1), explicit HL7 null (2), and missing PID segment (3)
        for expr in ["PID-3=", "PID-3=\"\""] {
            let f = RecordFilter::parse(expr).unwrap();
            let matched: Vec<usize> = records
                .iter()
                .enumerate()
                .filter_map(|(i, r)| f.matches_record(r).then_some(i))
                .collect();
            assert_eq!(matched, vec![1, 2, 3], "expr: {expr}");
        }

        // PID-3=\"\" matches only explicit HL7 null literal "" (record 2)
        let f_explicit_null = RecordFilter::parse(r#"PID-3=\"\""#).unwrap();
        let matched_null: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| f_explicit_null.matches_record(r).then_some(i))
            .collect();
        assert_eq!(matched_null, vec![2]);

        // PID-3!= and PID-3!="" match only populated PID-3 (records 0 and 4)
        for expr in ["PID-3!=", "PID-3!=\"\""] {
            let f = RecordFilter::parse(expr).unwrap();
            let matched: Vec<usize> = records
                .iter()
                .enumerate()
                .filter_map(|(i, r)| f.matches_record(r).then_some(i))
                .collect();
            assert_eq!(matched, vec![0, 4], "expr: {expr}");
        }

        // PID-5.2="" matches trailing-omitted (1), empty component (2), and missing PID (3)
        let f_pid52 = RecordFilter::parse("PID-5.2=\"\"").unwrap();
        let matched_pid52: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| f_pid52.matches_record(r).then_some(i))
            .collect();
        assert_eq!(matched_pid52, vec![1, 2, 3]);

        // Find with Target Path PID-3.4 and empty query finds records 0, 1, 2, 3 (missing PID-3.4), not 4
        let find_empty_pid34 = find_search_matches_with_options(
            &records,
            &SearchQueryOptions {
                query: String::new(),
                field_restriction: Some("PID-3.4".parse().unwrap()),
                case_sensitive: false,
                use_regex: false,
                match_whole: true,
            },
        )
        .unwrap();
        let hit_recs: Vec<usize> = find_empty_pid34.iter().map(|m| m.record_index).collect();
        assert_eq!(hit_recs, vec![0, 1, 2, 3]);

        // Empty query with NO Target Path returns 0 matches (inactive no-op)
        let noop = find_search_matches_with_options(
            &records,
            &SearchQueryOptions {
                query: String::new(),
                field_restriction: None,
                ..SearchQueryOptions::default()
            },
        )
        .unwrap();
        assert!(noop.is_empty());

        // Replace empty PID-3.4 with HOSP on PID|1||12345||DOE^JOHN expands intermediate delimiters -> PID|1||12345^^^HOSP||DOE^JOHN
        let rep_opts = Hl7ReplaceOptions {
            find: String::new(),
            replace: "HOSP".to_string(),
            field_restriction: Some("PID-3.4".parse().unwrap()),
            case_sensitive: false,
            use_regex: false,
            match_whole: true,
        };
        let (new_bytes, occ) = replace_in_record(&records[0], &rep_opts).unwrap().unwrap();
        assert_eq!(occ, 1);
        let new_str = String::from_utf8_lossy(&new_bytes);
        assert!(
            new_str.contains("PID|1||12345^^^HOSP||DOE^JOHN"),
            "unexpected replaced record: {new_str}"
        );
        // Record 4 already has PID-3.4=HOSP, so match_whole with empty find skips it (0 changes)
        assert!(replace_in_record(&records[4], &rep_opts).unwrap().is_none());
    }

    #[test]
    fn wildcard_repetition_contains_and_exists_filter_operators() {
        let raw = concat!(
            // 0: Multi-repetition PID-3 (AAA^MR~123^PI), has ROL-4
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
            "PID|1||AAA^^^HOSP^MR~123^^^CLINIC^PI||DOE^JOHN\r",
            "ROL|1|UP|AT|9999^HOUSE^GREGORY\r",
            "OBX|1|NM|GLU||95|mg/dL\r",
            // 1: Single PID-3 (999^MR), no ROL segment, OBX-5=200
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120100||ORU^R01|MSG2|P|2.5.1\r",
            "PID|1||999^^^HOSP^MR||SMITH^JANE\r",
            "OBX|1|NM|GLU||200|mg/dL\r",
            // 2: Empty PID-3, ROL with empty ROL-4
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120200||ADT^A08|MSG3|P|2.5.1\r",
            "PID|1||||JONES^ALEX\r",
            "ROL|1|UP|AT|\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        let matched = |expr: &str| -> Vec<usize> {
            let f = RecordFilter::parse(expr).unwrap();
            records
                .iter()
                .enumerate()
                .filter_map(|(i, r)| f.matches_record(r).then_some(i))
                .collect()
        };

        // Wildcard repetition exact match across ~-separated repetitions
        assert_eq!(matched("PID-3(*).1=123"), vec![0]);
        assert_eq!(matched("PID-3[*].1=123"), vec![0]);
        assert_eq!(matched("PID-3(2).1=123"), vec![0]);
        assert_eq!(matched("PID-3[2].1=123"), vec![0]);
        assert_eq!(matched("PID-3(1).1=123"), Vec::<usize>::new());

        // Segment wildcard notation
        assert_eq!(matched("OBX(*)-5=200"), vec![1]);

        // Contains operator with wildcard repetition (~= and 'contains')
        assert_eq!(matched("PID-3(*).1 ~= 23"), vec![0]);
        assert_eq!(matched("PID-3(*).1 contains 23"), vec![0]);
        assert_eq!(matched("PID-5.1 contains SMITH"), vec![1]);

        // Exists operator with wildcard repetition and segment presence
        assert_eq!(matched("PID-3(*).1 exists"), vec![0, 1]);
        assert_eq!(matched("ROL-4 exists"), vec![0]);
        assert_eq!(matched("OBX-5 exists"), vec![0, 1]);

        // Strict CLI parser also accepts wildcard, regex (=~), and exists
        assert!(RecordFilter::parse_field_predicate("PID-3(*).1=123").is_ok());
        assert!(RecordFilter::parse_field_predicate("PID-3(*).1 ~= 23").is_ok());
        assert!(RecordFilter::parse_field_predicate("PID-5.1 =~ ^SM.*").is_ok());
        assert!(RecordFilter::parse_field_predicate("ROL-4 exists").is_ok());

        // Regex (=~) operator and relational numeric/timestamp operators
        assert_eq!(matched("PID-5.1 =~ ^SM.*"), vec![1]);
        assert_eq!(matched("PID-5.1!=SMITH"), vec![0, 2]);
        assert_eq!(matched("OBX-5 >= 200"), vec![1]);
        assert_eq!(matched("OBX-5 < 100"), vec![0]);
        assert_eq!(matched("MSH-7 >= 20260115120100"), vec![1, 2]);
        assert_eq!(matched("MSH-7 <= 20260115120000"), vec![0]);
    }

    #[test]
    fn wildcard_star_filters_entire_raw_message_content() {
        let raw = concat!(
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
            "PID|1||MRN100^^^HOSP^MR||DOE^JANE\r",
            "OBX|1|TX|NOTE||Elevated Troponin\r",
            "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120100||ORU^R01|MSG2|P|2.5.1\r",
            "PID|1||MRN200^^^HOSP^MR||SMITH^JOHN\r",
            "ZCS|1|CUSTOM_ALPHA_TOKEN\r",
        );
        let records = load_from_bytes(raw.as_bytes(), ScannerConfig::default())
            .unwrap()
            .records;

        let matched = |expr: &str| -> Vec<usize> {
            let f = RecordFilter::parse(expr).unwrap();
            records
                .iter()
                .enumerate()
                .filter_map(|(i, r)| f.matches_record(r).then_some(i))
                .collect()
        };

        // Matches across PID, OBX, and custom Z* segments case-insensitively by default
        assert_eq!(matched("* ~= troponin"), vec![0]);
        assert_eq!(matched("* = doe^jane"), vec![0]);
        assert_eq!(matched("* contains custom_alpha_token"), vec![1]);
        assert_eq!(matched("* =~ custom_[a-z]+_token"), vec![1]);
        // Negation (`!=`) excludes messages containing the term anywhere
        assert_eq!(matched("* != custom_alpha_token"), vec![0]);
        assert_eq!(matched("* != troponin"), vec![1]);
        // Presence (`* exists`) matches all non-empty messages
        assert_eq!(matched("* exists"), vec![0, 1]);
    }
}
