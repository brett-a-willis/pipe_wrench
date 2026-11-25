//! Core HL7 v2 parsing, framing, addressing, I/O, editing, and serialization library.
//!
//! `hl7-core` has no UI and no async dependencies (NFR-4).
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![warn(missing_docs)]

pub mod ack;
pub mod address;
pub mod attachment;
pub mod delimiters;
pub mod dictionary;
pub mod diff;
pub mod edit;
pub mod encoding;
pub mod error;
pub mod filter;
pub mod io;
pub mod memory;
pub mod record;
pub mod report;
pub mod scan;
pub mod scanner;
pub mod scrub;
pub mod timestamp;

pub use ack::{
    build_hl7_ack, current_hl7_timestamp, evaluate_ack, extract_msa2_control_id,
    frame_ack_from_file, generate_unique_control_id, msh_suppresses_ack, AckCode, AckMode,
    AckResult,
};
pub use address::{parse_filter_field_path, FieldAddress};
pub use attachment::{
    decode_base64, detect_and_decode_attachment, detect_attachment_summary, detect_type_from_magic,
    extract_record_attachments, format_byte_size, AttachmentSummary, DetectedAttachment,
    ExtractedRecordAttachment, MIN_STANDALONE_BASE64_LEN,
};
pub use delimiters::Delimiters;
pub use dictionary::{
    data_type_component, field_data_type, DataTypeComponent, Hl7Dictionary, SegmentDictionaryEntry,
};
pub use diff::{
    compute_char_diff_spans, diff_hl7_message_set, diff_hl7_messages, CharDiffKind, CharDiffSpan,
    DiffChangeKind, DiffOptions, FieldDiffRow, MessageDiffResult, MultiFieldDiffRow,
    MultiMessageDiffResult, MultiSegmentDiffBlock, SegmentDiffBlock, TargetFieldDiffCell,
};
pub use edit::{
    clear_segment_field, rewrite_segment_field, AssignFieldCmd, BatchEditRecordsCmd,
    ChangeDelimitersCmd, DeleteRecordsCmd, DeleteSegmentsCmd, Document, EditCommand, EditFieldCmd,
    EditOutcome, EditRecordCmd, InsertRecordsCmd, ReorderRecordsCmd, ReplaceCmd, ReplaceScope,
    SelectionAnchor, SortRecordsCmd, UndoStack, UndoWarning, DEFAULT_UNDO_WARNING_BYTES,
};
pub use encoding::ResolvedCharset;
pub use error::{AddressError, CoreError, DelimiterError, EditError, IoError, ScanError};
pub use filter::{
    find_all_search_matches, find_search_matches_with_options, replace_in_record,
    replace_in_single_field, replace_in_string, summarize_matches_by_field, FieldFilterOp,
    FieldHitCount, FilterMatchOptions, Hl7ReplaceOptions, RecordFilter, ReplaceSummary,
    SearchMatch, SearchQueryOptions,
};
pub use io::{
    list_zip_entries, load_file, load_file_with_limit, load_files, load_from_bytes,
    load_zip_entries, load_zip_entries_with_limit, parse_batch_with_limit, save_records_atomic,
    save_selected_records_atomic, FileFilterList, LoadProgress, MultiLoadOutput, SaveOptions,
};
pub use memory::{
    estimate_file_set_metrics, format_memory_bytes, MemoryBudget, AST_EXPANSION_FACTOR,
    DEFAULT_RAM_BUDGET_FRACTION, MIN_SAFE_LOAD_BUDGET_BYTES, RSS_WARNING_FRACTION,
};
pub use record::{
    FieldDisplayOptions, FieldEntry, Record, RecordFlags, RecordKind, RecordSummary,
    SaveTerminator, SegmentIndex, SegmentSpan, SourceTerminator,
};
pub use report::{
    format_fixed_cell, parse_delimiter_separator, DelimitedExportOptions, DelimitedQuoteMode,
    ExportEncoding, ExportRowScope, FixedWidthAlign, FixedWidthColumnSpec, FixedWidthExportOptions,
    ReportColumn, ReportDefinition, ReportError, ReportRow, ReportRowGrain, ReportTable,
    SortKeySpec, FORBIDDEN_REPORT_NAME_CHARS, MAX_FIXED_COLUMN_WIDTH,
};
pub use scan::{
    normalize_extension_glob, scan_directory, scan_reader, BatchMismatch, DirectoryScanConfig,
    DirectoryScanReport, FileScanResult, FileScanStatus, GroupByKey, GroupByRow, RollupCounts,
    ScanProgress, DEFAULT_MAX_TRACKED_CONTROL_IDS, SCAN_WORKER_MAX_LINE_BYTES,
};
pub use scanner::{
    load_records_from_reader, load_records_from_reader_with_limit, LoadOutput, MllpEnvelope,
    RecordBoundaryScanner, RecoveryMode, ScanEvent, ScannerConfig, ScannerSummary,
};
pub use scrub::{
    count_custom_phi_matches, CustomPhiMode, CustomPhiRule, PhiScrubConfig, PhiScrubber,
    PHI_SCRUB_DISCLAIMER, PHI_SCRUB_DISCLAIMER_BODY, PHI_SCRUB_DISCLAIMER_HEADING,
};
pub use timestamp::{compare_typed_values, Hl7Timestamp, SortDirection, SortKeyType};
