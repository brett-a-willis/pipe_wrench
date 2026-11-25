//! Tabular report builder, typed sorting, report definition persistence, and exporters
//! (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`, `D-7`, `D-13`).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::address::FieldAddress;
use crate::dictionary::Hl7Dictionary;
use crate::record::Record;
use crate::timestamp::{compare_typed_values, SortDirection, SortKeyType};

/// Maximum column width allowed for fixed-width exports (`RPT-4`).
pub const MAX_FIXED_COLUMN_WIDTH: usize = 9_999;

/// Forbidden characters in report definition names (`RPT-6`).
pub const FORBIDDEN_REPORT_NAME_CHARS: &[char] =
    &['\\', '/', ':', '*', '?', '<', '>', '|', ',', ';', '%'];

/// Errors produced when building, sorting, saving, or exporting reports (`RPT-*`, `SORT-4`).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// Unknown escape shorthand in delimited export separator (`RPT-3`).
    #[error("Unknown escape sequence in separator.")]
    UnknownEscapeSeparator,
    /// Fixed-width column exceeded 9,999 characters (`RPT-4`).
    #[error("Field width cannot exceed 9,999 characters.")]
    FieldWidthExceeded,
    /// Invalid fixed-width column width of 0.
    #[error("Field widths must be at least 1 character.")]
    FieldWidthZero,
    /// Report definition name is empty or contains forbidden characters (`RPT-6`).
    #[error("Invalid report definition name '{name}': cannot be empty or contain \\ / : * ? < > | , ; %")]
    InvalidReportName {
        /// Rejected report name.
        name: String,
    },
    /// Multi-column sort exceeded 3 keys (`SORT-4`).
    #[error("Multi-column sort supports at most 3 keys (got {0}).")]
    TooManySortKeys(usize),
    /// Column index out of bounds.
    #[error("Report column index {index} out of bounds (table has {len} columns).")]
    ColumnOutOfBounds {
        /// Requested column index.
        index: usize,
        /// Number of columns in the report.
        len: usize,
    },
    /// Serialization or I/O error during report export/persistence.
    #[error("Report export error: {0}")]
    Export(String),
}

/// Row grain for a report (`§8`, `§12.1`, `D-13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReportRowGrain {
    /// One row per record (first occurrence of each segment, default per `D-13`).
    #[default]
    PerRecord,
    /// One row per occurrence of `driving_segment` within each record (`D-13`).
    PerSegment {
        /// 3-byte ASCII segment tag driving row expansion (e.g. `*b"OBX"`).
        driving_segment: [u8; 3],
    },
}

/// One configured column in a [`ReportDefinition`] (`RPT-1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportColumn {
    /// Field address for this column (e.g. `PID-3`).
    pub address: FieldAddress,
    /// Data type used when sorting this column (`String`, `Number`, `Date`).
    pub column_type: SortKeyType,
}

impl ReportColumn {
    /// Create a report column with the given address and type.
    pub fn new(address: FieldAddress, column_type: SortKeyType) -> Self {
        Self {
            address,
            column_type,
        }
    }
}

/// Serializable report definition (`RPT-1`, `RPT-6`, `G-6`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportDefinition {
    /// User-chosen report name (validated via [`validate_report_name`]).
    pub name: String,
    /// Row grain (`PerRecord` or `PerSegment`).
    pub grain: ReportRowGrain,
    /// Ordered list of columns.
    pub columns: Vec<ReportColumn>,
}

impl Default for ReportDefinition {
    fn default() -> Self {
        Self {
            name: "Untitled Report".to_string(),
            grain: ReportRowGrain::PerRecord,
            columns: Vec::new(),
        }
    }
}

impl ReportDefinition {
    /// Validate that `name` is non-empty and contains none of `\ / : * ? < > | , ; %` (`RPT-6`).
    pub fn validate_name(name: &str) -> Result<(), ReportError> {
        let trimmed = name.trim();
        if trimmed.is_empty()
            || trimmed
                .chars()
                .any(|c| FORBIDDEN_REPORT_NAME_CHARS.contains(&c))
        {
            return Err(ReportError::InvalidReportName {
                name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Serialize this report definition to TOML (`RPT-6`, `G-6`).
    pub fn to_toml(&self) -> Result<String, ReportError> {
        Self::validate_name(&self.name)?;
        toml::to_string_pretty(self).map_err(|e| ReportError::Export(e.to_string()))
    }

    /// Deserialize a report definition from TOML (`RPT-6`, `G-6`).
    pub fn from_toml(toml_str: &str) -> Result<Self, ReportError> {
        let def: Self = toml::from_str(toml_str).map_err(|e| ReportError::Export(e.to_string()))?;
        Self::validate_name(&def.name)?;
        Ok(def)
    }
}

/// One sort key in a single-column or multi-column report sort (`SORT-3`, `SORT-4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKeySpec {
    /// 0-based column index in the report.
    pub column_index: usize,
    /// Sort direction.
    pub direction: SortDirection,
}

/// One data row in a built [`ReportTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// 0-based source record index.
    pub record_index: usize,
    /// 1-based occurrence index of the driving segment (1 for `PerRecord`).
    pub segment_occurrence: usize,
    /// Formatted cell values aligned with `ReportDefinition::columns`.
    pub cells: Vec<String>,
}

/// Built tabular report with sorting and export operations (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`).
#[derive(Debug, Clone)]
pub struct ReportTable {
    /// Underlying report definition.
    pub definition: ReportDefinition,
    /// Column headers formatted as field addresses (e.g. `"PID-3"`).
    pub address_headers: Vec<String>,
    /// Column headers formatted as dictionary descriptions (falling back to address if unknown).
    pub description_headers: Vec<String>,
    /// Rows of the report table.
    pub rows: Vec<ReportRow>,
    /// Currently active sort keys (1 for header click `SORT-3`, up to 3 for `SORT-4`).
    pub active_sort: Vec<SortKeySpec>,
}

impl ReportTable {
    /// Build a [`ReportTable`] from `records` and an optional [`Hl7Dictionary`] (`RPT-1`, `D-13`).
    pub fn build(
        definition: ReportDefinition,
        records: &[Record],
        dictionary: Option<&Hl7Dictionary>,
    ) -> Self {
        let address_headers: Vec<String> = definition
            .columns
            .iter()
            .map(|c| c.address.to_string())
            .collect();

        let description_headers: Vec<String> = definition
            .columns
            .iter()
            .map(|c| {
                dictionary
                    .and_then(|d| d.field_description(&c.address))
                    .map_or_else(|| c.address.to_string(), str::to_string)
            })
            .collect();

        let mut rows = Vec::new();

        for (rec_idx, rec) in records.iter().enumerate() {
            match definition.grain {
                ReportRowGrain::PerRecord => {
                    let cells: Vec<String> = definition
                        .columns
                        .iter()
                        .map(|col| extract_cell_value(rec, &col.address, None))
                        .collect();
                    if !cells.is_empty() && cells.iter().all(|c| c.is_empty()) {
                        continue;
                    }
                    rows.push(ReportRow {
                        record_index: rec_idx,
                        segment_occurrence: 1,
                        cells,
                    });
                }
                ReportRowGrain::PerSegment { driving_segment } => {
                    let seg_indices: Vec<usize> = rec
                        .segments()
                        .find_by_tag(&driving_segment)
                        .map(|(idx, _)| idx)
                        .collect();

                    for (occ_idx, &driving_seg_idx) in seg_indices.iter().enumerate() {
                        let cells: Vec<String> = definition
                            .columns
                            .iter()
                            .map(|col| {
                                if col.address.segment == driving_segment {
                                    extract_cell_value(rec, &col.address, Some(driving_seg_idx))
                                } else {
                                    extract_cell_value(rec, &col.address, None)
                                }
                            })
                            .collect();
                        if !cells.is_empty() && cells.iter().all(|c| c.is_empty()) {
                            continue;
                        }
                        rows.push(ReportRow {
                            record_index: rec_idx,
                            segment_occurrence: occ_idx + 1,
                            cells,
                        });
                    }
                }
            }
        }

        Self {
            definition,
            address_headers,
            description_headers,
            rows,
            active_sort: Vec::new(),
        }
    }

    /// Return the active header names based on `use_descriptions`.
    pub fn headers(&self, use_descriptions: bool) -> &[String] {
        if use_descriptions {
            &self.description_headers
        } else {
            &self.address_headers
        }
    }

    /// Toggle ascending/descending sort on a single column (`SORT-3`).
    pub fn sort_by_column_toggle(
        &mut self,
        column_index: usize,
    ) -> Result<SortDirection, ReportError> {
        let len = self.definition.columns.len();
        if column_index >= len {
            return Err(ReportError::ColumnOutOfBounds {
                index: column_index,
                len,
            });
        }
        let next_dir = match self.active_sort.as_slice() {
            [single] if single.column_index == column_index => single.direction.toggle(),
            _ => SortDirection::Ascending,
        };
        let key = SortKeySpec {
            column_index,
            direction: next_dir,
        };
        self.sort_multi(&[key])?;
        Ok(next_dir)
    }

    /// Stable multi-column sort with up to 3 keys (`SORT-3`, `SORT-4`).
    pub fn sort_multi(&mut self, keys: &[SortKeySpec]) -> Result<(), ReportError> {
        if keys.len() > 3 {
            return Err(ReportError::TooManySortKeys(keys.len()));
        }
        let col_len = self.definition.columns.len();
        for k in keys {
            if k.column_index >= col_len {
                return Err(ReportError::ColumnOutOfBounds {
                    index: k.column_index,
                    len: col_len,
                });
            }
        }

        let columns = &self.definition.columns;
        self.rows.sort_by(|a, b| {
            for k in keys {
                let col_type = columns[k.column_index].column_type;
                let va = a.cells.get(k.column_index).map_or("", String::as_str);
                let vb = b.cells.get(k.column_index).map_or("", String::as_str);
                let ord = compare_typed_values(va, vb, col_type, k.direction);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });

        self.active_sort = keys.to_vec();
        Ok(())
    }

    /// Compute default fixed-width column widths (longest cell value or header, minimum 1) (`RPT-4`).
    pub fn default_column_widths(&self, use_descriptions_in_header: bool) -> Vec<usize> {
        let headers = self.headers(use_descriptions_in_header);
        headers
            .iter()
            .enumerate()
            .map(|(col_idx, h)| {
                let max_cell = self
                    .rows
                    .iter()
                    .filter_map(|r| r.cells.get(col_idx))
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0);
                h.chars()
                    .count()
                    .max(max_cell)
                    .clamp(1, MAX_FIXED_COLUMN_WIDTH)
            })
            .collect()
    }

    /// Return indices of columns whose configured width is narrower than the column header name (`RPT-4`).
    pub fn narrow_column_warnings(
        &self,
        specs: &[FixedWidthColumnSpec],
        use_descriptions_in_header: bool,
    ) -> Vec<usize> {
        let headers = self.headers(use_descriptions_in_header);
        specs
            .iter()
            .enumerate()
            .filter_map(|(i, spec)| {
                let header_len = headers.get(i).map_or(0, |h| h.chars().count());
                (spec.width < header_len).then_some(i)
            })
            .collect()
    }

    /// Export to delimited text (`RPT-3`, `RPT-9`).
    pub fn export_delimited(&self, opts: &DelimitedExportOptions) -> Result<Vec<u8>, ReportError> {
        let sep = parse_delimiter_separator(&opts.separator_input)?;
        let mut out = String::new();

        if opts.include_header {
            let headers = self.headers(opts.use_descriptions_in_header);
            write_delimited_line(&mut out, headers, &sep, opts.quote_mode);
        }

        for row in self.select_rows(&opts.scope) {
            write_delimited_line(&mut out, &row.cells, &sep, opts.quote_mode);
        }

        Ok(opts.encoding.encode(&out))
    }

    /// Export to fixed-width text (`RPT-4`, `RPT-9`).
    pub fn export_fixed_width(
        &self,
        opts: &FixedWidthExportOptions,
    ) -> Result<Vec<u8>, ReportError> {
        for spec in &opts.columns {
            if spec.width == 0 {
                return Err(ReportError::FieldWidthZero);
            }
            if spec.width > MAX_FIXED_COLUMN_WIDTH {
                return Err(ReportError::FieldWidthExceeded);
            }
        }

        let mut out = String::new();
        if opts.include_header {
            let headers = self.headers(opts.use_descriptions_in_header);
            write_fixed_width_line(&mut out, headers, &opts.columns, true);
        }

        for row in self.select_rows(&opts.scope) {
            write_fixed_width_line(&mut out, &row.cells, &opts.columns, false);
        }

        Ok(opts.encoding.encode(&out))
    }

    /// Format as space-padded fixed-width columns, CRLF-separated, with header for clipboard (`RPT-5`).
    ///
    /// Omits rows where every requested column is empty, trims trailing spaces on each line, and
    /// trims trailing blank lines (`trim_end()`).
    pub fn format_clipboard_text(
        &self,
        scope: &ExportRowScope,
        use_descriptions_in_header: bool,
    ) -> String {
        let widths = self.default_column_widths(use_descriptions_in_header);
        let specs: Vec<FixedWidthColumnSpec> = widths
            .into_iter()
            .map(|w| FixedWidthColumnSpec {
                width: (w + 1).min(MAX_FIXED_COLUMN_WIDTH),
                align: FixedWidthAlign::LeftSpace,
            })
            .collect();

        let mut lines: Vec<String> = Vec::new();
        let mut header_line = String::new();
        write_fixed_width_line(
            &mut header_line,
            self.headers(use_descriptions_in_header),
            &specs,
            true,
        );
        lines.push(header_line.trim_end().to_string());

        for row in self.select_rows(scope) {
            if !row.cells.is_empty() && row.cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let mut row_line = String::new();
            write_fixed_width_line(&mut row_line, &row.cells, &specs, false);
            let trimmed = row_line.trim_end();
            if !trimmed.is_empty() {
                lines.push(trimmed.to_string());
            }
        }

        lines.join("\r\n").trim_end().to_string()
    }

    /// Export to RFC 4180 CSV (`RPT-8`, `RPT-9`).
    pub fn export_csv(
        &self,
        scope: &ExportRowScope,
        include_header: bool,
        use_descriptions_in_header: bool,
        encoding: ExportEncoding,
    ) -> Result<Vec<u8>, ReportError> {
        let mut wtr = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(Vec::new());

        if include_header {
            wtr.write_record(self.headers(use_descriptions_in_header))
                .map_err(|e| ReportError::Export(e.to_string()))?;
        }
        for row in self.select_rows(scope) {
            wtr.write_record(&row.cells)
                .map_err(|e| ReportError::Export(e.to_string()))?;
        }
        let raw_utf8 = wtr
            .into_inner()
            .map_err(|e| ReportError::Export(e.to_string()))?;
        let text = String::from_utf8_lossy(&raw_utf8);
        Ok(encoding.encode(&text))
    }

    /// Export to JSON array of objects keyed by field address (`RPT-8`, `RPT-9`).
    pub fn export_json(
        &self,
        scope: &ExportRowScope,
        encoding: ExportEncoding,
    ) -> Result<Vec<u8>, ReportError> {
        let mut arr = Vec::new();
        for row in self.select_rows(scope) {
            let mut obj = Map::new();
            for (col_idx, key) in self.address_headers.iter().enumerate() {
                let val = row.cells.get(col_idx).cloned().unwrap_or_default();
                obj.insert(key.clone(), Value::String(val));
            }
            arr.push(Value::Object(obj));
        }
        let json_str =
            serde_json::to_string_pretty(&arr).map_err(|e| ReportError::Export(e.to_string()))?;
        Ok(encoding.encode(&json_str))
    }

    fn select_rows<'a>(&'a self, scope: &ExportRowScope) -> Vec<&'a ReportRow> {
        match scope {
            ExportRowScope::AllRows => self.rows.iter().collect(),
            ExportRowScope::SelectedRows(indices) => {
                indices.iter().filter_map(|&i| self.rows.get(i)).collect()
            }
        }
    }
}

/// Output character encoding for report exports (`RPT-9`, `D-7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportEncoding {
    /// UTF-8 without BOM (default per `D-7`).
    #[default]
    Utf8,
    /// UTF-8 with leading `EF BB BF` BOM (`RPT-9`).
    Utf8Bom,
    /// Windows-1252 (`G-2`, `RPT-9`).
    Windows1252,
}

impl ExportEncoding {
    /// Encode `text` into bytes using this export encoding.
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => {
                let mut out = Vec::with_capacity(3 + text.len());
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                out.extend_from_slice(text.as_bytes());
                out
            }
            Self::Windows1252 => {
                let (cow, _, _) = encoding_rs::WINDOWS_1252.encode(text);
                cow.into_owned()
            }
        }
    }
}

/// Scope of rows included in a report export (`RPT-3`, `RPT-4`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExportRowScope {
    /// Export all rows in the table.
    #[default]
    AllRows,
    /// Export only the specified 0-based row indices.
    SelectedRows(Vec<usize>),
}

/// Quoting policy for delimited text exports (`RPT-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DelimitedQuoteMode {
    /// Quote only fields that contain the separator or `"`.
    #[default]
    QuoteContainingSeparator,
    /// Quote every field unconditionally.
    QuoteAll,
}

/// Options for [`ReportTable::export_delimited`] (`RPT-3`, `RPT-9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelimitedExportOptions {
    /// All rows or selected rows.
    pub scope: ExportRowScope,
    /// Include header row.
    pub include_header: bool,
    /// Use field descriptions instead of addresses in the header row.
    pub use_descriptions_in_header: bool,
    /// User-entered separator string (literals + `\a \b \t \f \n \r` or `^a ^b ^t ^f ^n ^r`).
    pub separator_input: String,
    /// Quoting mode.
    pub quote_mode: DelimitedQuoteMode,
    /// Output encoding.
    pub encoding: ExportEncoding,
}

impl Default for DelimitedExportOptions {
    fn default() -> Self {
        Self {
            scope: ExportRowScope::AllRows,
            include_header: true,
            use_descriptions_in_header: false,
            separator_input: ",".to_string(),
            quote_mode: DelimitedQuoteMode::QuoteContainingSeparator,
            encoding: ExportEncoding::Utf8,
        }
    }
}

/// Column alignment and padding for fixed-width exports (`RPT-4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FixedWidthAlign {
    /// Left-justified, space-padded on the right (truncation keeps left side).
    #[default]
    LeftSpace,
    /// Right-justified, space-padded on the left (truncation keeps right side).
    RightSpace,
    /// Right-justified, zero-padded (`'0'`) on the left (truncation keeps right side).
    RightZero,
}

/// Per-column specification for fixed-width exports (`RPT-4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedWidthColumnSpec {
    /// Character width (`1..=9999`).
    pub width: usize,
    /// Alignment and fill character.
    pub align: FixedWidthAlign,
}

/// Options for [`ReportTable::export_fixed_width`] (`RPT-4`, `RPT-9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedWidthExportOptions {
    /// All rows or selected rows.
    pub scope: ExportRowScope,
    /// Include header row.
    pub include_header: bool,
    /// Use field descriptions instead of addresses in the header row.
    pub use_descriptions_in_header: bool,
    /// Per-column widths and alignments.
    pub columns: Vec<FixedWidthColumnSpec>,
    /// Output encoding.
    pub encoding: ExportEncoding,
}

/// Parse a user-entered separator string supporting literal characters plus escape shorthand
/// `\a \b \t \f \n \r` and `^a ^b ^t ^f ^n ^r` (`RPT-3`).
///
/// Any unknown escape returns [`ReportError::UnknownEscapeSeparator`]
/// (`"Unknown escape sequence in separator."`).
pub fn parse_delimiter_separator(input: &str) -> Result<String, ReportError> {
    if input.is_empty() {
        return Err(ReportError::UnknownEscapeSeparator);
    }
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();

    while let Some(c) = chars.next() {
        if c == '\\' || c == '^' {
            let Some(next) = chars.next() else {
                // Lone `\` or `^` as the entire single-char separator is allowed if literal,
                // except when part of an invalid escape sequence.
                if input.len() == 1 {
                    out.push(c);
                    break;
                }
                return Err(ReportError::UnknownEscapeSeparator);
            };
            let mapped = match next {
                'a' | 'A' => '\x07',
                'b' | 'B' => '\x08',
                't' | 'T' => '\t',
                'f' | 'F' => '\x0C',
                'n' | 'N' => '\n',
                'r' | 'R' => '\r',
                '\\' if c == '\\' => '\\',
                '^' if c == '^' => '^',
                _ => return Err(ReportError::UnknownEscapeSeparator),
            };
            out.push(mapped);
        } else {
            out.push(c);
        }
    }

    Ok(out)
}

fn write_delimited_line(
    out: &mut String,
    cells: &[String],
    sep: &str,
    quote_mode: DelimitedQuoteMode,
) {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        let needs_quote = match quote_mode {
            DelimitedQuoteMode::QuoteAll => true,
            DelimitedQuoteMode::QuoteContainingSeparator => {
                cell.contains(sep) || cell.contains('"')
            }
        };
        if needs_quote {
            out.push('"');
            for ch in cell.chars() {
                if ch == '"' {
                    out.push_str("\"\"");
                } else {
                    out.push(ch);
                }
            }
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push_str("\r\n");
}

fn write_fixed_width_line(
    out: &mut String,
    cells: &[String],
    specs: &[FixedWidthColumnSpec],
    is_header: bool,
) {
    for (i, spec) in specs.iter().enumerate() {
        let val = cells.get(i).map_or("", String::as_str);
        // Header cells always pad with spaces (never zero-fill headers).
        let align = if is_header && spec.align == FixedWidthAlign::RightZero {
            FixedWidthAlign::RightSpace
        } else {
            spec.align
        };
        out.push_str(&format_fixed_cell(val, spec.width, align));
    }
    out.push_str("\r\n");
}

/// Format a single cell to exact character `width` according to `align` (`RPT-4`).
pub fn format_fixed_cell(val: &str, width: usize, align: FixedWidthAlign) -> String {
    let chars: Vec<char> = val.chars().collect();
    if chars.len() >= width {
        return match align {
            FixedWidthAlign::LeftSpace => chars[..width].iter().collect(),
            FixedWidthAlign::RightSpace | FixedWidthAlign::RightZero => {
                chars[chars.len() - width..].iter().collect()
            }
        };
    }

    let pad_count = width - chars.len();
    match align {
        FixedWidthAlign::LeftSpace => {
            let mut s: String = chars.into_iter().collect();
            for _ in 0..pad_count {
                s.push(' ');
            }
            s
        }
        FixedWidthAlign::RightSpace => {
            let mut s = String::with_capacity(width);
            for _ in 0..pad_count {
                s.push(' ');
            }
            s.extend(chars);
            s
        }
        FixedWidthAlign::RightZero => {
            let mut s = String::with_capacity(width);
            for _ in 0..pad_count {
                s.push('0');
            }
            s.extend(chars);
            s
        }
    }
}

fn extract_cell_value(
    rec: &Record,
    addr: &FieldAddress,
    specific_seg_idx: Option<usize>,
) -> String {
    let seg_idx = match specific_seg_idx {
        Some(idx) => idx,
        None => {
            let Some((first_idx, _)) = rec.segments().find_by_tag(&addr.segment).next() else {
                return String::new();
            };
            first_idx
        }
    };
    let Some(bytes) = rec.field_bytes(seg_idx, addr) else {
        return String::new();
    };
    rec.charset().decode_display(bytes).into_owned()
}
