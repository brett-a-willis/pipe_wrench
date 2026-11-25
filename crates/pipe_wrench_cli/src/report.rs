//! Implementation of the `pipe_wrench_cli report` subcommand (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use directories::ProjectDirs;
use hl7_core::{
    load_files, DelimitedExportOptions, DelimitedQuoteMode, ExportEncoding, ExportRowScope,
    FieldAddress, FilterMatchOptions, FixedWidthAlign, FixedWidthColumnSpec,
    FixedWidthExportOptions, Hl7Dictionary, RecordFilter, RecoveryMode, ReportColumn,
    ReportDefinition, ReportRowGrain, ReportTable, ScannerConfig, SortDirection, SortKeySpec,
    SortKeyType,
};

/// Output format for `pipe_wrench_cli report` (`RPT-3..RPT-8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ReportOutputFormat {
    /// RFC 4180 CSV (`RPT-8`).
    #[default]
    Csv,
    /// JSON array of objects keyed by field address (`RPT-8`).
    Json,
    /// Custom delimited text (`RPT-3`).
    Delimited,
    /// Fixed-width columns (`RPT-4`).
    Fixed,
}

/// Output character encoding for `pipe_wrench_cli report` (`RPT-9`, `D-7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CliExportEncoding {
    /// UTF-8 without BOM (default).
    #[default]
    #[value(name = "utf8")]
    Utf8,
    /// UTF-8 with leading `EF BB BF` BOM.
    #[value(name = "utf8-bom")]
    Utf8Bom,
    /// Windows-1252.
    #[value(name = "win1252")]
    Win1252,
}

impl From<CliExportEncoding> for ExportEncoding {
    fn from(val: CliExportEncoding) -> Self {
        match val {
            CliExportEncoding::Utf8 => Self::Utf8,
            CliExportEncoding::Utf8Bom => Self::Utf8Bom,
            CliExportEncoding::Win1252 => Self::Windows1252,
        }
    }
}

/// Arguments for `pipe_wrench_cli report` (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`).
#[derive(Debug, Clone, Parser)]
pub struct ReportArgs {
    /// One or more input files (`.hl7`, `.txt`, `.dat`, `.gz`, `.zip`) to extract from.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Load a saved TOML `ReportDefinition` by name or file path (`RPT-6`).
    #[arg(long)]
    pub definition: Option<String>,

    /// Comma-separated field addresses with optional typed suffixes (`OBX-5:number`, `MSH-7:date`, default `string`) (`RPT-1`).
    #[arg(long)]
    pub fields: Option<String>,

    /// Row grain: `record` or `segment:<TAG>` (e.g. `segment:OBX`) (`D-13`).
    #[arg(long)]
    pub grain: Option<String>,

    /// Optional plain-text or field-path filter expression supporting `(*)` wildcard repetitions
    /// and `=`, `!=`, `~=`, `=~`, `contains`, `exists`, `>`, `>=`, `<`, `<=` (`D-26`).
    #[arg(long)]
    pub filter: Option<String>,

    /// Optional field-path filter expression supporting `(*)` wildcard repetitions and `=`, `!=`, `~=`, `=~`,
    /// `contains`, `exists`, `>`, `>=`, `<`, `<=` (e.g. `PID-3(*).1=123`, `ROL-4 exists`) (`D-26`).
    #[arg(long)]
    pub filter_field: Vec<String>,

    /// Case-insensitive filter matching (default: true; pass `--ignore-case=false` for case-sensitive) (`D-26`).
    #[arg(long, short = 'i', default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub ignore_case: bool,

    /// Interpret `--filter` and `--filter-field` values as regular expressions (`D-26`).
    #[arg(long, default_value_t = false)]
    pub regex: bool,

    /// Invert filter match (`-v` / `--invert-match`) (`D-26`).
    #[arg(long, short = 'v', default_value_t = false)]
    pub invert_match: bool,

    /// Up to 3 sort keys (`<addr>[:asc|desc]`, comma-separated or repeatable) (`SORT-3`, `SORT-4`).
    #[arg(long)]
    pub sort_by: Vec<String>,

    /// Output format (`csv`, `json`, `delimited`, or `fixed`).
    #[arg(long, value_enum, default_value_t = ReportOutputFormat::Csv)]
    pub format: ReportOutputFormat,

    /// Separator for `--format delimited` (supports `\t`, `\n`, `^t`, etc.; default `,`) (`RPT-3`).
    #[arg(long, default_value = ",")]
    pub sep: String,

    /// Omit header row in `csv`, `delimited`, or `fixed` output.
    #[arg(long, default_value_t = false)]
    pub no_header: bool,

    /// Output character encoding (`utf8`, `utf8-bom`, or `win1252`) (`RPT-9`).
    #[arg(long, value_enum, default_value_t = CliExportEncoding::Utf8)]
    pub encoding: CliExportEncoding,

    /// Write output to file instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Parse `--fields <addr1[:type],addr2[:type],...>` into [`ReportColumn`]s (`RPT-1`).
pub fn parse_report_columns(spec: &str) -> Result<Vec<ReportColumn>, String> {
    let mut cols = Vec::new();
    for raw in spec.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let (addr_part, col_type) = match token.split_once(':') {
            Some((a, t)) => {
                let kt = match t.trim().to_ascii_lowercase().as_str() {
                    "string" | "str" => SortKeyType::String,
                    "number" | "num" => SortKeyType::Number,
                    "date" | "time" | "timestamp" => SortKeyType::Date,
                    other => {
                        return Err(format!(
                            "Invalid field type '{other}' in '{token}': expected string, number, or date."
                        ))
                    }
                };
                (a.trim(), kt)
            }
            None => (token, SortKeyType::String),
        };
        let addr: FieldAddress = addr_part
            .parse()
            .map_err(|e| format!("Invalid field address '{addr_part}': {e}"))?;
        cols.push(ReportColumn::new(addr, col_type));
    }
    if cols.is_empty() {
        return Err("At least one field must be specified in --fields.".to_string());
    }
    Ok(cols)
}

/// Parse `--grain record|segment:<TAG>` into a [`ReportRowGrain`] (`D-13`).
pub fn parse_report_grain(spec: &str) -> Result<ReportRowGrain, String> {
    let trimmed = spec.trim();
    if trimmed.eq_ignore_ascii_case("record") {
        return Ok(ReportRowGrain::PerRecord);
    }
    if let Some(tag_str) = trimmed
        .strip_prefix("segment:")
        .or_else(|| trimmed.strip_prefix("SEGMENT:"))
    {
        let tag = tag_str.trim().to_ascii_uppercase();
        let bytes = tag.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
            return Err(format!(
                "Invalid driving segment tag '{tag}' in --grain: expected 3 ASCII characters."
            ));
        }
        return Ok(ReportRowGrain::PerSegment {
            driving_segment: [bytes[0], bytes[1], bytes[2]],
        });
    }
    Err(format!(
        "Invalid --grain '{trimmed}': expected 'record' or 'segment:<TAG>'."
    ))
}

/// Execute `pipe_wrench_cli report`, writing to `--out` (if set) and returning the encoded output bytes.
pub fn execute_report_command(args: &ReportArgs) -> Result<Vec<u8>, String> {
    let mut def = match &args.definition {
        Some(name_or_path) => load_named_report_definition(name_or_path)?,
        None => ReportDefinition {
            name: "CLI Report".to_string(),
            grain: ReportRowGrain::PerRecord,
            columns: Vec::new(),
        },
    };

    if let Some(ref fields_spec) = args.fields {
        def.columns = parse_report_columns(fields_spec)?;
    }
    if let Some(ref grain_spec) = args.grain {
        def.grain = parse_report_grain(grain_spec)?;
    }

    if def.columns.is_empty() {
        return Err(
            "No report columns defined. Pass --fields <addr1,addr2,...> or --definition <name>."
                .to_string(),
        );
    }

    let cancel = AtomicBool::new(false);
    let scanner_cfg = ScannerConfig {
        recovery_mode: RecoveryMode::RecoverNonHl7,
        ..ScannerConfig::default()
    };
    let mut load_out =
        load_files(&args.files, scanner_cfg, &cancel, |_| {}).map_err(|e| e.to_string())?;

    let filter_opts = FilterMatchOptions {
        case_sensitive: !args.ignore_case,
        use_regex: args.regex,
        invert_match: args.invert_match,
    };
    if let Some(filter) = RecordFilter::from_cli_specs_with_options(
        args.filter.as_deref(),
        &args.filter_field,
        filter_opts,
    )? {
        load_out.records.retain(|r| filter.matches_record(r));
    }

    let dict = Hl7Dictionary::builtin();
    let mut table = ReportTable::build(def, &load_out.records, Some(&dict));

    if !args.sort_by.is_empty() {
        let sort_specs = parse_sort_by_specs(&args.sort_by, &table.definition.columns)?;
        table.sort_multi(&sort_specs).map_err(|e| e.to_string())?;
    }

    let encoding: ExportEncoding = args.encoding.into();
    let include_header = !args.no_header;

    let bytes = match args.format {
        ReportOutputFormat::Csv => table
            .export_csv(&ExportRowScope::AllRows, include_header, false, encoding)
            .map_err(|e| e.to_string())?,
        ReportOutputFormat::Json => table
            .export_json(&ExportRowScope::AllRows, encoding)
            .map_err(|e| e.to_string())?,
        ReportOutputFormat::Delimited => {
            let opts = DelimitedExportOptions {
                scope: ExportRowScope::AllRows,
                include_header,
                use_descriptions_in_header: false,
                separator_input: args.sep.clone(),
                quote_mode: DelimitedQuoteMode::QuoteContainingSeparator,
                encoding,
            };
            table.export_delimited(&opts).map_err(|e| e.to_string())?
        }
        ReportOutputFormat::Fixed => {
            let widths = table.default_column_widths(false);
            let columns: Vec<FixedWidthColumnSpec> = widths
                .into_iter()
                .map(|w| FixedWidthColumnSpec {
                    width: (w + 1).min(hl7_core::MAX_FIXED_COLUMN_WIDTH),
                    align: FixedWidthAlign::LeftSpace,
                })
                .collect();
            let opts = FixedWidthExportOptions {
                scope: ExportRowScope::AllRows,
                include_header,
                use_descriptions_in_header: false,
                columns,
                encoding,
            };
            table.export_fixed_width(&opts).map_err(|e| e.to_string())?
        }
    };

    if let Some(ref out_path) = args.out {
        std::fs::write(out_path, &bytes)
            .map_err(|e| format!("Failed writing '{}': {e}", out_path.display()))?;
    }

    Ok(bytes)
}

fn parse_sort_by_specs(
    raw_specs: &[String],
    columns: &[ReportColumn],
) -> Result<Vec<SortKeySpec>, String> {
    let mut out = Vec::new();
    for item in raw_specs.iter().flat_map(|s| s.split(',')) {
        let token = item.trim();
        if token.is_empty() {
            continue;
        }
        let (addr_part, dir) = match token.split_once(':') {
            Some((a, d)) => {
                let dir = match d.trim().to_ascii_lowercase().as_str() {
                    "asc" | "ascending" => SortDirection::Ascending,
                    "desc" | "descending" => SortDirection::Descending,
                    other => {
                        return Err(format!(
                            "Invalid sort direction '{other}' in '{token}': expected asc or desc."
                        ))
                    }
                };
                (a.trim(), dir)
            }
            None => (token, SortDirection::Ascending),
        };
        let addr: FieldAddress = addr_part
            .parse()
            .map_err(|e| format!("Invalid --sort-by address '{addr_part}': {e}"))?;
        let col_idx = columns
            .iter()
            .position(|c| c.address == addr)
            .ok_or_else(|| {
                format!("--sort-by address '{addr}' is not present in the report columns.")
            })?;
        out.push(SortKeySpec {
            column_index: col_idx,
            direction: dir,
        });
    }
    Ok(out)
}

fn load_named_report_definition(name_or_path: &str) -> Result<ReportDefinition, String> {
    let direct = Path::new(name_or_path);
    if direct.is_file() {
        let body = std::fs::read_to_string(direct).map_err(|e| {
            format!(
                "Failed reading report definition '{}': {e}",
                direct.display()
            )
        })?;
        return ReportDefinition::from_toml(&body).map_err(|e| e.to_string());
    }
    if let Some(dirs) = ProjectDirs::from("io", "pipe_wrench", "pipe_wrench") {
        let candidate = dirs
            .config_dir()
            .join("reports")
            .join(format!("{name_or_path}.toml"));
        if candidate.is_file() {
            let body = std::fs::read_to_string(&candidate).map_err(|e| {
                format!(
                    "Failed reading report definition '{}': {e}",
                    candidate.display()
                )
            })?;
            return ReportDefinition::from_toml(&body).map_err(|e| e.to_string());
        }
    }
    Err(format!("Report definition '{name_or_path}' not found."))
}
