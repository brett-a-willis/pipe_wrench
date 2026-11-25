//! Application session state, selection synchronization, cached record breakdown,
//! and background non-blocking file loading (G-5, §5.1..§5.5, FO-3..FO-15, FS-1..FS-8, NFR-1).

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Idle debounce duration before an active live raw-editor typing burst is coalesced
/// onto [`UndoStack`] as a single [`EditRecordCmd`] (`EDIT-2`, `D-26`).
pub const INLINE_RAW_DEBOUNCE_DURATION: Duration = Duration::from_millis(600);

/// Pre-edit baseline snapshot captured at the start of a continuous typing burst
/// in the live Raw Message Editor so keystrokes coalesce into one [`EditRecordCmd`] (`EDIT-2`).
#[derive(Debug, Clone)]
pub struct PendingInlineRawEdit {
    /// Target 0-based record index being edited.
    pub record_index: usize,
    /// Record bytes before the typing burst began.
    pub baseline_bytes: Vec<u8>,
    /// Record kind before the typing burst began.
    pub baseline_kind: RecordKind,
    /// Active delimiters before the typing burst began.
    pub baseline_delimiters: Delimiters,
    /// Record `edited` flag before the typing burst began.
    pub baseline_edited: bool,
    /// Formatted raw editor text before the typing burst began.
    pub baseline_text: String,
    /// Latest CR-joined record bytes produced by the live typing burst.
    pub latest_bytes: Vec<u8>,
    /// Timestamp of the most recent keystroke in the live typing burst.
    pub last_modified_at: Instant,
}

use hl7_core::{
    clear_segment_field, current_hl7_timestamp, detect_and_decode_attachment, diff_hl7_message_set,
    diff_hl7_messages, estimate_file_set_metrics, find_search_matches_with_options,
    format_byte_size, list_zip_entries, load_file, load_file_with_limit, load_from_bytes,
    load_zip_entries_with_limit, replace_in_record, replace_in_single_field, save_records_atomic,
    save_selected_records_atomic, summarize_matches_by_field, AttachmentSummary,
    BatchEditRecordsCmd, DeleteRecordsCmd, DeleteSegmentsCmd, Delimiters, DetectedAttachment,
    DiffOptions, Document, EditCommand, EditError, EditFieldCmd, EditOutcome, EditRecordCmd,
    FieldAddress, FieldDisplayOptions, FieldEntry, FieldHitCount, FilterMatchOptions,
    Hl7Dictionary, Hl7ReplaceOptions, InsertRecordsCmd, LoadProgress, MemoryBudget,
    MessageDiffResult, MultiMessageDiffResult, PhiScrubber, Record, RecordFilter, RecordFlags,
    RecordKind, RecordSummary, RecoveryMode, ReorderRecordsCmd, ReplaceSummary, ResolvedCharset,
    SaveOptions, SaveTerminator, ScannerConfig, SearchMatch, SearchQueryOptions, SelectionAnchor,
    SortDirection, SortKeyType, SortRecordsCmd, SourceTerminator, UndoStack, UndoWarning,
};

use crate::config::AppSettings;
use crate::strings;
use crate::ui::dialogs::{
    AssignFieldDialog, ChangeDelimitersDialog, DeleteConfirmDialog, EditFieldDialog,
    EditRecordDialog, EditSegmentDialog, InsertSegmentDialog, InsertSegmentMode, ReplaceDialog,
};
use crate::ui::net_panels::NetworkPanelState;
use crate::ui::reports::{ReportWindowState, SortRecordsDialog};
use crate::ui::scan_window::DirectoryScanWindowState;

/// Origin of the currently loaded records (FO-3, FO-15, FS-2, NET-13).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DataSource {
    /// No records loaded yet.
    #[default]
    Empty,
    /// Loaded from one or more files on disk (FO-3).
    Files(Vec<PathBuf>),
    /// Pasted from the system clipboard (FO-15).
    Clipboard,
    /// Received from the TCP/IP MLLP listener (NET-13).
    TcpIp,
}

impl DataSource {
    /// Window title suffix describing the data source (FO-3, FO-15, NET-13).
    pub fn display_label(&self) -> String {
        match self {
            Self::Empty => "No file loaded".to_string(),
            Self::Files(paths) => match paths.as_slice() {
                [single] => single.display().to_string(),
                many => format!("{} files loaded", many.len()),
            },
            Self::Clipboard => strings::SOURCE_CLIPBOARD.to_string(),
            Self::TcpIp => strings::SOURCE_TCPIP.to_string(),
        }
    }

    /// Returns the single file path when `Save Now` can overwrite in place without prompting (FS-1, FS-2).
    pub fn single_file_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Files(paths) if paths.len() == 1 => paths.first(),
            _ => None,
        }
    }
}

/// One pre-rendered segment row for the selected record (§5.1, NFR-1).
#[derive(Debug, Clone)]
pub struct SegmentViewRow {
    /// 0-based segment index in the record.
    pub seg_index: usize,
    /// 3-character segment tag (e.g. `"PID"`).
    pub tag: String,
    /// 1-based occurrence of this segment tag in the record.
    pub occurrence: usize,
    /// Optional segment description from the dictionary (§5.4).
    pub description: Option<String>,
    /// Preview of the raw segment text.
    pub preview: String,
    /// Character/byte range of this segment inside [`SelectedRecordCache::raw_text`].
    pub raw_text_range: Range<usize>,
    /// Range of row indices in [`SelectedRecordCache::field_rows`] belonging to this segment.
    pub field_rows_range: Range<usize>,
}

/// One pre-rendered field row for the selected record (§5.2, §5.5, NFR-1).
#[derive(Debug, Clone)]
pub struct FieldViewRow {
    /// 0-based row index in `field_rows`.
    pub row_index: usize,
    /// 0-based parent segment index.
    pub seg_index: usize,
    /// Parsed field address, or `None` for non-addressable rows.
    pub address: Option<FieldAddress>,
    /// Optional dictionary description for this field, component, or subcomponent (§5.4).
    pub description: Option<String>,
    /// Optional full hierarchical tooltip (e.g. `"PID-5.1 — Patient Name › Family Name (XPN.1)"`).
    pub tooltip: Option<String>,
    /// Decoded value string (either verbatim or escape-decoded per §5.5).
    pub display_value: String,
    /// Reserved non-editable marker flag (always `false`).
    pub is_parsed_marker: bool,
    /// `true` if the field can be edited (EDIT-3).
    pub is_editable: bool,
    /// Approximate highlight range inside [`SelectedRecordCache::raw_text`].
    pub raw_text_range: Range<usize>,
    /// Detected Base64 / HL7 `ED` attachment metadata for one-click extraction in the Segment Accordion.
    pub attachment: Option<AttachmentSummary>,
}

/// Cached breakdown for the currently selected record so all 4 panes render in < 50 ms (NFR-1).
#[derive(Debug, Clone)]
pub struct SelectedRecordCache {
    /// Selected record index.
    pub record_index: usize,
    /// Resolved character set for this record (G-2).
    pub charset: ResolvedCharset,
    /// Missing MLLP start block flag (NET-4).
    pub missing_start_block: bool,
    /// Pre-built segment rows.
    pub segments: Vec<SegmentViewRow>,
    /// Pre-built field rows across all segments of the selected record.
    pub field_rows: Vec<FieldViewRow>,
    /// Displayable raw text of the record (segments separated by `\n` for the raw pane).
    pub raw_text: String,
}

/// Messages sent from the background file-loader worker thread to the UI thread (G-5, FO-13).
#[derive(Debug)]
pub enum LoaderMessage {
    /// Incremental progress update.
    Progress(LoadProgress),
    /// Strict mode encountered non-HL7 bytes; prompt the user (FO-11).
    NeedsNonHl7Confirmation {
        /// Paths being loaded.
        paths: Vec<PathBuf>,
        /// `true` when appending to the existing record list (`Ctrl+Shift+O`, `D-28`).
        append: bool,
    },
    /// A single `.zip` archive contains multiple entries; prompt the user to choose (FO-4, D-5).
    NeedsZipEntrySelection {
        /// Path to the `.zip` archive.
        zip_path: PathBuf,
        /// Available non-directory entry names.
        entries: Vec<String>,
        /// `true` when appending to the existing record list (`Ctrl+Shift+O`, `D-28`).
        append: bool,
    },
    /// Load completed.
    Completed {
        /// Loaded records.
        records: Vec<Record>,
        /// Per-record source file path metadata (`D-28`).
        record_sources: Vec<Option<PathBuf>>,
        /// Source paths.
        paths: Vec<PathBuf>,
        /// Whether loading was canceled.
        canceled: bool,
        /// `true` when appending to the existing record list (`Ctrl+Shift+O`, `D-28`).
        append: bool,
        /// When set, indicates a bounded memory-safe subset load (`Loaded first N of M messages`).
        truncated_from_total: Option<usize>,
    },
    /// Load failed with an error message.
    Failed(String),
}

/// Active background load job handle (FO-13, G-5).
pub struct ActiveLoadJob {
    /// Receiver for progress and completion messages.
    pub rx: Receiver<LoaderMessage>,
    /// Cancellation flag shared with the worker thread.
    pub cancel: Arc<AtomicBool>,
    /// Latest progress snapshot.
    pub progress: Option<LoadProgress>,
}

/// Preflight guardrail modal shown when opening file(s) projected to exceed the dynamic
/// system-aware safe memory budget (`Phase 1`).
#[derive(Debug, Clone)]
pub struct MemoryGuardrailDialog {
    /// Target file paths waiting for user confirmation.
    pub paths: Vec<PathBuf>,
    /// Recovery mode to use when proceeding.
    pub recovery_mode: RecoveryMode,
    /// Optional `.zip` entry selection override.
    pub zip_entries_override: Option<Vec<String>>,
    /// `true` when appending rather than replacing.
    pub append: bool,
    /// Total file size on disk in bytes.
    pub total_disk_bytes: u64,
    /// Estimated in-memory HL7 AST heap footprint in bytes.
    pub estimated_heap_bytes: u64,
    /// Live available physical system RAM in bytes at preflight time.
    pub available_ram_bytes: u64,
    /// Safe load budget (`max(30% of available RAM, 256 MiB)`).
    pub safe_budget_bytes: u64,
    /// Exact or estimated total message count across the selected files.
    pub estimated_total_messages: usize,
    /// Recommended safe subset count (`N`) that fits within `safe_budget_bytes`.
    pub safe_message_limit: usize,
}

/// Pending action gated on the unsaved-changes confirmation modal (FS-8).
#[derive(Debug, Clone)]
pub enum PendingUnsavedAction {
    /// Open specific file paths.
    OpenFiles(Vec<PathBuf>),
    /// Replace record list with clipboard contents.
    PasteReplace,
    /// Close the application window.
    ExitApp,
}

/// One frequency bucket row in the **Unique Values / Field Distribution** modal.
#[derive(Debug, Clone, PartialEq)]
pub struct UniqueValueRow {
    /// Raw field value (empty when `is_empty_missing == true`).
    pub value: String,
    /// `true` for the synthetic `(Empty / Missing)` bucket.
    pub is_empty_missing: bool,
    /// Number of loaded messages containing this value at the target HL7 path.
    pub count: usize,
    /// Percentage of loaded messages (`0.0..=100.0`).
    pub percent: f32,
}

/// Sort column for the **Unique Values / Field Distribution** modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UniqueValuesSortColumn {
    /// Sort alphabetically by value.
    Value,
    /// Sort numerically by message count (default).
    #[default]
    Count,
}

/// Modal state for **Show Unique Values (<PATH>)** from the Segment Accordion context menu.
#[derive(Debug, Clone)]
pub struct UniqueValuesModalState {
    /// Canonical HL7 path being analyzed (e.g. `"PID-3.1"` or `"PV1-2"`).
    pub path: String,
    /// Optional HL7 dictionary field/component description.
    pub description: Option<String>,
    /// Total loaded messages scanned.
    pub total_messages: usize,
    /// Frequency distribution rows.
    pub rows: Vec<UniqueValueRow>,
    /// Active sort column.
    pub sort_col: UniqueValuesSortColumn,
    /// `true` for descending order (default for Count).
    pub sort_desc: bool,
}

/// Default filter target path (`*` = Entire Message Content).
pub const DEFAULT_FILTER_PATH: &str = "*";

/// Maximum number of messages that can be compared simultaneously (1 Baseline + up to 4 Targets).
pub const MAX_COMPARE_MESSAGES: usize = 5;

/// One target message column in the **Compare HL7 Messages** window (compared against Baseline).
#[derive(Debug, Clone)]
pub struct CompareTargetEntry {
    /// Display label (e.g. `"Message #2 (ADT^A01 • CMP2)"`).
    pub label: String,
    /// Raw HL7 message text.
    pub raw: String,
    /// Optional 0-based document index when loaded from `SessionState::doc`.
    pub record_index: Option<usize>,
}

/// Modal state for the WinMerge-style side-by-side / multi-column **Message Compare** window.
#[derive(Debug, Clone)]
pub struct CompareMessagesModalState {
    /// Display label for the Left (baseline) pane.
    pub left_label: String,
    /// Display label for the primary Right (current) pane (kept in sync with `targets[0]`).
    pub right_label: String,
    /// Raw HL7 message text on the Left (baseline) side.
    pub left_raw: String,
    /// Raw HL7 message text on the primary Right (current) side (kept in sync with `targets[0]`).
    pub right_raw: String,
    /// Optional 0-based document index for the Left message (when loaded from `SessionState::doc`).
    pub left_record_index: Option<usize>,
    /// Optional 0-based document index for the primary Right message (kept in sync with `targets[0]`).
    pub right_record_index: Option<usize>,
    /// Target message columns compared against the Baseline (`1..=4` entries; `>= 2` renders multi-column grid).
    pub targets: Vec<CompareTargetEntry>,
    /// Whether the interactive **Baseline Input Zone** is active instead of the diff table.
    pub editing_baseline: bool,
    /// Editable multiline draft buffer for the Left (baseline) message.
    pub baseline_draft: String,
    /// One-shot flag to auto-focus the baseline multiline `TextEdit`.
    pub focus_baseline_input: bool,
    /// Active diff options (`ignore_volatile_ids_and_timestamps`, `differences_only`).
    pub options: DiffOptions,
    /// Computed 2-message side-by-side diff result (used in 2-column mode).
    pub diff: MessageDiffResult,
    /// Computed multi-message diff result (used when comparing 3..=5 messages against Baseline).
    pub multi_diff: MultiMessageDiffResult,
    /// 0-based cursor into the navigable difference rows (`0..active_total_diff_rows()`).
    pub active_diff_cursor: Option<usize>,
    /// One-shot flag telling the Compare modal to scroll vertically to `active_diff_cursor`.
    pub scroll_to_active_diff: bool,
    /// One-shot flag telling the Compare modal to reset scroll offset to top-left `(0.0, 0.0)`.
    pub reset_scroll_to_top_left: bool,
}

impl CompareMessagesModalState {
    /// Returns `true` when 2 or more target columns are active (3..=5 total messages).
    pub fn is_multi_target(&self) -> bool {
        self.targets.len() >= 2
    }

    /// Total navigable difference rows for the active mode (2-column or multi-column).
    pub fn active_total_diff_rows(&self) -> usize {
        if self.is_multi_target() {
            self.multi_diff.total_diff_rows
        } else {
            self.diff.total_diff_rows
        }
    }
}

/// Target mode for the deterministic PHI de-identification modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubPhiMode {
    /// Scrub a copy of the loaded messages and write atomically to a chosen output file
    /// without mutating the open document.
    ExportSynthetic,
    /// Scrub the currently loaded messages in place as a single undoable [`BatchEditRecordsCmd`].
    InPlaceLoaded,
}

/// Modal dialog state for `File -> Export Synthetic (Scrub PHI)...` and
/// `Edit -> Scrub PHI in Loaded Messages...`.
#[derive(Debug, Clone)]
pub struct ScrubPhiDialog {
    /// Whether to export synthetic messages to a new file or scrub loaded messages in place.
    pub mode: ScrubPhiMode,
    /// Optional user-supplied deterministic seed (blank = random per-run seed).
    pub seed_input: String,
    /// Quick-add HL7 path input for custom PHI rules (e.g. `"ZPI-4"`, `"PID-18.1"`, `"OBX-5"`).
    pub custom_path_input: String,
    /// Selected scrub mode for newly added custom PHI rules.
    pub custom_mode: hl7_core::CustomPhiMode,
    /// Inline validation error when an invalid HL7 path is submitted.
    pub custom_path_error: Option<String>,
}

impl ScrubPhiDialog {
    /// Create a fresh Scrub PHI dialog for `mode`.
    pub fn new(mode: ScrubPhiMode) -> Self {
        Self {
            mode,
            seed_input: String::new(),
            custom_path_input: String::new(),
            custom_mode: hl7_core::CustomPhiMode::Synthetic,
            custom_path_error: None,
        }
    }
}

/// Layout mode for the Unified Record Detail Area (`§5.1`, `D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailViewMode {
    /// Split view showing both the Raw Message Editor and the Expandable Segment Accordion.
    #[default]
    Split,
    /// Show only the In-Place Raw Message Editor.
    RawOnly,
    /// Show only the Expandable Segment Accordion & Aligned Field Breakdown.
    AccordionOnly,
}

/// Active tab in the Find & Replace panel (`Ctrl+F` / `Ctrl+H`, `D-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindReplaceTab {
    /// Find tab (`Ctrl+F`).
    #[default]
    Find,
    /// Replace tab (`Ctrl+H`).
    Replace,
}

/// Target scope for `Count` and `Replace All` in the Find & Replace panel (`D-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindReplaceScope {
    /// Only the currently selected record.
    CurrentRecord,
    /// Currently visible (filtered) records.
    FilteredRecords,
    /// All loaded records in the document.
    #[default]
    AllRecords,
}

/// Snapshot of the active record captured when entering inline Segment Accordion edit mode,
/// enabling live preview sync to the Raw Message Editor, exact `Escape` rollback, and a single
/// atomic `UndoStack` commit on `Enter` / focus loss.
#[derive(Debug, Clone)]
struct InlineFieldEditBaseline {
    record_index: usize,
    baseline_bytes: Vec<u8>,
    baseline_kind: RecordKind,
    baseline_delimiters: Delimiters,
    baseline_edited: bool,
}

/// Complete GUI session state.
pub struct SessionState {
    /// Active in-memory document (G-4).
    pub doc: Document,
    /// Cached one-line summaries for the virtualized record list (§5.1).
    pub summaries: Vec<RecordSummary>,
    /// Unlimited-depth undo/redo stack (EDIT-1).
    pub undo_stack: UndoStack,
    /// Origin of the current document.
    pub source: DataSource,
    /// Per-record source file path metadata parallel to `doc.records` (`D-28`).
    pub record_sources: Vec<Option<PathBuf>>,
    /// Built-in + user Z-segment dictionary (§5.4).
    pub dictionary: Hl7Dictionary,
    /// Primary selected record index.
    pub selected_record: Option<usize>,
    /// Multi-selected record indices (§5.1).
    pub selected_records: BTreeSet<usize>,
    /// Selected segment index within the selected record.
    pub selected_segment: Option<usize>,
    /// Selected field row index within `record_cache.field_rows`.
    pub selected_field_row: Option<usize>,
    /// Multi-selected field row indices within `record_cache.field_rows` (Segment Accordion).
    pub selected_field_rows: BTreeSet<usize>,
    /// Anchor row index for `Shift+Click` contiguous range selection in the Segment Accordion.
    pub field_selection_anchor: Option<usize>,
    /// Canonical HL7 [`FieldAddress`] of the currently selected field/component/subcomponent (`D-30`).
    pub selected_field_path: Option<FieldAddress>,
    /// One-shot flag telling the field pane to scroll to `selected_field_row`.
    pub scroll_to_field_row: bool,
    /// One-shot flag telling the segment pane to scroll to `selected_segment`.
    pub scroll_to_segment: bool,
    /// One-shot flag telling the Segment Accordion's outer `ScrollArea` to scroll to the
    /// active selection (set only from Raw Editor cursor sync / Find jumps, never from manual accordion clicks).
    pub scroll_accordion_to_selection: bool,
    /// Toggle pill in the Segment Accordion toolbar that reveals empty HL7 fields (including
    /// trailing standard dictionary fields) with muted `(empty)` placeholders for inline editing.
    pub show_empty_fields: bool,
    /// Active inline field edit row index inside `record_cache.field_rows` (Segment Accordion).
    pub editing_field_row: Option<usize>,
    /// Segment index of the active inline field edit.
    pub editing_field_seg_index: Option<usize>,
    /// Canonical HL7 [`FieldAddress`] of the active inline field edit.
    pub editing_field_path: Option<FieldAddress>,
    /// Staged edit buffer for the active inline field edit (mutated on keystrokes; committed on Enter/blur).
    pub editing_field_buffer: String,
    /// Snapshot of the original value when inline editing began (to avoid no-op undo entries).
    pub editing_field_original: String,
    /// `true` on the first frame after entering inline edit mode so `TextEdit` requests focus once.
    pub editing_field_just_opened: bool,
    /// Pre-edit record snapshot for live preview sync, exact `Escape` revert, and single undo commit.
    editing_field_baseline: Option<InlineFieldEditBaseline>,
    /// Cached breakdown of `selected_record`.
    pub record_cache: Option<SelectedRecordCache>,
    /// Active background file-loading job (G-5, FO-13).
    pub active_load: Option<ActiveLoadJob>,
    /// Paths waiting on the FO-11 non-HL7 recovery prompt (`(paths, append)`).
    pub pending_non_hl7_paths: Option<(Vec<PathBuf>, bool)>,
    /// Active large-file memory guardrail modal (`Phase 1`).
    pub memory_guardrail_dialog: Option<MemoryGuardrailDialog>,
    /// Zip archive waiting on entry selection (`(zip_path, entries, selected_flags, append)`).
    pub pending_zip_picker: Option<(PathBuf, Vec<String>, Vec<bool>, bool)>,
    /// Pending action waiting on the FS-8 unsaved-changes modal.
    pub pending_unsaved_action: Option<PendingUnsavedAction>,
    /// Transient status/error banner message shown in the status bar.
    pub status_message: String,
    /// Active Edit Record dialog state (EDIT-2).
    pub edit_record_dialog: Option<EditRecordDialog>,
    /// Active Edit Segment (single-segment raw editor) dialog state (`EDIT-2`, `EDIT-10`, `D-26`).
    pub edit_segment_dialog: Option<EditSegmentDialog>,
    /// Active Insert Segment (`Above` / `Below` / `AtEnd`) dialog state (`EDIT-10`, `D-26`).
    pub insert_segment_dialog: Option<InsertSegmentDialog>,
    /// Active Edit Field dialog state (EDIT-3, EDIT-4).
    pub edit_field_dialog: Option<EditFieldDialog>,
    /// Active Assign Value to Field dialog state (EDIT-5).
    pub assign_field_dialog: Option<AssignFieldDialog>,
    /// Active Replace dialog state (EDIT-6).
    pub replace_dialog: Option<ReplaceDialog>,
    /// Active Change Message Delimiters dialog state (EDIT-9).
    pub change_delimiters_dialog: Option<ChangeDelimitersDialog>,
    /// Active deterministic PHI scrubber / synthetic export modal dialog state.
    pub scrub_phi_dialog: Option<ScrubPhiDialog>,
    /// Active Unique Values / Field Distribution modal state.
    pub unique_values_modal: Option<UniqueValuesModalState>,
    /// Active WinMerge-style Side-by-Side Message Compare modal state.
    pub compare_modal: Option<CompareMessagesModalState>,
    /// Active Delete confirmation modal state (EDIT-7, EDIT-8).
    pub delete_confirm_dialog: Option<DeleteConfirmDialog>,
    /// Active modal notice/error alert message.
    pub modal_alert: Option<String>,
    /// Active soft undo memory warning banner/modal (EDIT-1, D-12).
    pub undo_warning: Option<UndoWarning>,
    /// `true` when the record list has been sorted (`SORT-1`: window title shows `"(sorted)"`).
    pub is_sorted: bool,
    /// Active "Sort Records by Field..." dialog state (`SORT-2`).
    pub sort_records_dialog: Option<SortRecordsDialog>,
    /// Interactive Report Builder and Sortable Report Table window state (`RPT-1..RPT-9`).
    pub report_window: ReportWindowState,
    /// MLLP Client (Sender) and Server (Listener) panel state (`NET-10..NET-34`).
    pub network: NetworkPanelState,
    /// Paths that contained 0 HL7 records, waiting on the FO-12 external editor prompt.
    pub pending_no_hl7_paths: Option<Vec<PathBuf>>,
    /// Whether the Preferences / Settings window is open (`FO-1`, `FO-2`, `G-6`).
    pub preferences_open: bool,
    /// Input field for adding a new file filter pattern in Preferences (`FO-1`, `FO-2`).
    pub new_filter_pattern_input: String,
    /// Inline error message in the Preferences window.
    pub preferences_error: Option<String>,
    /// Whether the Keyboard Shortcuts reference modal is open (`NFR-6`).
    pub shortcuts_help_open: bool,
    /// Whether the About `pipe_wrench` modal dialog is open.
    pub about_open: bool,
    /// Headless Directory Scan window state (`SCAN-1..SCAN-15`).
    pub scan_window: DirectoryScanWindowState,
    /// Layout mode of the Unified Record Detail Area (`D-26`).
    pub detail_view_mode: DetailViewMode,
    /// Height of the Raw Message Editor inside `DetailViewMode::Split` (`D-26`).
    pub detail_raw_split_height: f32,
    /// Editable text buffer for the in-place Raw Message Editor (`EDIT-2`, `D-26`).
    pub inline_raw_text: String,
    /// Last cursor byte position in `inline_raw_text` (`EDIT-2`, `D-26`).
    pub inline_raw_cursor: Option<usize>,
    /// Active uncommitted typing burst in the live Raw Message Editor for undo coalescing (`EDIT-2`).
    pub pending_inline_raw_edit: Option<PendingInlineRawEdit>,
    /// Expanded segment indices in the Segment Accordion (`D-26`).
    pub expanded_segments: BTreeSet<usize>,
    /// Anchor record index for `Shift+Click` range selection in the `Records` table (`D-26`).
    pub selection_anchor_record: Option<usize>,
    /// One-shot flag telling the `Records` table to scroll to the active `selected_record` row (`D-26`).
    pub scroll_to_record: bool,
    /// Whether the inline `Rec # / Total` numeric jump box is active in the `Records` pane (`D-26`).
    pub record_jump_active: bool,
    /// One-shot flag to focus the `Rec # / Total` inline numeric jump input (`D-26`).
    pub focus_record_jump_input: bool,
    /// Text buffer for the inline `Rec # / Total` numeric jump input (`1..=visible_count`) (`D-26`).
    pub record_jump_input: String,
    /// Whether the non-destructive Record List Filter bar is visible (`D-26`).
    pub filter_bar_open: bool,
    /// One-shot flag to focus the Filter bar query input and place the cursor at the end.
    pub focus_filter_input: bool,
    /// Whether the interactive `?` syntax help popover is open in the Filter bar (`D-28`).
    pub filter_help_open: bool,
    /// Whether the interactive `?` syntax help popover is open in the Find & Replace panel (`D-28`).
    pub find_help_open: bool,
    /// Unified Path / Expression input (`PID-3(*).1` with Operator+Value, or inline `PID-3(*).1=123`, `MSH-7>=20260101`, `ROL-4 exists`, or plain text) (`D-26`).
    pub filter_query: String,
    /// Unified Filter bar operator dropdown (`Equals`, `NotEquals`, `Contains`, `Regex`, `GreaterThan`, `GreaterOrEqual`, `LessThan`, `LessOrEqual`, `Exists`).
    pub filter_builder_op: FilterBuilderOp,
    /// Unified Filter bar value input (hidden/unused when `Exists` is selected or when a full inline expression is typed).
    pub filter_builder_value: String,
    /// Stacked multi-filter rules rendered as removable chips in the Filter bar.
    pub active_filters: Vec<ActiveFilterRule>,
    /// Combination mode (`AND` vs. `OR`) when multiple filter rules are active.
    pub filter_combine_mode: FilterCombineMode,
    /// Case-sensitive toggle for the Record List Filter bar (`D-26`).
    pub filter_case_sensitive: bool,
    /// Regex toggle for the Record List Filter bar (`D-26`).
    pub filter_use_regex: bool,
    /// Invert match (`-v`) toggle for the Record List Filter bar (`D-26`).
    pub filter_invert_match: bool,
    /// Inline parse error for `filter_query`, if any.
    pub filter_error: Option<String>,
    /// Non-destructively filtered 0-based document record indices (`None` when no filter is active) (`D-26`).
    pub filtered_record_indices: Option<Vec<usize>>,
    /// Whether the Find & Replace panel (`Ctrl+F` / `Ctrl+H`) is visible (`D-26`, `D-27`).
    pub search_bar_open: bool,
    /// Active tab (`Find` or `Replace`) in the Find & Replace panel (`D-27`).
    pub find_replace_tab: FindReplaceTab,
    /// Target scope (`CurrentRecord`, `FilteredRecords`, `AllRecords`) for `Count` and `Replace All` (`D-27`).
    pub find_replace_scope: FindReplaceScope,
    /// One-shot flag to focus the `Find what` input box when toggled via `Ctrl+F` / `Ctrl+H` / `F3`.
    pub focus_search_input: bool,
    /// Current `Find what` query in the Find & Replace panel (`D-26`, `D-27`).
    pub search_query: String,
    /// Current `Replace with` text in the Replace tab (`D-27`).
    pub replace_with_input: String,
    /// Optional canonical Field / Component / Subfield Path restriction (e.g. `"PID-3.1.4"`, `"OBX-5"`) (`D-26`, `D-27`).
    pub search_field_input: String,
    /// Case-sensitive (`Aa`) toggle in the Find & Replace panel (`D-26`, `D-27`).
    pub search_case_sensitive: bool,
    /// Regular expression (`.*`) toggle in the Find & Replace panel (`D-26`, `D-27`).
    pub search_use_regex: bool,
    /// Match whole value (when field path is set) or whole word (unrestricted) toggle (`D-27`).
    pub search_match_whole: bool,
    /// Wrap around toggle (default `true`) (`D-27`).
    pub search_wrap_around: bool,
    /// Recent `Find what` history (most recent first, max 15) (`D-27`).
    pub find_history: Vec<String>,
    /// Recent `Replace with` history (most recent first, max 15) (`D-27`).
    pub replace_history: Vec<String>,
    /// Status banner for Replace / Replace All operations (`D-27`).
    pub replace_status_banner: String,
    /// Whether the expandable Hit Breakdown popover/panel is open in the Find & Replace panel (`D-26`).
    pub search_breakdown_open: bool,
    /// Signature that produced `search_matches`.
    pub last_searched_query: String,
    /// All `(record_index, segment_index, Option<FieldAddress>)` matches for `last_searched_query` (`D-26`).
    pub search_matches: Vec<SearchMatch>,
    /// Hit count breakdown grouped by canonical field path for `search_matches` (`D-26`).
    pub search_field_breakdown: Vec<FieldHitCount>,
    /// Active index into `search_matches`.
    pub search_match_cursor: Option<usize>,
    /// Status readout for Jump-to-Next Search (e.g. `"Match 1 of 4"` or `"Wrapped to start"`).
    pub search_status: String,
    /// Current 0-based page index in the bounded `Records` list (`§5.1`, `NFR-1`).
    pub record_page_index: usize,
    /// Session override flag set when the user clicks `Load All` in the paged `Records` list (`§5.1`).
    pub load_all_pages_override: bool,
    /// Active source document index while dragging a row in the `Records` list (`D-29`).
    pub dragged_record_index: Option<usize>,
    /// Monotonic counter for newly inserted scaffold record control IDs (`NEW0001`, `NEW0002`, ...) (`D-29`).
    new_record_counter: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// Initialize an empty session with the built-in HL7 v2 dictionary.
    pub fn new() -> Self {
        Self {
            doc: Document::new(),
            summaries: Vec::new(),
            undo_stack: UndoStack::default(),
            source: DataSource::Empty,
            record_sources: Vec::new(),
            dictionary: Hl7Dictionary::builtin(),
            selected_record: None,
            selected_records: BTreeSet::new(),
            selected_segment: None,
            selected_field_row: None,
            selected_field_rows: BTreeSet::new(),
            field_selection_anchor: None,
            selected_field_path: None,
            scroll_to_field_row: false,
            scroll_to_segment: false,
            scroll_accordion_to_selection: false,
            show_empty_fields: false,
            editing_field_row: None,
            editing_field_seg_index: None,
            editing_field_path: None,
            editing_field_buffer: String::new(),
            editing_field_original: String::new(),
            editing_field_just_opened: false,
            editing_field_baseline: None,
            record_cache: None,
            active_load: None,
            pending_non_hl7_paths: None,
            pending_zip_picker: None,
            pending_unsaved_action: None,
            memory_guardrail_dialog: None,
            status_message: "Ready".to_string(),
            edit_record_dialog: None,
            edit_segment_dialog: None,
            insert_segment_dialog: None,
            edit_field_dialog: None,
            assign_field_dialog: None,
            replace_dialog: None,
            change_delimiters_dialog: None,
            scrub_phi_dialog: None,
            unique_values_modal: None,
            compare_modal: None,
            delete_confirm_dialog: None,
            modal_alert: None,
            undo_warning: None,
            is_sorted: false,
            sort_records_dialog: None,
            report_window: ReportWindowState::default(),
            network: NetworkPanelState::default(),
            pending_no_hl7_paths: None,
            preferences_open: false,
            new_filter_pattern_input: String::new(),
            preferences_error: None,
            shortcuts_help_open: false,
            about_open: false,
            scan_window: DirectoryScanWindowState::default(),
            detail_view_mode: DetailViewMode::Split,
            detail_raw_split_height: 190.0,
            inline_raw_text: String::new(),
            inline_raw_cursor: None,
            pending_inline_raw_edit: None,
            expanded_segments: BTreeSet::new(),
            selection_anchor_record: None,
            scroll_to_record: false,
            record_jump_active: false,
            focus_record_jump_input: false,
            record_jump_input: String::new(),
            filter_bar_open: false,
            focus_filter_input: false,
            filter_help_open: false,
            find_help_open: false,
            filter_query: DEFAULT_FILTER_PATH.to_string(),
            filter_builder_op: FilterBuilderOp::Contains,
            filter_builder_value: String::new(),
            active_filters: Vec::new(),
            filter_combine_mode: FilterCombineMode::And,
            filter_case_sensitive: false,
            filter_use_regex: false,
            filter_invert_match: false,
            filter_error: None,
            filtered_record_indices: None,
            search_bar_open: false,
            find_replace_tab: FindReplaceTab::Find,
            find_replace_scope: FindReplaceScope::AllRecords,
            focus_search_input: false,
            search_query: String::new(),
            replace_with_input: String::new(),
            search_field_input: String::new(),
            search_case_sensitive: false,
            search_use_regex: false,
            search_match_whole: false,
            search_wrap_around: true,
            find_history: Vec::new(),
            replace_history: Vec::new(),
            replace_status_banner: String::new(),
            search_breakdown_open: false,
            last_searched_query: String::new(),
            search_matches: Vec::new(),
            search_field_breakdown: Vec::new(),
            search_match_cursor: None,
            search_status: String::new(),
            record_page_index: 0,
            load_all_pages_override: false,
            dragged_record_index: None,
            new_record_counter: 1,
        }
    }

    /// Replace the document with `records` from `source`, rebuilding summaries and selecting record 0.
    pub fn set_records(
        &mut self,
        records: Vec<Record>,
        source: DataSource,
        settings: &AppSettings,
    ) {
        let default_src = source.single_file_path().cloned();
        let rec_sources = vec![default_src; records.len()];
        self.set_records_with_sources(records, rec_sources, source, settings);
    }

    /// Replace the document with `records` and per-record source file metadata (`D-28`).
    pub fn set_records_with_sources(
        &mut self,
        records: Vec<Record>,
        record_sources: Vec<Option<PathBuf>>,
        source: DataSource,
        settings: &AppSettings,
    ) {
        self.summaries = records.iter().map(Record::summary).collect();
        self.doc = Document::from_records(records);
        self.record_sources = record_sources;
        self.undo_stack.clear();
        self.undo_stack.mark_saved();
        self.source = source;
        self.selected_records.clear();
        self.selection_anchor_record = None;
        self.is_sorted = false;
        self.record_page_index = 0;
        self.load_all_pages_override = false;
        self.pending_inline_raw_edit = None;
        self.recompute_filter_if_active();
        self.invalidate_search();

        if self.doc.is_empty() {
            self.selected_record = None;
            self.selected_segment = None;
            self.selected_field_row = None;
            self.record_cache = None;
            self.inline_raw_text.clear();
            self.expanded_segments.clear();
        } else {
            let first = self
                .filtered_record_indices
                .as_ref()
                .and_then(|v| v.first().copied())
                .unwrap_or(0);
            self.select_record(first, false, settings);
        }
    }

    /// Append `records` to the current document (e.g. Paste Append FO-15 or TCP/IP NET-13).
    pub fn append_records(
        &mut self,
        records: Vec<Record>,
        source: DataSource,
        settings: &AppSettings,
    ) {
        let default_src = source.single_file_path().cloned();
        let rec_sources = vec![default_src; records.len()];
        self.append_records_with_sources(records, rec_sources, source, settings);
    }

    /// Append `records` with per-record source file metadata, merging multi-file `DataSource::Files` lists (`D-28`).
    pub fn append_records_with_sources(
        &mut self,
        records: Vec<Record>,
        record_sources: Vec<Option<PathBuf>>,
        source: DataSource,
        settings: &AppSettings,
    ) {
        let was_empty = self.doc.is_empty();
        for rec in records {
            self.summaries.push(rec.summary());
            self.doc.records.push(rec);
        }
        self.record_sources.extend(record_sources);
        match (&mut self.source, source) {
            (DataSource::Files(existing), DataSource::Files(new_paths)) => {
                for p in new_paths {
                    if !existing.contains(&p) {
                        existing.push(p);
                    }
                }
            }
            (slot, new_src) => {
                *slot = new_src;
            }
        }
        self.recompute_filter_if_active();
        self.invalidate_search();
        if was_empty && !self.doc.is_empty() {
            self.select_record(0, false, settings);
        }
    }

    /// Synchronously load one or more files (sorted lexicographically), replacing the current document
    /// and preserving per-record source file metadata (`FO-3`, `D-28`).
    pub fn load_files_sync(
        &mut self,
        paths: &[PathBuf],
        recovery_mode: RecoveryMode,
        settings: &AppSettings,
    ) -> Result<usize, String> {
        let (records, rec_sources, sorted_paths) =
            load_files_with_per_record_sources(paths, recovery_mode, settings)?;
        let has_legacy_bytes = records
            .iter()
            .any(|r| std::str::from_utf8(r.bytes()).is_err());
        let count = records.len();
        self.set_records_with_sources(
            records,
            rec_sources,
            DataSource::Files(sorted_paths),
            settings,
        );
        self.status_message = if has_legacy_bytes {
            format!(
                "Loaded {count} message(s) (non-UTF-8 bytes decoded with Windows-1252/Latin-1 fallback)."
            )
        } else {
            format!("Loaded {count} message(s).")
        };
        Ok(count)
    }

    /// Synchronously append one or more files (`File -> Append File(s)...`, `Ctrl+Shift+O`)
    /// without clearing existing messages, preserving per-record source file metadata (`D-28`).
    pub fn append_files_sync(
        &mut self,
        paths: &[PathBuf],
        recovery_mode: RecoveryMode,
        settings: &AppSettings,
    ) -> Result<usize, String> {
        let (records, rec_sources, sorted_paths) =
            load_files_with_per_record_sources(paths, recovery_mode, settings)?;
        let count = records.len();
        self.append_records_with_sources(
            records,
            rec_sources,
            DataSource::Files(sorted_paths),
            settings,
        );
        self.status_message = format!("Appended {count} message(s) ({} total).", self.doc.len());
        Ok(count)
    }

    /// Select a record by index (updating or replacing multi-select) and rebuild the pane cache.
    pub fn select_record(&mut self, index: usize, toggle_multi: bool, settings: &AppSettings) {
        self.select_record_with_modifiers(index, toggle_multi, false, settings);
    }

    /// Select a record by index supporting single-click, `Ctrl+Click` toggle, and `Shift+Click` range (`§5.1`, `D-26`).
    pub fn select_record_with_modifiers(
        &mut self,
        index: usize,
        ctrl_toggle: bool,
        shift_range: bool,
        settings: &AppSettings,
    ) {
        if index >= self.doc.len() {
            return;
        }
        if self.selected_record != Some(index) {
            let _ = self.flush_pending_inline_raw_edit(settings);
        }
        if shift_range {
            let anchor = self
                .selection_anchor_record
                .or(self.selected_record)
                .unwrap_or(index);
            let lo = anchor.min(index);
            let hi = anchor.max(index);
            self.selected_records.clear();
            match &self.filtered_record_indices {
                Some(visible) => {
                    for &doc_idx in visible {
                        if doc_idx >= lo && doc_idx <= hi {
                            self.selected_records.insert(doc_idx);
                        }
                    }
                }
                None => {
                    for doc_idx in lo..=hi {
                        self.selected_records.insert(doc_idx);
                    }
                }
            }
            self.selected_records.insert(index);
        } else if ctrl_toggle {
            if !self.selected_records.remove(&index) {
                self.selected_records.insert(index);
            }
            self.selection_anchor_record = Some(index);
        } else {
            self.selected_records.clear();
            self.selected_records.insert(index);
            self.selection_anchor_record = Some(index);
        }
        if let Some(ref cache) = self.record_cache {
            if cache.record_index != index {
                self.cancel_inline_field_edit();
            }
        } else {
            self.cancel_inline_field_edit();
        }
        self.selected_record = Some(index);
        self.selected_segment = Some(0);
        self.selected_field_row = None;
        self.selected_field_rows.clear();
        self.field_selection_anchor = None;
        self.sync_record_page_to_selected(settings);
        self.rebuild_selected_record_cache(settings);
    }

    /// Select a segment in the active record, expanding it in the accordion and scrolling to its first field (§5.1).
    pub fn select_segment(&mut self, seg_index: usize) {
        self.selected_segment = Some(seg_index);
        self.expanded_segments.insert(seg_index);
        if let Some(ref cache) = self.record_cache {
            if let Some(seg_row) = cache.segments.get(seg_index) {
                if !seg_row.field_rows_range.is_empty() {
                    self.selected_field_row = Some(seg_row.field_rows_range.start);
                    self.scroll_to_field_row = true;
                }
            }
        }
    }

    /// Toggle expansion of `seg_index` in the Segment Accordion (`D-26`).
    pub fn toggle_segment_accordion(&mut self, seg_index: usize) {
        self.selected_segment = Some(seg_index);
        if !self.expanded_segments.remove(&seg_index) {
            self.expanded_segments.insert(seg_index);
        }
    }

    /// Expand all segments in the active record's Segment Accordion (`D-26`).
    pub fn expand_all_segments(&mut self) {
        if let Some(ref cache) = self.record_cache {
            self.expanded_segments = (0..cache.segments.len()).collect();
        }
    }

    /// Collapse all segments in the active record's Segment Accordion (`D-26`).
    pub fn collapse_all_segments(&mut self) {
        self.expanded_segments.clear();
    }

    /// Select a field row in the field list directly from the Segment Accordion (§5.1).
    ///
    /// Does **not** set `scroll_accordion_to_selection` so manual browsing in the accordion never jumps.
    pub fn select_field_row(&mut self, row_index: usize) {
        if let Some(ref cache) = self.record_cache {
            if let Some(frow) = cache.field_rows.get(row_index) {
                self.selected_field_row = Some(row_index);
                self.selected_field_rows.clear();
                self.selected_field_rows.insert(row_index);
                self.field_selection_anchor = Some(row_index);
                self.selected_field_path = frow.address;
                self.selected_segment = Some(frow.seg_index);
                self.expanded_segments.insert(frow.seg_index);
                self.scroll_to_segment = true;
            }
        }
    }

    /// Select a field/component/subcomponent row in the Segment Accordion honoring standard
    /// desktop `Ctrl+Click` (toggle) and `Shift+Click` (contiguous visible range) modifiers.
    pub fn select_field_row_with_modifiers(
        &mut self,
        row_index: usize,
        ctrl_toggle: bool,
        shift_range: bool,
    ) {
        if !ctrl_toggle && !shift_range {
            self.select_field_row(row_index);
            return;
        }
        let Some(ref cache) = self.record_cache else {
            return;
        };
        let Some(frow) = cache.field_rows.get(row_index) else {
            return;
        };
        let seg_index = frow.seg_index;
        let addr = frow.address;

        if shift_range {
            let visible_rows: Vec<usize> = cache
                .segments
                .iter()
                .filter(|seg| self.expanded_segments.contains(&seg.seg_index))
                .flat_map(|seg| seg.field_rows_range.clone())
                .collect();
            let anchor = self
                .field_selection_anchor
                .or(self.selected_field_row)
                .unwrap_or(row_index);
            if let (Some(a_pos), Some(b_pos)) = (
                visible_rows.iter().position(|&r| r == anchor),
                visible_rows.iter().position(|&r| r == row_index),
            ) {
                let (lo, hi) = if a_pos <= b_pos {
                    (a_pos, b_pos)
                } else {
                    (b_pos, a_pos)
                };
                if !ctrl_toggle {
                    self.selected_field_rows.clear();
                }
                for &r in &visible_rows[lo..=hi] {
                    self.selected_field_rows.insert(r);
                }
            } else {
                self.selected_field_rows.insert(row_index);
            }
            self.selected_field_row = Some(row_index);
            self.selected_field_path = addr;
            self.selected_segment = Some(seg_index);
        } else if ctrl_toggle {
            if self.selected_field_rows.contains(&row_index) {
                self.selected_field_rows.remove(&row_index);
                let next_primary = self.selected_field_rows.iter().next_back().copied();
                self.selected_field_row = next_primary;
                self.selected_field_path = next_primary
                    .and_then(|r| cache.field_rows.get(r))
                    .and_then(|f| f.address);
            } else {
                self.selected_field_rows.insert(row_index);
                self.selected_field_row = Some(row_index);
                self.selected_field_path = addr;
                self.selected_segment = Some(seg_index);
                self.field_selection_anchor = Some(row_index);
            }
        }
    }

    /// Return all currently selected Segment Accordion rows in display order.
    pub fn selected_accordion_field_rows(&self) -> Vec<&FieldViewRow> {
        let Some(ref cache) = self.record_cache else {
            return Vec::new();
        };
        if !self.selected_field_rows.is_empty() {
            self.selected_field_rows
                .iter()
                .filter_map(|&idx| cache.field_rows.get(idx))
                .collect()
        } else if let Some(idx) = self.selected_field_row {
            cache.field_rows.get(idx).into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Format selected Segment Accordion row values for clipboard copy (newline-separated).
    pub fn format_accordion_copy_values(&self) -> String {
        self.selected_accordion_field_rows()
            .into_iter()
            .map(|r| r.display_value.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Format selected Segment Accordion HL7 paths for clipboard copy (newline-separated).
    pub fn format_accordion_copy_paths(&self) -> String {
        self.selected_accordion_field_rows()
            .into_iter()
            .filter_map(|r| r.address.map(|a| a.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Format selected Segment Accordion rows as `<PATH> (<Name>): <Value>` lines.
    pub fn format_accordion_copy_summary(&self) -> String {
        self.selected_accordion_field_rows()
            .into_iter()
            .map(format_field_row_summary_line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Filter by all currently selected Segment Accordion rows (`PATH = VALUE` rules), either
    /// replacing active filters (`append = false`) or appending to them (`append = true`).
    pub fn filter_by_selected_accordion_rows(&mut self, append: bool, settings: &AppSettings) {
        let pairs: Vec<(String, String)> = self
            .selected_accordion_field_rows()
            .into_iter()
            .filter_map(|r| r.address.map(|a| (a.to_string(), r.display_value.clone())))
            .collect();
        if pairs.is_empty() {
            return;
        }
        self.filter_bar_open = true;
        self.filter_help_open = false;
        self.filter_use_regex = false;
        if append {
            self.promote_input_query_to_rule_if_needed();
        } else {
            self.active_filters.clear();
            self.filter_query.clear();
        }
        for (path, val) in pairs {
            let expr = format_filter_by_path_query(&path, &val);
            let rule = ActiveFilterRule {
                query: expr,
                case_sensitive: self.filter_case_sensitive,
                use_regex: false,
                invert_match: false,
            };
            if !self.active_filters.contains(&rule) {
                self.active_filters.push(rule);
            }
        }
        self.filter_query.clear();
        self.focus_filter_input = true;
        self.apply_filter(settings);
    }

    /// Open the Report Builder pre-populated with all currently selected Segment Accordion paths.
    pub fn open_report_from_selected_accordion_fields(&mut self) {
        let addrs: Vec<FieldAddress> = self
            .selected_accordion_field_rows()
            .into_iter()
            .filter_map(|r| r.address)
            .collect();
        if addrs.is_empty() {
            self.open_report_from_selected_field();
        } else {
            self.report_window
                .open_with_fields(&addrs, &self.doc.records, &self.dictionary);
        }
    }

    /// Synchronize selection from a byte offset in the Raw Message Editor using the active message's
    /// delimiters (`SEG-F[(R)][.C[.S]]`), auto-expanding the target segment and highlighting/scrolling
    /// the matching leaf row in the Segment Accordion without scrolling the Raw Editor (`§5.1`, `D-30`).
    pub fn select_from_raw_offset(&mut self, byte_offset: usize) {
        let Some(rec_idx) = self.selected_record else {
            return;
        };
        let delims = self
            .doc
            .records
            .get(rec_idx)
            .map_or(Delimiters::STANDARD, |r| r.delimiters());

        let resolved = {
            let raw_src = if !self.inline_raw_text.is_empty() {
                self.inline_raw_text.as_str()
            } else if let Some(ref cache) = self.record_cache {
                cache.raw_text.as_str()
            } else {
                return;
            };
            crate::ui::detail_pane::resolve_raw_cursor_location(raw_src, byte_offset, delims)
        };

        let Some((seg_idx, resolved_addr)) = resolved else {
            return;
        };
        let Some(ref cache) = self.record_cache else {
            return;
        };
        if seg_idx >= cache.segments.len() {
            return;
        }

        self.selected_segment = Some(seg_idx);
        self.expanded_segments.insert(seg_idx);
        self.selected_field_path = resolved_addr;
        self.scroll_to_segment = true;
        self.scroll_accordion_to_selection = true;

        // Find the exact leaf FieldViewRow matching `resolved_addr` (falling back to parent field or segment's first row).
        let matched_row_idx = if let Some(addr) = resolved_addr {
            cache
                .field_rows
                .iter()
                .find(|f| !f.is_parsed_marker && f.seg_index == seg_idx && f.address == Some(addr))
                .or_else(|| {
                    let parent = FieldAddress {
                        segment: addr.segment,
                        field: addr.field,
                        repetition: addr.repetition,
                        component: addr.component,
                        subcomponent: None,
                    };
                    cache.field_rows.iter().find(|f| {
                        !f.is_parsed_marker && f.seg_index == seg_idx && f.address == Some(parent)
                    })
                })
                .or_else(|| {
                    let base = FieldAddress {
                        segment: addr.segment,
                        field: addr.field,
                        repetition: None,
                        component: None,
                        subcomponent: None,
                    };
                    cache.field_rows.iter().find(|f| {
                        !f.is_parsed_marker && f.seg_index == seg_idx && f.address == Some(base)
                    })
                })
                .map(|f| f.row_index)
        } else {
            None
        };

        self.selected_field_row = matched_row_idx;
        self.scroll_to_field_row = matched_row_idx.is_some();
    }

    /// Rebuild the cached segment, field, and raw text breakdown for `selected_record` (NFR-1).
    pub fn rebuild_selected_record_cache(&mut self, settings: &AppSettings) {
        self.rebuild_selected_record_cache_internal(settings, true);
    }

    /// Toggle the Segment Accordion `Show Empty` pill and rebuild the active record cache.
    pub fn set_show_empty_fields(&mut self, show: bool, settings: &AppSettings) {
        if self.show_empty_fields != show {
            self.show_empty_fields = show;
            self.rebuild_selected_record_cache_internal(settings, false);
        }
    }

    /// Rebuild the cached segment, field, and raw text breakdown for `selected_record`,
    /// optionally preserving the active `inline_raw_text` buffer and cursor position
    /// during live auto-apply typing (`EDIT-2`, `NFR-1`).
    fn rebuild_selected_record_cache_internal(
        &mut self,
        settings: &AppSettings,
        reset_inline_editor: bool,
    ) {
        let Some(rec_idx) = self.selected_record else {
            self.record_cache = None;
            return;
        };
        let Some(rec) = self.doc.records.get(rec_idx) else {
            self.record_cache = None;
            return;
        };

        let prior_seg_count = self.record_cache.as_ref().map_or(0, |c| c.segments.len());
        let charset = rec.charset();
        let delims = rec.delimiters();
        let show_empty = settings.display_empty_fields || self.show_empty_fields;
        let field_opts = FieldDisplayOptions {
            display_empty_fields: show_empty,
            display_compound_fields: settings.display_compound_fields,
        };

        let seg_index = rec.segments();
        let mut segments = Vec::with_capacity(seg_index.len());
        let mut field_rows = Vec::new();
        let mut raw_text = String::with_capacity(rec.bytes().len() + seg_index.len());

        for (i, span) in seg_index.iter().enumerate() {
            if i > 0 {
                raw_text.push('\n');
            }
            let seg_raw_start = raw_text.len();
            let seg_bytes = rec.segment_bytes(i).unwrap_or(&[]);
            let seg_decoded = charset.decode_display(seg_bytes);
            raw_text.push_str(&seg_decoded);
            let seg_raw_end = raw_text.len();

            let tag_str = String::from_utf8_lossy(&span.tag).into_owned();
            let seg_desc = if settings.display_segment_descriptions {
                self.dictionary
                    .segment_description(&span.tag)
                    .map(str::to_string)
            } else {
                None
            };

            let field_start_idx = field_rows.len();
            let entries: Vec<FieldEntry> = rec.field_entries(i, field_opts);

            for entry in entries {
                let row_index = field_rows.len();
                let desc = if settings.display_field_descriptions {
                    entry
                        .address
                        .as_ref()
                        .and_then(|addr| self.dictionary.field_description(addr))
                        .map(str::to_string)
                } else {
                    None
                };

                let value_bytes = if settings.decode_escape_sequences
                    && !entry.is_parsed_marker
                    && entry.address.is_none_or(|a| !a.is_header_delimiter_field())
                {
                    delims.decode_escapes(&entry.raw_value)
                } else {
                    entry.raw_value.clone()
                };
                let display_value = charset.decode_display(&value_bytes).into_owned();

                let raw_range = locate_field_in_segment_text(
                    &raw_text[seg_raw_start..seg_raw_end],
                    seg_raw_start,
                    entry.address.as_ref(),
                    delims,
                )
                .unwrap_or(seg_raw_start..seg_raw_end);

                let tooltip = entry
                    .address
                    .as_ref()
                    .and_then(|addr| self.dictionary.field_tooltip(addr));

                let attachment = if entry.is_parsed_marker {
                    None
                } else {
                    detect_attachment_for_entry(rec, i, entry.address, &display_value, delims)
                        .map(|a| a.summary())
                };

                field_rows.push(FieldViewRow {
                    row_index,
                    seg_index: i,
                    address: entry.address,
                    description: desc,
                    tooltip,
                    display_value,
                    is_parsed_marker: entry.is_parsed_marker,
                    is_editable: entry.is_editable && !settings.decode_escape_sequences,
                    raw_text_range: raw_range,
                    attachment,
                });
            }

            if self.show_empty_fields {
                let present_fields: BTreeSet<u16> = field_rows[field_start_idx..]
                    .iter()
                    .filter_map(|r| r.address.map(|a| a.field.get()))
                    .collect();
                for fnum in self.dictionary.segment_field_numbers(&span.tag) {
                    if present_fields.contains(&fnum) {
                        continue;
                    }
                    let Some(nz_field) = std::num::NonZeroU16::new(fnum) else {
                        continue;
                    };
                    let addr = FieldAddress {
                        segment: span.tag,
                        field: nz_field,
                        repetition: None,
                        component: None,
                        subcomponent: None,
                    };
                    let row_index = field_rows.len();
                    let desc = if settings.display_field_descriptions {
                        self.dictionary.field_description(&addr).map(str::to_string)
                    } else {
                        None
                    };
                    let tooltip = self.dictionary.field_tooltip(&addr);
                    field_rows.push(FieldViewRow {
                        row_index,
                        seg_index: i,
                        address: Some(addr),
                        description: desc,
                        tooltip,
                        display_value: String::new(),
                        is_parsed_marker: false,
                        is_editable: !addr.is_header_delimiter_field()
                            && !settings.decode_escape_sequences,
                        raw_text_range: seg_raw_start..seg_raw_end,
                        attachment: None,
                    });
                }
            }

            let field_end_idx = field_rows.len();
            segments.push(SegmentViewRow {
                seg_index: i,
                tag: tag_str,
                occurrence: span.occurrence,
                description: seg_desc,
                preview: seg_decoded.into_owned(),
                raw_text_range: seg_raw_start..seg_raw_end,
                field_rows_range: field_start_idx..field_end_idx,
            });
        }

        if reset_inline_editor {
            self.inline_raw_text = raw_text.clone();
            self.inline_raw_cursor = None;
            // Expand all segments by default when switching records.
            self.expanded_segments = (0..segments.len()).collect();
        } else {
            self.expanded_segments.retain(|&idx| idx < segments.len());
            for idx in prior_seg_count..segments.len() {
                self.expanded_segments.insert(idx);
            }
            if let Some(sel_seg) = self.selected_segment {
                if sel_seg >= segments.len() {
                    self.selected_segment = segments.len().checked_sub(1);
                }
            }
        }

        self.record_cache = Some(SelectedRecordCache {
            record_index: rec_idx,
            charset,
            missing_start_block: rec.flags().missing_start_block,
            segments,
            field_rows,
            raw_text,
        });
    }

    /// Launch a background worker thread to load `paths` without blocking the UI thread (G-5, FO-13).
    pub fn start_background_load(
        &mut self,
        paths: Vec<PathBuf>,
        recovery_mode: RecoveryMode,
        zip_entries_override: Option<Vec<String>>,
        settings: &AppSettings,
        ctx: egui::Context,
    ) {
        self.start_background_load_with_mode(
            paths,
            recovery_mode,
            zip_entries_override,
            false,
            settings,
            ctx,
        );
    }

    /// Preflight-check `paths` against `budget_override` (or live `MemoryBudget::from_system()`):
    /// - Within budget: launches background loading immediately with zero extra prompts.
    /// - Exceeds budget: pauses full parsing and opens `memory_guardrail_dialog` so the user can
    ///   choose `Load Safe Subset (First N Messages)`, `Load All Anyway`, or `Cancel`.
    #[allow(clippy::too_many_arguments)]
    pub fn request_open_files_with_guardrail(
        &mut self,
        paths: Vec<PathBuf>,
        recovery_mode: RecoveryMode,
        zip_entries_override: Option<Vec<String>>,
        append: bool,
        settings: &AppSettings,
        ctx: egui::Context,
        budget_override: Option<MemoryBudget>,
    ) {
        if paths.is_empty() {
            return;
        }
        let budget = budget_override.unwrap_or_else(MemoryBudget::from_system);
        let config = ScannerConfig {
            envelope: settings.mllp_envelope(),
            recovery_mode,
            ..ScannerConfig::default()
        };
        let (total_disk_bytes, est_messages, _is_exact) = estimate_file_set_metrics(&paths, config);
        if budget.exceeds_budget(total_disk_bytes) {
            let total_est = est_messages.max(1);
            let safe_limit = budget.estimate_safe_message_limit(total_disk_bytes, total_est);
            self.memory_guardrail_dialog = Some(MemoryGuardrailDialog {
                paths,
                recovery_mode,
                zip_entries_override,
                append,
                total_disk_bytes,
                estimated_heap_bytes: budget.estimated_heap_bytes(total_disk_bytes),
                available_ram_bytes: budget.available_ram_bytes,
                safe_budget_bytes: budget.safe_load_budget_bytes,
                estimated_total_messages: total_est,
                safe_message_limit: safe_limit,
            });
            return;
        }
        self.start_background_load_with_options(
            paths,
            recovery_mode,
            zip_entries_override,
            append,
            None,
            None,
            settings,
            ctx,
        );
    }

    /// Proceed with the active [`MemoryGuardrailDialog`] by loading only the first `N` safe messages.
    pub fn confirm_memory_guardrail_safe_subset(
        &mut self,
        settings: &AppSettings,
        ctx: egui::Context,
    ) {
        let Some(dlg) = self.memory_guardrail_dialog.take() else {
            return;
        };
        self.start_background_load_with_options(
            dlg.paths,
            dlg.recovery_mode,
            dlg.zip_entries_override,
            dlg.append,
            Some(dlg.safe_message_limit),
            Some(dlg.estimated_total_messages),
            settings,
            ctx,
        );
    }

    /// Proceed with the active [`MemoryGuardrailDialog`] by loading all messages despite the warning.
    pub fn confirm_memory_guardrail_load_all(
        &mut self,
        settings: &AppSettings,
        ctx: egui::Context,
    ) {
        let Some(dlg) = self.memory_guardrail_dialog.take() else {
            return;
        };
        self.start_background_load_with_options(
            dlg.paths,
            dlg.recovery_mode,
            dlg.zip_entries_override,
            dlg.append,
            None,
            None,
            settings,
            ctx,
        );
    }

    /// Abort the active [`MemoryGuardrailDialog`] cleanly without allocating.
    pub fn cancel_memory_guardrail(&mut self) {
        self.memory_guardrail_dialog = None;
        self.status_message = "Large file load canceled.".to_string();
    }

    /// Launch a background worker thread to load or append `paths` without blocking the UI thread (`G-5`, `FO-13`, `D-28`).
    pub fn start_background_load_with_mode(
        &mut self,
        paths: Vec<PathBuf>,
        recovery_mode: RecoveryMode,
        zip_entries_override: Option<Vec<String>>,
        append: bool,
        settings: &AppSettings,
        ctx: egui::Context,
    ) {
        self.start_background_load_with_options(
            paths,
            recovery_mode,
            zip_entries_override,
            append,
            None,
            None,
            settings,
            ctx,
        );
    }

    /// Launch a background worker thread to load or append `paths` with an optional `max_messages` cap.
    #[allow(clippy::too_many_arguments)]
    pub fn start_background_load_with_options(
        &mut self,
        paths: Vec<PathBuf>,
        recovery_mode: RecoveryMode,
        zip_entries_override: Option<Vec<String>>,
        append: bool,
        max_messages: Option<usize>,
        total_estimated_messages: Option<usize>,
        settings: &AppSettings,
        ctx: egui::Context,
    ) {
        if paths.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let config = ScannerConfig {
            envelope: settings.mllp_envelope(),
            recovery_mode,
            ..ScannerConfig::default()
        };

        std::thread::spawn(move || {
            for p in &paths {
                if let Err(msg) = validate_file_not_binary(p) {
                    let _ = tx.send(LoaderMessage::Failed(msg));
                    ctx.request_repaint();
                    return;
                }
            }
            // Single .zip with multiple entries prompts user first (FO-4, D-5).
            if paths.len() == 1 && zip_entries_override.is_none() {
                let p = &paths[0];
                if p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                {
                    if let Ok(entries) = list_zip_entries(p) {
                        if entries.len() > 1 {
                            let _ = tx.send(LoaderMessage::NeedsZipEntrySelection {
                                zip_path: p.clone(),
                                entries,
                                append,
                            });
                            ctx.request_repaint();
                            return;
                        }
                    }
                }
            }

            if let (1, Some(chosen_entries)) = (paths.len(), zip_entries_override) {
                let p = &paths[0];
                match load_zip_entries_with_limit(
                    p,
                    Some(&chosen_entries),
                    config,
                    &cancel_worker,
                    max_messages,
                ) {
                    Ok(out) => {
                        if out.aborted_on_non_hl7 {
                            let _ =
                                tx.send(LoaderMessage::NeedsNonHl7Confirmation { paths, append });
                        } else {
                            let rec_sources = vec![Some(p.clone()); out.records.len()];
                            let truncated = max_messages
                                .filter(|&limit| out.records.len() >= limit)
                                .and(total_estimated_messages);
                            let _ = tx.send(LoaderMessage::Completed {
                                records: out.records,
                                record_sources: rec_sources,
                                paths,
                                canceled: cancel_worker.load(Ordering::Relaxed),
                                append,
                                truncated_from_total: truncated,
                            });
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(LoaderMessage::Failed(e.to_string()));
                    }
                }
                ctx.request_repaint();
                return;
            }

            // Sort paths lexicographically (FO-3) and load file-by-file so per-record source paths are preserved.
            let mut sorted_paths = paths.clone();
            sorted_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then_with(|| a.cmp(b)));

            let total_files = sorted_paths.len();
            let mut all_records = Vec::new();
            let mut all_sources = Vec::new();
            let mut bytes_read = 0u64;
            let mut aborted_non_hl7 = false;
            let mut canceled = false;

            for (idx, path) in sorted_paths.iter().enumerate() {
                if cancel_worker.load(Ordering::Relaxed) {
                    canceled = true;
                    break;
                }
                let remaining = match max_messages {
                    Some(limit) => {
                        let rem = limit.saturating_sub(all_records.len());
                        if rem == 0 {
                            break;
                        }
                        Some(rem)
                    }
                    None => None,
                };
                match load_file_with_limit(path, config, &cancel_worker, remaining) {
                    Ok(out) => {
                        bytes_read += out.summary.bytes_read;
                        if out.aborted_on_non_hl7 {
                            aborted_non_hl7 = true;
                            break;
                        }
                        let n = out.records.len();
                        all_records.extend(out.records);
                        all_sources.extend(std::iter::repeat_n(Some(path.clone()), n));
                        let _ = tx.send(LoaderMessage::Progress(LoadProgress {
                            files_done: idx + 1,
                            total_files,
                            bytes_read,
                        }));
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        let _ = tx.send(LoaderMessage::Failed(e.to_string()));
                        ctx.request_repaint();
                        return;
                    }
                }
            }

            if aborted_non_hl7 {
                let _ = tx.send(LoaderMessage::NeedsNonHl7Confirmation {
                    paths: sorted_paths,
                    append,
                });
            } else {
                let truncated = max_messages
                    .filter(|&limit| all_records.len() >= limit)
                    .and(total_estimated_messages);
                let _ = tx.send(LoaderMessage::Completed {
                    records: all_records,
                    record_sources: all_sources,
                    paths: sorted_paths,
                    canceled: canceled || cancel_worker.load(Ordering::Relaxed),
                    append,
                    truncated_from_total: truncated,
                });
            }
            ctx.request_repaint();
        });

        self.active_load = Some(ActiveLoadJob {
            rx,
            cancel,
            progress: None,
        });
        self.status_message = if append {
            "Appending file(s)...".to_string()
        } else {
            "Loading file(s)...".to_string()
        };
    }

    /// Poll the active background loader channel, updating state without blocking (G-5).
    pub fn poll_background_load(&mut self, settings: &AppSettings) {
        let mut finished = false;
        let mut next_msg = None;

        if let Some(ref mut job) = self.active_load {
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    LoaderMessage::Progress(p) => {
                        job.progress = Some(p);
                    }
                    other => {
                        next_msg = Some(other);
                        finished = true;
                        break;
                    }
                }
            }
        }

        if finished {
            self.active_load = None;
        }

        if let Some(msg) = next_msg {
            match msg {
                LoaderMessage::Progress(_) => {}
                LoaderMessage::NeedsNonHl7Confirmation { paths, append } => {
                    self.pending_non_hl7_paths = Some((paths, append));
                    self.status_message = strings::PROMPT_NON_HL7_RECOVERY.to_string();
                }
                LoaderMessage::NeedsZipEntrySelection {
                    zip_path,
                    entries,
                    append,
                } => {
                    let flags = vec![true; entries.len()];
                    self.pending_zip_picker = Some((zip_path, entries, flags, append));
                }
                LoaderMessage::Completed {
                    records,
                    record_sources,
                    paths,
                    canceled,
                    append,
                    truncated_from_total,
                } => {
                    if canceled {
                        self.status_message = "File load canceled.".to_string();
                    } else if records.is_empty() {
                        self.status_message = strings::MSG_NO_HL7_RECORDS_FOUND.to_string();
                        self.pending_no_hl7_paths = Some(paths);
                    } else if append {
                        let count = records.len();
                        let file_count = paths.len();
                        self.append_records_with_sources(
                            records,
                            record_sources,
                            DataSource::Files(paths),
                            settings,
                        );
                        self.status_message = if let Some(total_est) = truncated_from_total {
                            format!(
                                "Loaded first {count} of {total_est} messages to protect system RAM ({} total).",
                                self.doc.len()
                            )
                        } else {
                            format!(
                                "Appended {count} message(s) from {file_count} file(s) ({} total).",
                                self.doc.len()
                            )
                        };
                        tracing::info!(count, file_count, "Appended HL7 messages");
                    } else {
                        let count = records.len();
                        let file_count = paths.len();
                        self.set_records_with_sources(
                            records,
                            record_sources,
                            DataSource::Files(paths),
                            settings,
                        );
                        self.status_message = if let Some(total_est) = truncated_from_total {
                            format!(
                                "Loaded first {count} of {total_est} messages to protect system RAM."
                            )
                        } else if file_count > 1 {
                            format!("Loaded {count} message(s) from {file_count} files.")
                        } else {
                            format!("Loaded {count} message(s).")
                        };
                        tracing::info!(count, file_count, "Loaded HL7 messages");
                    }
                }
                LoaderMessage::Failed(err) => {
                    self.status_message = format!("Load error: {err}");
                }
            }
        }
    }

    /// Paste HL7 text from clipboard, either replacing or appending (FO-15).
    pub fn paste_from_text(&mut self, text: &str, append: bool, settings: &AppSettings) {
        if let Some(reason) = detect_binary_payload(text.as_bytes(), None) {
            self.status_message = reason.to_string();
            return;
        }
        let sanitized = sanitize_pasted_hl7_text(text);
        if sanitized.trim().is_empty()
            || !has_valid_hl7_header(&sanitized, settings.mllp_envelope())
        {
            self.status_message =
                "No valid HL7 messages found in clipboard text (expected MSH, FHS, or BHS header)."
                    .to_string();
            return;
        }
        let config = ScannerConfig {
            envelope: settings.mllp_envelope(),
            recovery_mode: RecoveryMode::RecoverNonHl7,
            ..ScannerConfig::default()
        };
        match load_from_bytes(sanitized.as_bytes(), config) {
            Ok(out) => {
                if out.records.is_empty() {
                    self.status_message = strings::MSG_NO_HL7_RECORDS_FOUND.to_string();
                    return;
                }
                let count = out.records.len();
                if append {
                    self.append_records(out.records, DataSource::Clipboard, settings);
                    self.status_message = format!("Appended {count} record(s) from clipboard.");
                } else {
                    self.set_records(out.records, DataSource::Clipboard, settings);
                    self.status_message = format!("Pasted {count} record(s) from clipboard.");
                }
            }
            Err(e) => {
                self.status_message = format!("Clipboard parse error: {e}");
            }
        }
    }

    /// Handle a clipboard read outcome (including non-text/image clipboard errors such as
    /// `ContentNotAvailable`) and ingest valid HL7 messages with Undo support.
    pub fn paste_from_clipboard_result(
        &mut self,
        clipboard_result: Result<String, &str>,
        replace_list: bool,
        settings: &AppSettings,
    ) -> Result<usize, String> {
        let text = match clipboard_result {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) | Err(_) => {
                let msg = "Clipboard does not contain text. Copy HL7 message text (starting with MSH, FHS, or BHS) and try again."
                    .to_string();
                self.status_message = msg.clone();
                return Err(msg);
            }
        };
        if replace_list {
            if let Some(reason) = detect_binary_payload(text.as_bytes(), None) {
                let msg = reason.to_string();
                self.status_message = msg.clone();
                return Err(msg);
            }
            let sanitized = sanitize_pasted_hl7_text(&text);
            if !has_valid_hl7_header(&sanitized, settings.mllp_envelope()) {
                let msg = "No valid HL7 messages found in clipboard text (expected MSH, FHS, or BHS header)."
                    .to_string();
                self.status_message = msg.clone();
                return Err(msg);
            }
            self.paste_from_text(&sanitized, false, settings);
            Ok(self.doc.len())
        } else {
            self.paste_records_undoable(&text, settings)
        }
    }

    /// Global `Ctrl+V` paste of raw HL7 v2 record(s) from clipboard outside text inputs (`D-29`).
    ///
    /// Parses single messages, multi-message batches, `FHS`/`BHS` headers, MLLP-framed text, and
    /// `\r` / `\n` / `\r\n` terminators. Inserts after the active selection (or at `0` if empty),
    /// selects the first newly pasted record, and pushes an atomic [`InsertRecordsCmd`] onto
    /// [`UndoStack`] so `Ctrl+Z` cleanly removes the pasted records.
    pub fn paste_records_undoable(
        &mut self,
        text: &str,
        settings: &AppSettings,
    ) -> Result<usize, String> {
        if let Some(reason) = detect_binary_payload(text.as_bytes(), None) {
            let msg = reason.to_string();
            self.status_message = msg.clone();
            return Err(msg);
        }

        let sanitized = sanitize_pasted_hl7_text(text);
        if sanitized.trim().is_empty() {
            let msg = "Clipboard does not contain text. Copy HL7 message text (starting with MSH, FHS, or BHS) and try again."
                .to_string();
            self.status_message = msg.clone();
            return Err(msg);
        }

        if !has_valid_hl7_header(&sanitized, settings.mllp_envelope()) {
            let msg =
                "No valid HL7 messages found in clipboard text (expected MSH, FHS, or BHS header)."
                    .to_string();
            self.status_message = msg.clone();
            return Err(msg);
        }

        let config = ScannerConfig {
            envelope: settings.mllp_envelope(),
            recovery_mode: RecoveryMode::RecoverNonHl7,
            ..ScannerConfig::default()
        };
        let out = load_from_bytes(sanitized.as_bytes(), config).map_err(|e| {
            let msg = format!("No valid HL7 messages found in clipboard text: {e}");
            self.status_message = msg.clone();
            msg
        })?;

        let has_valid_header = out.records.iter().any(|r| {
            matches!(
                r.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            )
        });
        if out.records.is_empty() || !has_valid_header {
            let msg =
                "No valid HL7 messages found in clipboard text (expected MSH, FHS, or BHS header)."
                    .to_string();
            self.status_message = msg.clone();
            return Err(msg);
        }

        let count = out.records.len();
        let insert_idx = if self.doc.is_empty() {
            0
        } else {
            self.selected_record
                .map_or(self.doc.len(), |sel| (sel + 1).min(self.doc.len()))
        };

        let cmd = Box::new(InsertRecordsCmd::new(
            insert_idx,
            out.records,
            format!("Paste {count} Message(s) from Clipboard"),
        ));
        self.execute_command(cmd, settings)
            .map_err(|e| e.to_string())?;

        if self.source == DataSource::Empty {
            self.source = DataSource::Clipboard;
        }
        self.record_sources.resize(self.doc.len(), None);
        self.selected_records.clear();
        self.selected_records.insert(insert_idx);
        self.selected_record = Some(insert_idx);
        self.selection_anchor_record = Some(insert_idx);
        self.scroll_to_record = true;
        self.status_message = format!("Pasted {count} message(s) from clipboard.");
        Ok(count)
    }

    /// Insert a new minimal valid HL7 v2 scaffold record (`+` button) immediately below the active
    /// selection (or at index `0` if empty), pushing an atomic [`InsertRecordsCmd`] onto [`UndoStack`] (`D-29`).
    pub fn insert_new_scaffold_record(
        &mut self,
        settings: &AppSettings,
    ) -> Result<usize, EditError> {
        let ts = current_hl7_timestamp();
        let ctrl_id = format!("NEW{:04}", self.new_record_counter);
        self.new_record_counter = self.new_record_counter.saturating_add(1);
        let raw_line = format!("MSH|^~\\&|||||{ts}||ADT^A01|{ctrl_id}|P|2.5.1");
        let rec = Record::from_normalized_bytes(
            raw_line.into_bytes(),
            RecordKind::Msh,
            Delimiters::STANDARD,
            SourceTerminator::Cr,
            RecordFlags::default(),
        );

        let insert_idx = if self.doc.is_empty() {
            0
        } else {
            self.selected_record
                .map_or(self.doc.len(), |sel| (sel + 1).min(self.doc.len()))
        };

        let cmd = Box::new(InsertRecordsCmd::new(
            insert_idx,
            vec![rec],
            format!("Insert Message #{}", insert_idx + 1),
        ));
        self.execute_command(cmd, settings)?;
        self.record_sources.resize(self.doc.len(), None);
        self.selected_records.clear();
        self.selected_records.insert(insert_idx);
        self.selected_record = Some(insert_idx);
        self.selection_anchor_record = Some(insert_idx);
        self.scroll_to_record = true;
        self.status_message = format!("Inserted new message #{}.", insert_idx + 1);
        Ok(insert_idx)
    }

    /// Return `true` when selected record(s) can be moved up (`↑`) by one position (`D-29`).
    pub fn can_move_selected_records_up(&self) -> bool {
        if self.filtered_record_indices.is_some() || self.doc.len() <= 1 {
            return false;
        }
        self.selected_records
            .first()
            .copied()
            .or(self.selected_record)
            .is_some_and(|first| first > 0)
    }

    /// Return `true` when selected record(s) can be moved down (`↓`) by one position (`D-29`).
    pub fn can_move_selected_records_down(&self) -> bool {
        if self.filtered_record_indices.is_some() || self.doc.len() <= 1 {
            return false;
        }
        self.selected_records
            .last()
            .copied()
            .or(self.selected_record)
            .is_some_and(|last| last + 1 < self.doc.len())
    }

    /// Move the currently selected record(s) up (`↑`) by one position via [`ReorderRecordsCmd`] (`D-29`).
    pub fn move_selected_records_up(&mut self, settings: &AppSettings) -> Result<bool, EditError> {
        if !self.can_move_selected_records_up() {
            return Ok(false);
        }
        let n = self.doc.len();
        let selected: BTreeSet<usize> = if !self.selected_records.is_empty() {
            self.selected_records.clone()
        } else if let Some(sel) = self.selected_record {
            std::iter::once(sel).collect()
        } else {
            return Ok(false);
        };

        let mut perm: Vec<usize> = (0..n).collect();
        for i in 1..n {
            if selected.contains(&perm[i]) && !selected.contains(&perm[i - 1]) {
                perm.swap(i - 1, i);
            }
        }

        let new_selected: BTreeSet<usize> = perm
            .iter()
            .enumerate()
            .filter_map(|(new_idx, orig_idx)| selected.contains(orig_idx).then_some(new_idx))
            .collect();
        let old_primary = self.selected_record;
        let new_primary = old_primary
            .and_then(|op| perm.iter().position(|&orig| orig == op))
            .or_else(|| new_selected.first().copied());

        let cmd = Box::new(ReorderRecordsCmd::new(
            format!("Move {} Message(s) Up", selected.len()),
            perm,
            new_primary,
            old_primary,
        ));
        self.execute_command(cmd, settings)?;
        self.selected_records = new_selected;
        self.selected_record = new_primary;
        self.selection_anchor_record = new_primary;
        self.scroll_to_record = true;
        Ok(true)
    }

    /// Move the currently selected record(s) down (`↓`) by one position via [`ReorderRecordsCmd`] (`D-29`).
    pub fn move_selected_records_down(
        &mut self,
        settings: &AppSettings,
    ) -> Result<bool, EditError> {
        if !self.can_move_selected_records_down() {
            return Ok(false);
        }
        let n = self.doc.len();
        let selected: BTreeSet<usize> = if !self.selected_records.is_empty() {
            self.selected_records.clone()
        } else if let Some(sel) = self.selected_record {
            std::iter::once(sel).collect()
        } else {
            return Ok(false);
        };

        let mut perm: Vec<usize> = (0..n).collect();
        for i in (0..n - 1).rev() {
            if selected.contains(&perm[i]) && !selected.contains(&perm[i + 1]) {
                perm.swap(i, i + 1);
            }
        }

        let new_selected: BTreeSet<usize> = perm
            .iter()
            .enumerate()
            .filter_map(|(new_idx, orig_idx)| selected.contains(orig_idx).then_some(new_idx))
            .collect();
        let old_primary = self.selected_record;
        let new_primary = old_primary
            .and_then(|op| perm.iter().position(|&orig| orig == op))
            .or_else(|| new_selected.first().copied());

        let cmd = Box::new(ReorderRecordsCmd::new(
            format!("Move {} Message(s) Down", selected.len()),
            perm,
            new_primary,
            old_primary,
        ));
        self.execute_command(cmd, settings)?;
        self.selected_records = new_selected;
        self.selected_record = new_primary;
        self.selection_anchor_record = new_primary;
        self.scroll_to_record = true;
        Ok(true)
    }

    /// Drag-and-drop reorder one or more messages via [`ReorderRecordsCmd`] (`D-29`).
    ///
    /// When `from_idx` is part of a multi-row selection (`selected_records.len() > 1`), all
    /// selected rows move together in their relative order to the insertion slot relative to
    /// `to_idx` (above `to_idx` when dragging upward, below `to_idx` when dragging downward)
    /// and remain selected at their new contiguous positions.
    pub fn reorder_record_drag(
        &mut self,
        from_idx: usize,
        to_idx: usize,
        settings: &AppSettings,
    ) -> Result<bool, EditError> {
        let n = self.doc.len();
        if self.filtered_record_indices.is_some()
            || from_idx >= n
            || to_idx >= n
            || from_idx == to_idx
        {
            return Ok(false);
        }

        let moving: Vec<usize> =
            if self.selected_records.contains(&from_idx) && self.selected_records.len() > 1 {
                self.selected_records
                    .iter()
                    .copied()
                    .filter(|&i| i < n)
                    .collect()
            } else {
                vec![from_idx]
            };
        let moving_set: BTreeSet<usize> = moving.iter().copied().collect();

        // Insertion slot in 0..=n: above `to_idx` when dragging upward, below `to_idx` when dragging downward.
        let raw_slot = if from_idx > to_idx {
            to_idx
        } else {
            to_idx + 1
        };
        let unselected: Vec<usize> = (0..n).filter(|i| !moving_set.contains(i)).collect();
        let unselected_before = (0..raw_slot)
            .filter(|i| !moving_set.contains(i))
            .count()
            .min(unselected.len());

        let mut perm = Vec::with_capacity(n);
        perm.extend_from_slice(&unselected[..unselected_before]);
        perm.extend_from_slice(&moving);
        perm.extend_from_slice(&unselected[unselected_before..]);

        if perm.iter().enumerate().all(|(i, &orig)| i == orig) {
            return Ok(false);
        }

        let new_selected: BTreeSet<usize> =
            (unselected_before..(unselected_before + moving.len())).collect();
        let old_primary = self.selected_record;
        let new_primary = perm
            .iter()
            .position(|&orig| orig == from_idx)
            .or(Some(unselected_before));
        let desc = if moving.len() == 1 {
            format!("Move Message #{} to #{}", from_idx + 1, to_idx + 1)
        } else {
            format!("Move {} Messages", moving.len())
        };

        let cmd = Box::new(ReorderRecordsCmd::new(desc, perm, new_primary, old_primary));
        self.execute_command(cmd, settings)?;
        self.selected_records = new_selected;
        self.selected_record = new_primary;
        self.selection_anchor_record = new_primary;
        self.scroll_to_record = true;
        Ok(true)
    }

    /// Save all or selected records atomically to `path` (FS-1, FS-4..FS-7).
    pub fn save_to_path(
        &mut self,
        path: PathBuf,
        selected_only: bool,
        settings: &AppSettings,
    ) -> anyhow::Result<()> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let opts = SaveOptions {
            term_mode: settings.save_terminator.into(),
            disk_envelope: settings
                .disk_envelope_on_save
                .then(|| settings.mllp_envelope()),
        };
        if selected_only {
            let indices: Vec<usize> = self.selected_records.iter().copied().collect();
            save_selected_records_atomic(&path, &self.doc.records, &indices, opts)?;
            self.status_message = format!(
                "Saved {} selected record(s) to {}",
                indices.len(),
                path.display()
            );
        } else {
            save_records_atomic(&path, &self.doc.records, opts)?;
            self.undo_stack.mark_saved();
            self.source = DataSource::Files(vec![path.clone()]);
            self.status_message =
                format!("Saved {} record(s) to {}", self.doc.len(), path.display());
        }
        Ok(())
    }

    /// Export only the currently matched filtered records atomically to `path` without modifying
    /// the open document, undo stack, or active source path.
    pub fn export_filtered_to_path(
        &mut self,
        path: PathBuf,
        settings: &AppSettings,
    ) -> anyhow::Result<usize> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let Some(indices) = self.filtered_record_indices.clone() else {
            anyhow::bail!("No active filter to export.");
        };
        let opts = SaveOptions {
            term_mode: settings.save_terminator.into(),
            disk_envelope: settings
                .disk_envelope_on_save
                .then(|| settings.mllp_envelope()),
        };
        save_selected_records_atomic(&path, &self.doc.records, &indices, opts)?;
        let count = indices.len();
        self.status_message = format!("Exported {count} filtered message(s) to {}", path.display());
        Ok(count)
    }

    /// Deterministically scrub PHI across all loaded records in place as a single atomic,
    /// undoable [`BatchEditRecordsCmd`] (`Ctrl+Z` restores original messages).
    ///
    /// Returns `(modified_record_count, used_seed)`.
    pub fn scrub_loaded_records_undoable(
        &mut self,
        seed: Option<&str>,
        settings: &AppSettings,
    ) -> Result<(usize, String), String> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        if self.doc.is_empty() {
            return Err("No messages loaded to scrub.".to_string());
        }
        let scrubber = match seed.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => PhiScrubber::new(s),
            None => PhiScrubber::with_random_seed(),
        }
        .with_extra_paths(settings.custom_phi_rules.clone());
        let used_seed = scrubber.seed().to_string();
        let scrubbed = scrubber.scrub_records(&self.doc.records);

        let mut updates: Vec<(usize, Vec<u8>)> = Vec::new();
        for (idx, (orig, next)) in self.doc.records.iter().zip(scrubbed).enumerate() {
            if orig.bytes() != next.bytes() {
                updates.push((idx, next.bytes().to_vec()));
            }
        }
        let modified = updates.len();
        if modified == 0 {
            self.status_message = format!("No PHI fields required scrubbing (seed: {used_seed}).");
            return Ok((0, used_seed));
        }

        let selection_hint = self.selected_record.map(|record_index| SelectionAnchor {
            record_index,
            segment_index: self.selected_segment,
            address: self.selected_field_path,
        });
        let cmd = Box::new(BatchEditRecordsCmd::new(
            format!("Scrub PHI ({modified} message(s))"),
            updates,
            modified,
            selection_hint,
        ));
        self.execute_command(cmd, settings)
            .map_err(|e| e.to_string())?;
        self.status_message = format!(
            "Scrubbed PHI in {modified} message(s) (seed: {used_seed}). Press Ctrl+Z to undo."
        );
        Ok((modified, used_seed))
    }

    /// Deterministically scrub PHI across a copy of all loaded records and write them atomically
    /// to `path` without modifying the open document, undo stack, or active source path.
    ///
    /// Returns `(exported_record_count, used_seed)`.
    pub fn export_synthetic_to_path(
        &mut self,
        path: PathBuf,
        seed: Option<&str>,
        settings: &AppSettings,
    ) -> Result<(usize, String), String> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        if self.doc.is_empty() {
            return Err("No messages loaded to export.".to_string());
        }
        let scrubber = match seed.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => PhiScrubber::new(s),
            None => PhiScrubber::with_random_seed(),
        }
        .with_extra_paths(settings.custom_phi_rules.clone());
        let used_seed = scrubber.seed().to_string();
        let scrubbed = scrubber.scrub_records(&self.doc.records);
        let opts = SaveOptions {
            term_mode: settings.save_terminator.into(),
            disk_envelope: settings
                .disk_envelope_on_save
                .then(|| settings.mllp_envelope()),
        };
        save_records_atomic(&path, &scrubbed, opts).map_err(|e| e.to_string())?;
        let count = scrubbed.len();
        self.status_message = format!(
            "Exported {count} synthetic message(s) to {} (seed: {used_seed}).",
            path.display()
        );
        Ok((count, used_seed))
    }

    /// Collect distinct non-empty `Z*` / custom segment field paths from the currently selected
    /// message for the Scrub PHI modal's one-click quick-pick dropdown.
    pub fn detected_custom_z_paths_in_selected(&self) -> Vec<String> {
        let Some(ref cache) = self.record_cache else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for frow in &cache.field_rows {
            if frow.display_value.trim().is_empty() {
                continue;
            }
            let Some(addr) = frow.address else {
                continue;
            };
            if addr.segment[0] == b'Z' && addr.component.is_none() {
                let p = addr.to_string();
                if seen.insert(p.clone()) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Decode a detected Base64 / HL7 `ED` attachment for Segment Accordion `row_index`,
    /// returning `(attachment, suggested_filename)` (e.g. `MSG0001_OBX-5.pdf`).
    pub fn extract_attachment_for_field_row(
        &self,
        row_index: usize,
    ) -> Option<(DetectedAttachment, String)> {
        let cache = self.record_cache.as_ref()?;
        let frow = cache.field_rows.get(row_index)?;
        let rec = self.doc.records.get(cache.record_index)?;
        let delims = rec.delimiters();
        let att = detect_attachment_for_entry(
            rec,
            frow.seg_index,
            frow.address,
            &frow.display_value,
            delims,
        )?;
        let control_id = self
            .summaries
            .get(cache.record_index)
            .map(|s| s.control_id.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("MSG");
        let path_label = match frow.address {
            Some(addr) if att.is_ed_format && addr.component.is_some() => {
                // Use the parent field path (e.g. `OBX-5`) for ED.5 component rows
                let tag = String::from_utf8_lossy(&addr.segment);
                format!("{tag}-{}", addr.field.get())
            }
            Some(addr) => addr.to_string(),
            None => "ATTACHMENT".to_string(),
        };
        let suggested = att.suggested_filename(control_id, &path_label);
        Some((att, suggested))
    }

    /// Decode and write a Base64 / HL7 `ED` attachment from Segment Accordion `row_index` to `path`.
    pub fn save_attachment_for_row_to_path(
        &mut self,
        row_index: usize,
        path: PathBuf,
    ) -> Result<usize, String> {
        let Some((att, _)) = self.extract_attachment_for_field_row(row_index) else {
            return Err("Selected field does not contain a valid Base64 / ED payload.".to_string());
        };
        std::fs::write(&path, &att.bytes)
            .map_err(|e| format!("Failed to write attachment '{}': {e}", path.display()))?;
        let len = att.bytes.len();
        self.status_message = format!(
            "Saved {} attachment ({}) to {}.",
            att.label,
            format_byte_size(len),
            path.display()
        );
        Ok(len)
    }

    /// Scan all loaded messages for `path` and open the **Unique Values / Field Distribution** modal.
    pub fn open_unique_values_for_path(&mut self, path: &str) {
        let trimmed = path.trim();
        let Ok(addr) = trimmed.parse::<FieldAddress>() else {
            self.status_message = format!("Invalid HL7 path for Unique Values: '{trimmed}'");
            return;
        };
        let total_messages = self.doc.records.len();
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut empty_missing_count = 0usize;

        for rec in &self.doc.records {
            let charset = rec.charset();
            let mut found_in_msg: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for (seg_idx, span) in rec.segments().iter().enumerate() {
                if span.tag != addr.segment {
                    continue;
                }
                if let Some(raw_bytes) = rec.field_bytes(seg_idx, &addr) {
                    let val = charset.decode_display(raw_bytes).trim().to_string();
                    if !val.is_empty() {
                        found_in_msg.insert(val);
                    }
                }
            }
            if found_in_msg.is_empty() {
                empty_missing_count += 1;
            } else {
                for v in found_in_msg {
                    *counts.entry(v).or_insert(0) += 1;
                }
            }
        }

        let denom = total_messages.max(1) as f32;
        let mut rows: Vec<UniqueValueRow> = counts
            .into_iter()
            .map(|(value, count)| UniqueValueRow {
                value,
                is_empty_missing: false,
                count,
                percent: (count as f32 / denom) * 100.0,
            })
            .collect();
        if empty_missing_count > 0 {
            rows.push(UniqueValueRow {
                value: String::new(),
                is_empty_missing: true,
                count: empty_missing_count,
                percent: (empty_missing_count as f32 / denom) * 100.0,
            });
        }

        let description = self.dictionary.field_description(&addr).map(str::to_string);
        self.unique_values_modal = Some(UniqueValuesModalState {
            path: trimmed.to_string(),
            description,
            total_messages,
            rows,
            sort_col: UniqueValuesSortColumn::Count,
            sort_desc: true,
        });
        self.apply_unique_values_sort();
    }

    /// Toggle or switch the active sort column in the **Unique Values** modal.
    pub fn sort_unique_values(&mut self, col: UniqueValuesSortColumn) {
        if let Some(ref mut modal) = self.unique_values_modal {
            if modal.sort_col == col {
                modal.sort_desc = !modal.sort_desc;
            } else {
                modal.sort_col = col;
                modal.sort_desc = col == UniqueValuesSortColumn::Count;
            }
        }
        self.apply_unique_values_sort();
    }

    fn apply_unique_values_sort(&mut self) {
        let Some(ref mut modal) = self.unique_values_modal else {
            return;
        };
        let col = modal.sort_col;
        let desc = modal.sort_desc;
        modal.rows.sort_by(|a, b| {
            let ord = match col {
                UniqueValuesSortColumn::Count => {
                    a.count.cmp(&b.count).then_with(|| a.value.cmp(&b.value))
                }
                UniqueValuesSortColumn::Value => a
                    .is_empty_missing
                    .cmp(&b.is_empty_missing)
                    .then_with(|| {
                        a.value
                            .to_ascii_lowercase()
                            .cmp(&b.value.to_ascii_lowercase())
                    })
                    .then_with(|| a.count.cmp(&b.count)),
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }

    /// Format the active **Unique Values** distribution table as TSV for clipboard export.
    pub fn format_unique_values_table(&self) -> String {
        let Some(ref modal) = self.unique_values_modal else {
            return String::new();
        };
        let mut out = format!("Value\tCount\t% of Messages ({})\n", modal.path);
        for row in &modal.rows {
            let label = if row.is_empty_missing {
                "(Empty / Missing)"
            } else {
                row.value.as_str()
            };
            out.push_str(&format!("{label}\t{}\t{:.1}%\n", row.count, row.percent));
        }
        out
    }

    /// Append an exclusion filter (`<PATH> != "<VALUE>"`) and apply immediately.
    pub fn exclude_filter_for_path(&mut self, path: &str, value: &str, settings: &AppSettings) {
        let trimmed_path = path.trim();
        let trimmed_val = value.trim();
        if trimmed_path.is_empty() || trimmed_val.is_empty() {
            return;
        }
        let expr = if trimmed_val.contains(char::is_whitespace) {
            format!("{trimmed_path} != \"{trimmed_val}\"")
        } else {
            format!("{trimmed_path} != {trimmed_val}")
        };
        self.filter_bar_open = true;
        self.filter_help_open = false;
        self.filter_use_regex = false;
        self.promote_input_query_to_rule_if_needed();
        let rule = ActiveFilterRule {
            query: expr,
            case_sensitive: self.filter_case_sensitive,
            use_regex: false,
            invert_match: false,
        };
        if !self.active_filters.contains(&rule) {
            self.active_filters.push(rule);
        }
        self.filter_query = DEFAULT_FILTER_PATH.to_string();
        self.apply_filter(settings);
    }

    /// Apply/append a presence filter (`<PATH> exists`) to show messages where `path` is non-empty.
    pub fn filter_present_for_path(&mut self, path: &str, settings: &AppSettings) {
        let trimmed_path = path.trim();
        if trimmed_path.is_empty() {
            return;
        }
        let expr = format!("{trimmed_path} exists");
        self.filter_bar_open = true;
        self.filter_help_open = false;
        self.filter_use_regex = false;
        self.promote_input_query_to_rule_if_needed();
        let rule = ActiveFilterRule {
            query: expr,
            case_sensitive: self.filter_case_sensitive,
            use_regex: false,
            invert_match: false,
        };
        if !self.active_filters.contains(&rule) {
            self.active_filters.push(rule);
        }
        self.filter_query = DEFAULT_FILTER_PATH.to_string();
        self.apply_filter(settings);
    }

    /// Add `path` (with its HL7 dictionary name) to the Report Builder columns and open the Reports window.
    pub fn add_path_to_report_and_open(&mut self, path: &str) {
        let Ok(addr) = path.trim().parse::<FieldAddress>() else {
            return;
        };
        self.report_window
            .open_with_fields(&[addr], &self.doc.records, &self.dictionary);
    }

    /// Open the WinMerge-style **Compare HL7 Messages** window from the current Messages selection.
    ///
    /// - When 3..=[`MAX_COMPARE_MESSAGES`] messages are selected, treats the first selected message
    ///   as the Baseline and compares every subsequent selected message against it in a multi-column view.
    /// - When 2 messages are selected, compares them side-by-side (Left Baseline vs Right).
    /// - When 1 message is selected, places it on the Right and opens the interactive Baseline
    ///   Input Zone on the Left ready to paste or pick another loaded message.
    pub fn open_compare_selected_messages(&mut self) {
        let doc_len = self.doc.records.len();
        let sel: Vec<usize> = self
            .selected_records
            .iter()
            .copied()
            .filter(|&idx| idx < doc_len)
            .take(MAX_COMPARE_MESSAGES)
            .collect();
        if sel.len() >= 3 {
            self.open_compare_multi(&sel);
        } else if sel.len() == 2 {
            self.open_compare_pair(sel[0], sel[1]);
        } else if let Some(idx) = sel
            .first()
            .copied()
            .or_else(|| self.selected_record.filter(|&i| i < doc_len))
        {
            let (right_label, right_raw) = self.format_record_for_compare(idx);
            let options = DiffOptions::default();
            let diff = diff_hl7_messages("", &right_raw, options);
            let multi_diff = diff_hl7_message_set("", &[(&right_label, &right_raw)], options);
            let active_diff_cursor = (diff.total_diff_rows > 0).then_some(0);
            self.compare_modal = Some(CompareMessagesModalState {
                left_label: "Baseline (Empty — Paste from Clipboard)".to_string(),
                right_label: right_label.clone(),
                left_raw: String::new(),
                right_raw: right_raw.clone(),
                left_record_index: None,
                right_record_index: Some(idx),
                targets: vec![CompareTargetEntry {
                    label: right_label,
                    raw: right_raw,
                    record_index: Some(idx),
                }],
                editing_baseline: true,
                baseline_draft: String::new(),
                focus_baseline_input: true,
                options,
                diff,
                multi_diff,
                active_diff_cursor,
                scroll_to_active_diff: false,
                reset_scroll_to_top_left: true,
            });
        } else {
            let options = DiffOptions::default();
            let diff = diff_hl7_messages("", "", options);
            let multi_diff = diff_hl7_message_set("", &[], options);
            self.compare_modal = Some(CompareMessagesModalState {
                left_label: "Baseline".to_string(),
                right_label: "Current".to_string(),
                left_raw: String::new(),
                right_raw: String::new(),
                left_record_index: None,
                right_record_index: None,
                targets: Vec::new(),
                editing_baseline: true,
                baseline_draft: String::new(),
                focus_baseline_input: true,
                options,
                diff,
                multi_diff,
                active_diff_cursor: None,
                scroll_to_active_diff: false,
                reset_scroll_to_top_left: true,
            });
        }
    }

    /// Open the WinMerge-style **Compare HL7 Messages** modal comparing `left_idx` vs `right_idx`.
    pub fn open_compare_pair(&mut self, left_idx: usize, right_idx: usize) {
        let (left_label, left_raw) = self.format_record_for_compare(left_idx);
        let (right_label, right_raw) = self.format_record_for_compare(right_idx);
        let options = DiffOptions::default();
        let diff = diff_hl7_messages(&left_raw, &right_raw, options);
        let multi_diff = diff_hl7_message_set(&left_raw, &[(&right_label, &right_raw)], options);
        let active_diff_cursor = (diff.total_diff_rows > 0).then_some(0);
        let baseline_draft = normalize_hl7_for_multiline(&left_raw);
        self.compare_modal = Some(CompareMessagesModalState {
            left_label,
            right_label: right_label.clone(),
            left_raw,
            right_raw: right_raw.clone(),
            left_record_index: Some(left_idx),
            right_record_index: Some(right_idx),
            targets: vec![CompareTargetEntry {
                label: right_label,
                raw: right_raw,
                record_index: Some(right_idx),
            }],
            editing_baseline: false,
            baseline_draft,
            focus_baseline_input: false,
            options,
            diff,
            multi_diff,
            active_diff_cursor,
            scroll_to_active_diff: false,
            reset_scroll_to_top_left: true,
        });
    }

    /// Open the multi-column **Compare HL7 Messages** window for `2..=MAX_COMPARE_MESSAGES`
    /// document indices, treating `indices[0]` as the Baseline and `indices[1..]` as targets.
    pub fn open_compare_multi(&mut self, indices: &[usize]) {
        if indices.len() < 2 {
            return;
        }
        let capped = &indices[..indices.len().min(MAX_COMPARE_MESSAGES)];
        if capped.len() == 2 {
            self.open_compare_pair(capped[0], capped[1]);
            return;
        }
        let (left_label, left_raw) = self.format_record_for_compare(capped[0]);
        let targets: Vec<CompareTargetEntry> = capped[1..]
            .iter()
            .map(|&idx| {
                let (label, raw) = self.format_record_for_compare(idx);
                CompareTargetEntry {
                    label,
                    raw,
                    record_index: Some(idx),
                }
            })
            .collect();
        let options = DiffOptions::default();
        let target_pairs: Vec<(&str, &str)> = targets
            .iter()
            .map(|t| (t.label.as_str(), t.raw.as_str()))
            .collect();
        let multi_diff = diff_hl7_message_set(&left_raw, &target_pairs, options);
        let (right_label, right_raw, right_record_index) = targets
            .first()
            .map(|t| (t.label.clone(), t.raw.clone(), t.record_index))
            .unwrap_or_else(|| ("Current".to_string(), String::new(), None));
        let diff = diff_hl7_messages(&left_raw, &right_raw, options);
        let active_diff_cursor = (multi_diff.total_diff_rows > 0).then_some(0);
        let baseline_draft = normalize_hl7_for_multiline(&left_raw);
        self.compare_modal = Some(CompareMessagesModalState {
            left_label,
            right_label,
            left_raw,
            right_raw,
            left_record_index: Some(capped[0]),
            right_record_index,
            targets,
            editing_baseline: false,
            baseline_draft,
            focus_baseline_input: false,
            options,
            diff,
            multi_diff,
            active_diff_cursor,
            scroll_to_active_diff: false,
            reset_scroll_to_top_left: true,
        });
    }

    fn format_record_for_compare(&self, doc_idx: usize) -> (String, String) {
        let Some(rec) = self.doc.records.get(doc_idx) else {
            return (format!("Message #{}", doc_idx + 1), String::new());
        };
        let sum = self
            .summaries
            .get(doc_idx)
            .cloned()
            .unwrap_or_else(|| rec.summary());
        let label = if sum.control_id.is_empty() {
            format!("Message #{} ({})", doc_idx + 1, sum.message_type)
        } else {
            format!(
                "Message #{} ({} • {})",
                doc_idx + 1,
                sum.message_type,
                sum.control_id
            )
        };
        let raw = rec.charset().decode_display(rec.bytes()).into_owned();
        (label, raw)
    }

    /// Recompute the active Compare modal diff after options or sides change.
    pub fn compare_recompute(&mut self) {
        if let Some(ref mut modal) = self.compare_modal {
            if let Some(first) = modal.targets.first() {
                modal.right_label = first.label.clone();
                modal.right_raw = first.raw.clone();
                modal.right_record_index = first.record_index;
            }
            modal.diff = diff_hl7_messages(&modal.left_raw, &modal.right_raw, modal.options);
            let target_pairs: Vec<(&str, &str)> = modal
                .targets
                .iter()
                .map(|t| (t.label.as_str(), t.raw.as_str()))
                .collect();
            modal.multi_diff = diff_hl7_message_set(&modal.left_raw, &target_pairs, modal.options);
            let total = modal.active_total_diff_rows();
            modal.active_diff_cursor = (total > 0).then_some(0);
            modal.scroll_to_active_diff = false;
            modal.reset_scroll_to_top_left = true;
        }
    }

    /// Step forward to the next difference in the Compare modal (`F7` / `Down`).
    pub fn compare_next_diff(&mut self) {
        if let Some(ref mut modal) = self.compare_modal {
            let total = modal.active_total_diff_rows();
            if total == 0 {
                modal.active_diff_cursor = None;
                return;
            }
            let next = match modal.active_diff_cursor {
                Some(cur) => (cur + 1) % total,
                None => 0,
            };
            modal.active_diff_cursor = Some(next);
            modal.scroll_to_active_diff = true;
        }
    }

    /// Step backward to the previous difference in the Compare modal (`Shift+F7` / `Up`).
    pub fn compare_prev_diff(&mut self) {
        if let Some(ref mut modal) = self.compare_modal {
            let total = modal.active_total_diff_rows();
            if total == 0 {
                modal.active_diff_cursor = None;
                return;
            }
            let prev = match modal.active_diff_cursor {
                Some(0) | None => total - 1,
                Some(cur) => cur - 1,
            };
            modal.active_diff_cursor = Some(prev);
            modal.scroll_to_active_diff = true;
        }
    }

    /// Swap the Left (Baseline) and Right (Current) messages in the Compare modal.
    pub fn compare_swap_sides(&mut self) {
        if let Some(ref mut modal) = self.compare_modal {
            if let Some(first) = modal.targets.first_mut() {
                std::mem::swap(&mut modal.left_label, &mut first.label);
                std::mem::swap(&mut modal.left_raw, &mut first.raw);
                std::mem::swap(&mut modal.left_record_index, &mut first.record_index);
            } else {
                std::mem::swap(&mut modal.left_label, &mut modal.right_label);
                std::mem::swap(&mut modal.left_raw, &mut modal.right_raw);
                std::mem::swap(&mut modal.left_record_index, &mut modal.right_record_index);
            }
            modal.baseline_draft = normalize_hl7_for_multiline(&modal.left_raw);
        }
        self.compare_recompute();
    }

    /// Promote target column `target_idx` into the Baseline slot (swapping the previous Baseline
    /// into that target slot) and re-diff all columns against the new Baseline.
    pub fn compare_promote_target_to_baseline(&mut self, target_idx: usize) {
        if let Some(ref mut modal) = self.compare_modal {
            if let Some(target) = modal.targets.get_mut(target_idx) {
                std::mem::swap(&mut modal.left_label, &mut target.label);
                std::mem::swap(&mut modal.left_raw, &mut target.raw);
                std::mem::swap(&mut modal.left_record_index, &mut target.record_index);
                modal.baseline_draft = normalize_hl7_for_multiline(&modal.left_raw);
            }
        }
        self.compare_recompute();
    }

    /// Remove target column `target_idx` from the multi-column comparison (keeping at least 1 target).
    pub fn compare_remove_target(&mut self, target_idx: usize) {
        if let Some(ref mut modal) = self.compare_modal {
            if modal.targets.len() > 1 && target_idx < modal.targets.len() {
                modal.targets.remove(target_idx);
            }
        }
        self.compare_recompute();
    }

    /// Load pasted clipboard text into the Left (Baseline) pane of the Compare modal
    /// and immediately switch into the live WinMerge diff view.
    pub fn compare_paste_baseline(&mut self, text: &str) {
        self.compare_apply_baseline(text, Some("Baseline (Clipboard)"));
    }

    /// Apply raw HL7 text as the Left (Baseline) side of the Compare modal and switch to diff view.
    pub fn compare_apply_baseline(&mut self, text: &str, label: Option<&str>) {
        if let Some(ref mut modal) = self.compare_modal {
            modal.left_label = label.unwrap_or("Baseline (Custom)").to_string();
            modal.left_raw = text.to_string();
            modal.left_record_index = None;
            modal.baseline_draft = normalize_hl7_for_multiline(text);
            modal.editing_baseline = false;
            modal.focus_baseline_input = false;
        }
        self.compare_recompute();
    }

    /// Select another loaded message from the active session as the Left (Baseline) side
    /// and immediately switch into the live WinMerge diff view.
    pub fn compare_select_loaded_baseline(&mut self, doc_idx: usize) {
        if doc_idx >= self.doc.records.len() {
            return;
        }
        let (label, raw) = self.format_record_for_compare(doc_idx);
        if let Some(ref mut modal) = self.compare_modal {
            modal.left_label = label;
            modal.baseline_draft = normalize_hl7_for_multiline(&raw);
            modal.left_raw = raw;
            modal.left_record_index = Some(doc_idx);
            modal.editing_baseline = false;
            modal.focus_baseline_input = false;
        }
        self.compare_recompute();
    }

    /// Reopen the interactive Baseline Input Zone pre-populated with the current Left raw text.
    pub fn compare_begin_edit_baseline(&mut self) {
        if let Some(ref mut modal) = self.compare_modal {
            modal.baseline_draft = normalize_hl7_for_multiline(&modal.left_raw);
            modal.editing_baseline = true;
            modal.focus_baseline_input = true;
        }
    }

    /// Request pruning all unmatched messages from memory (`Keep Only Filtered`).
    /// Prompts for confirmation when more than [`PRUNE_CONFIRM_THRESHOLD`] messages would be dropped.
    pub fn request_keep_only_filtered(
        &mut self,
        settings: &AppSettings,
    ) -> Result<bool, EditError> {
        let Some(ref keep) = self.filtered_record_indices else {
            return Ok(false);
        };
        let keep_count = keep.len();
        let drop_count = self.doc.len().saturating_sub(keep_count);
        if drop_count == 0 {
            self.status_message = "All messages already match the active filter.".to_string();
            return Ok(false);
        }
        if drop_count > PRUNE_CONFIRM_THRESHOLD {
            self.delete_confirm_dialog = Some(DeleteConfirmDialog::PruneUnmatched {
                keep_count,
                drop_count,
            });
            return Ok(false);
        }
        self.keep_only_filtered(settings).map(|dropped| dropped > 0)
    }

    /// Remove all messages that do not match the active filter from memory, keeping only the
    /// filtered subset. Drops of at most [`PRUNE_CONFIRM_THRESHOLD`] records are pushed onto
    /// `UndoStack`; larger drops prune in-place and clear `UndoStack` to release memory cleanly.
    pub fn keep_only_filtered(&mut self, settings: &AppSettings) -> Result<usize, EditError> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let Some(ref keep) = self.filtered_record_indices else {
            return Ok(0);
        };
        let keep_set: BTreeSet<usize> = keep.iter().copied().collect();
        let total = self.doc.len();
        let drop_indices: Vec<usize> = (0..total).filter(|i| !keep_set.contains(i)).collect();
        let dropped = drop_indices.len();
        if dropped == 0 {
            return Ok(0);
        }

        if dropped <= PRUNE_CONFIRM_THRESHOLD {
            let cmd = Box::new(DeleteRecordsCmd::selected(drop_indices));
            self.execute_command(cmd, settings)?;
        } else {
            let mut new_records = Vec::with_capacity(keep_set.len());
            let mut new_sources = Vec::with_capacity(keep_set.len());
            for (idx, rec) in std::mem::take(&mut self.doc.records)
                .into_iter()
                .enumerate()
            {
                let src = self.record_sources.get(idx).cloned().flatten();
                if keep_set.contains(&idx) {
                    new_records.push(rec);
                    new_sources.push(src);
                }
            }
            self.doc.records = new_records;
            self.record_sources = new_sources;
            self.summaries = self.doc.records.iter().map(Record::summary).collect();
            self.undo_stack.clear();
            self.invalidate_search();
            self.recompute_filter_if_active();
            if self.doc.is_empty() {
                self.reset_empty_document_overlays();
            } else {
                self.select_record(0, false, settings);
            }
        }

        let kept = self.doc.len();
        self.status_message =
            format!("Kept {kept} filtered message(s) (removed {dropped} unmatched from memory).");
        Ok(dropped)
    }

    /// Return `(record_index, segment_index, FieldAddress)` for the currently selected field row.
    pub fn selected_field_address(&self) -> Option<(usize, usize, FieldAddress)> {
        let rec_idx = self.selected_record?;
        let row_idx = self.selected_field_row?;
        let cache = self.record_cache.as_ref()?;
        let frow = cache.field_rows.get(row_idx)?;
        let addr = frow.address?;
        Some((rec_idx, frow.seg_index, addr))
    }

    /// Execute an [`EditCommand`] on the document, pushing it to `undo_stack`, refreshing summaries,
    /// and restoring selection per `EDIT-11`.
    pub fn execute_command(
        &mut self,
        cmd: Box<dyn EditCommand>,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        self.execute_command_internal(cmd, settings, true)
    }

    fn execute_command_internal(
        &mut self,
        cmd: Box<dyn EditCommand>,
        settings: &AppSettings,
        reset_inline_editor: bool,
    ) -> Result<EditOutcome, EditError> {
        self.undo_stack
            .set_memory_warning_bytes(settings.undo_warning_bytes);
        let prior_addr = self.selected_field_address().map(|(_, _, a)| a);
        let desc = cmd.description();
        let (outcome, warning) = self.undo_stack.execute(cmd, &mut self.doc)?;
        if let Some(w) = warning {
            self.undo_warning = Some(w);
        }
        self.summaries = self.doc.records.iter().map(Record::summary).collect();
        self.recompute_filter_if_active();
        self.invalidate_search();
        if reset_inline_editor {
            let hint = outcome.selection_hint.clone().map(|mut h| {
                if h.address.is_none() {
                    h.address = prior_addr;
                }
                h
            });
            self.apply_selection_anchor(hint, settings);
        } else {
            self.rebuild_selected_record_cache_internal(settings, false);
        }
        self.status_message = format!("{desc} ({} change(s)).", outcome.replacements);
        Ok(outcome)
    }

    /// Undo the most recent edit command and restore selection (EDIT-1, EDIT-11).
    pub fn undo(&mut self, settings: &AppSettings) -> Result<Option<EditOutcome>, EditError> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let prior_addr = self.selected_field_address().map(|(_, _, a)| a);
        let desc = self.undo_stack.undo_description().unwrap_or_default();
        let Some(outcome) = self.undo_stack.undo(&mut self.doc)? else {
            return Ok(None);
        };
        if !self.undo_stack.can_undo() {
            self.is_sorted = false;
        }
        self.summaries = self.doc.records.iter().map(Record::summary).collect();
        self.recompute_filter_if_active();
        self.invalidate_search();
        let hint = outcome.selection_hint.clone().map(|mut h| {
            if h.address.is_none() {
                h.address = prior_addr;
            }
            h
        });
        self.apply_selection_anchor(hint, settings);
        self.status_message = format!("Undo: {desc}");
        Ok(Some(outcome))
    }

    /// Redo the most recently undone edit command and restore selection (EDIT-1, EDIT-11).
    pub fn redo(&mut self, settings: &AppSettings) -> Result<Option<EditOutcome>, EditError> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let prior_addr = self.selected_field_address().map(|(_, _, a)| a);
        let desc = self.undo_stack.redo_description().unwrap_or_default();
        let Some(outcome) = self.undo_stack.redo(&mut self.doc)? else {
            return Ok(None);
        };
        self.summaries = self.doc.records.iter().map(Record::summary).collect();
        self.recompute_filter_if_active();
        self.invalidate_search();
        let hint = outcome.selection_hint.clone().map(|mut h| {
            if h.address.is_none() {
                h.address = prior_addr;
            }
            h
        });
        self.apply_selection_anchor(hint, settings);
        self.status_message = format!("Redo: {desc}");
        Ok(Some(outcome))
    }

    /// Return `true` when at least one HL7 message is loaded in the active session.
    pub fn has_messages(&self) -> bool {
        !self.doc.is_empty()
    }

    fn reset_empty_document_overlays(&mut self) {
        self.selected_record = None;
        self.selected_records.clear();
        self.selected_segment = None;
        self.selected_field_row = None;
        self.selected_field_rows.clear();
        self.field_selection_anchor = None;
        self.selected_field_path = None;
        self.editing_field_path = None;
        self.editing_field_buffer.clear();
        self.editing_field_original.clear();
        self.editing_field_just_opened = false;
        self.editing_field_baseline = None;
        self.record_cache = None;
        self.inline_raw_text.clear();
        self.record_jump_active = false;
        self.filter_bar_open = false;
        self.filter_help_open = false;
        self.filter_query.clear();
        self.active_filters.clear();
        self.filter_error = None;
        self.filtered_record_indices = None;
        self.search_bar_open = false;
        self.find_help_open = false;
        self.invalidate_search();
    }

    /// Restore selection after an edit, undo, or redo so the segment and field lists reselect
    /// the same address when it still exists (EDIT-11).
    pub fn apply_selection_anchor(
        &mut self,
        anchor: Option<SelectionAnchor>,
        settings: &AppSettings,
    ) {
        if self.doc.is_empty() {
            self.reset_empty_document_overlays();
            return;
        }

        let rec_idx = anchor
            .as_ref()
            .map(|a| a.record_index)
            .or(self.selected_record)
            .unwrap_or(0)
            .min(self.doc.len() - 1);

        self.selected_record = Some(rec_idx);
        self.selected_records.retain(|&i| i < self.doc.len());
        if self.selected_records.is_empty() {
            self.selected_records.insert(rec_idx);
        }
        self.sync_record_page_to_selected(settings);

        self.rebuild_selected_record_cache(settings);

        let Some(ref cache) = self.record_cache else {
            return;
        };

        let seg_idx = anchor
            .as_ref()
            .and_then(|a| a.segment_index)
            .or(self.selected_segment)
            .unwrap_or(0)
            .min(cache.segments.len().saturating_sub(1));
        self.selected_segment = Some(seg_idx);
        self.scroll_to_segment = true;

        if let Some(target_addr) = anchor.and_then(|a| a.address) {
            if let Some(frow) = cache
                .field_rows
                .iter()
                .find(|f| f.seg_index == seg_idx && f.address == Some(target_addr))
                .or_else(|| {
                    cache
                        .field_rows
                        .iter()
                        .find(|f| f.address == Some(target_addr))
                })
            {
                self.selected_field_row = Some(frow.row_index);
                self.selected_segment = Some(frow.seg_index);
                self.scroll_to_field_row = true;
                return;
            }
        }

        // If target address no longer exists, select the first field of `seg_idx`.
        if let Some(seg_row) = cache.segments.get(seg_idx) {
            self.selected_field_row =
                (!seg_row.field_rows_range.is_empty()).then_some(seg_row.field_rows_range.start);
        } else {
            self.selected_field_row = None;
        }
    }

    /// Open the raw multiline Edit Record dialog for the selected record (EDIT-2).
    pub fn open_edit_record_dialog(&mut self) {
        let Some(ref cache) = self.record_cache else {
            return;
        };
        self.edit_record_dialog = Some(EditRecordDialog {
            record_index: cache.record_index,
            text: cache.raw_text.clone(),
            cursor_byte_pos: None,
            error: None,
        });
    }

    /// Begin inline editing of the currently selected field/component/subcomponent row in the
    /// Segment Accordion, staging its current value once into `editing_field_buffer`.
    ///
    /// Blocked on `MSH-1/2`, `FHS-1/2`, `BHS-1/2`, and when read-only Decode Escape Sequences mode
    /// is active (§5.5, EDIT-3, D-9).
    pub fn start_inline_field_edit(&mut self, settings: &AppSettings) -> Result<(), EditError> {
        let _ = self.flush_pending_inline_raw_edit(settings);
        let Some(ref cache) = self.record_cache else {
            return Ok(());
        };
        let Some(row_idx) = self.selected_field_row else {
            return Ok(());
        };
        let Some(frow) = cache.field_rows.get(row_idx) else {
            return Ok(());
        };

        if settings.decode_escape_sequences || frow.is_parsed_marker || !frow.is_editable {
            let addr_label = frow
                .address
                .map_or_else(|| "marker".to_string(), |a| a.to_string());
            let reason = explain_cant_edit_field_row(cache, frow, settings);
            self.modal_alert = Some(reason);
            return Err(EditError::CannotEditField {
                address: addr_label,
            });
        }
        let Some(addr) = frow.address else {
            return Ok(());
        };
        let seg_index = frow.seg_index;
        let initial_value = frow.display_value.clone();

        self.editing_field_baseline = self.selected_record.and_then(|rec_idx| {
            self.doc
                .records
                .get(rec_idx)
                .map(|rec| InlineFieldEditBaseline {
                    record_index: rec_idx,
                    baseline_bytes: rec.bytes().to_vec(),
                    baseline_kind: rec.kind(),
                    baseline_delimiters: rec.delimiters(),
                    baseline_edited: rec.is_edited(),
                })
        });
        self.editing_field_row = Some(row_idx);
        self.editing_field_seg_index = Some(seg_index);
        self.editing_field_path = Some(addr);
        self.editing_field_buffer = initial_value.clone();
        self.editing_field_original = initial_value;
        self.editing_field_just_opened = true;
        self.expanded_segments.insert(seg_index);
        Ok(())
    }

    fn restore_inline_field_baseline_record(&mut self) -> Option<usize> {
        let baseline = self.editing_field_baseline.take()?;
        let rec_idx = baseline.record_index;
        if let Some(rec) = self.doc.records.get_mut(rec_idx) {
            rec.restore_raw_state(
                baseline.baseline_bytes,
                baseline.baseline_kind,
                baseline.baseline_delimiters,
                baseline.baseline_edited,
            );
            if let Some(slot) = self.summaries.get_mut(rec_idx) {
                *slot = rec.summary();
            }
            if self.selected_record == Some(rec_idx) {
                let charset = rec.charset();
                let decoded = charset.decode_display(rec.bytes());
                self.inline_raw_text = decoded.replace('\r', "\n");
            }
        }
        Some(rec_idx)
    }

    /// Apply the current `editing_field_buffer` as a live preview to the active record and Raw
    /// Message Editor **without** pushing an entry onto [`UndoStack`]. Invalid delimiters for the
    /// target [`FieldAddress`] hierarchy keep the last valid preview and display a non-blocking
    /// status bar hint. Returns `true` when the preview was applied.
    pub fn apply_live_inline_field_preview(&mut self, settings: &AppSettings) -> bool {
        let (Some(rec_idx), Some(seg_idx), Some(addr), Some(baseline)) = (
            self.selected_record,
            self.editing_field_seg_index,
            self.editing_field_path,
            self.editing_field_baseline.clone(),
        ) else {
            return false;
        };
        if baseline.record_index != rec_idx {
            return false;
        }

        if let Err(err) = hl7_core::edit::commands::validate_delimiters_for_address(
            self.editing_field_buffer.as_bytes(),
            baseline.baseline_delimiters,
            &addr,
        ) {
            self.status_message = err.to_string();
            return false;
        }

        if let Some(rec) = self.doc.records.get_mut(rec_idx) {
            rec.restore_raw_state(
                baseline.baseline_bytes,
                baseline.baseline_kind,
                baseline.baseline_delimiters,
                baseline.baseline_edited,
            );
        } else {
            return false;
        }

        let mut cmd = EditFieldCmd::new(
            rec_idx,
            seg_idx,
            addr,
            self.editing_field_buffer.as_bytes().to_vec(),
            false,
        );
        if let Err(err) = cmd.apply(&mut self.doc) {
            self.status_message = err.to_string();
            return false;
        }

        if let Some(rec) = self.doc.records.get(rec_idx) {
            if let Some(slot) = self.summaries.get_mut(rec_idx) {
                *slot = rec.summary();
            }
        }
        self.rebuild_selected_record_cache_internal(settings, true);
        if let Some(ref cache) = self.record_cache {
            if let Some(frow) = cache
                .field_rows
                .iter()
                .find(|f| f.seg_index == seg_idx && f.address == Some(addr))
            {
                self.editing_field_row = Some(frow.row_index);
                self.selected_field_row = Some(frow.row_index);
            }
        }
        self.status_message = format!("Editing {addr} (Enter to commit, Esc to cancel)...");
        true
    }

    /// Cancel the active inline field edit, restoring the pre-edit baseline record and Raw Message
    /// Editor without pushing anything onto [`UndoStack`].
    pub fn cancel_inline_field_edit(&mut self) {
        let _ = self.restore_inline_field_baseline_record();
        self.editing_field_row = None;
        self.editing_field_seg_index = None;
        self.editing_field_path = None;
        self.editing_field_buffer.clear();
        self.editing_field_original.clear();
        self.editing_field_just_opened = false;
    }

    /// Commit the active inline field edit via a single atomic `EditFieldCmd` on [`UndoStack`]
    /// (only when the value actually changed). Returns `Ok(true)` if modified, `Ok(false)` if unchanged.
    pub fn commit_inline_field_edit(&mut self, settings: &AppSettings) -> Result<bool, EditError> {
        let (Some(rec_idx), Some(seg_idx), Some(addr)) = (
            self.selected_record,
            self.editing_field_seg_index,
            self.editing_field_path,
        ) else {
            self.cancel_inline_field_edit();
            return Ok(false);
        };
        let new_val = std::mem::take(&mut self.editing_field_buffer);
        let orig_val = std::mem::take(&mut self.editing_field_original);
        self.editing_field_row = None;
        self.editing_field_seg_index = None;
        self.editing_field_path = None;
        self.editing_field_just_opened = false;

        // Restore pre-edit baseline first so `EditFieldCmd` captures the true original snapshot
        // and no-op commits leave the record byte-identical.
        let restored = self.restore_inline_field_baseline_record().is_some();

        if new_val == orig_val {
            if restored {
                self.rebuild_selected_record_cache_internal(settings, true);
            }
            return Ok(false);
        }

        let delims = self
            .doc
            .records
            .get(rec_idx)
            .map_or(Delimiters::STANDARD, Record::delimiters);
        let safe_val = escape_disallowed_inline_delimiters(&new_val, addr, delims);
        let auto_escaped = safe_val != new_val;

        let cmd = Box::new(EditFieldCmd::new(
            rec_idx,
            seg_idx,
            addr,
            safe_val.into_bytes(),
            false,
        ));
        match self.execute_command(cmd, settings) {
            Ok(_) => {
                self.status_message = if auto_escaped {
                    format!("Updated {addr} (auto-escaped structural delimiter characters).")
                } else {
                    format!("Updated {addr}.")
                };
                Ok(true)
            }
            Err(e) => {
                if restored {
                    self.rebuild_selected_record_cache_internal(settings, true);
                }
                self.status_message = e.to_string();
                self.modal_alert = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Open the Edit Field dialog for the currently selected field row (EDIT-3, EDIT-4, D-9, D-26).
    ///
    /// Blocked on `MSH-1/2`, `FHS-1/2`, `BHS-1/2`, and when read-only Decode Escape Sequences mode
    /// is active (§5.5).
    pub fn open_edit_field_dialog(&mut self, settings: &AppSettings) -> Result<(), EditError> {
        let Some(ref cache) = self.record_cache else {
            return Ok(());
        };
        let Some(row_idx) = self.selected_field_row else {
            return Ok(());
        };
        let Some(frow) = cache.field_rows.get(row_idx) else {
            return Ok(());
        };

        if settings.decode_escape_sequences || frow.is_parsed_marker || !frow.is_editable {
            let addr_label = frow
                .address
                .map_or_else(|| "marker".to_string(), |a| a.to_string());
            let reason = explain_cant_edit_field_row(cache, frow, settings);
            self.modal_alert = Some(reason);
            return Err(EditError::CannotEditField {
                address: addr_label,
            });
        }

        let Some(address) = frow.address else {
            let reason = explain_cant_edit_field_row(cache, frow, settings);
            self.modal_alert = Some(reason);
            return Err(EditError::CannotEditField {
                address: "marker".to_string(),
            });
        };

        if address.is_header_delimiter_field() {
            self.modal_alert = Some(explain_cant_edit_address(address, settings));
            return Err(EditError::CannotEditField {
                address: address.to_string(),
            });
        }

        self.edit_field_dialog = Some(EditFieldDialog {
            record_index: cache.record_index,
            segment_index: frow.seg_index,
            address,
            value: frow.display_value.clone(),
            show_escape_option: false,
            error: None,
        });
        Ok(())
    }

    /// Format the raw bytes of all currently selected records (joined by `settings.save_terminator`)
    /// as a String suitable for copying to the system clipboard (`§5.1`, `D-26`).
    pub fn format_selected_records_for_clipboard(&self, settings: &AppSettings) -> String {
        let term_mode: SaveTerminator = settings.save_terminator.into();
        let envelope = settings
            .disk_envelope_on_save
            .then(|| settings.mllp_envelope());

        let indices: Vec<usize> = if !self.selected_records.is_empty() {
            self.selected_records.iter().copied().collect()
        } else if let Some(idx) = self.selected_record {
            vec![idx]
        } else {
            Vec::new()
        };

        let mut buf = Vec::new();
        for idx in indices {
            if let Some(rec) = self.doc.records.get(idx) {
                let _ = rec.write_to(&mut buf, term_mode, envelope);
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Insert a segment boundary (`\n`) at the current cursor in the in-place Raw Message Editor (`EDIT-2`, `D-26`).
    pub fn insert_eos_in_inline_raw(&mut self) {
        let pos = self
            .inline_raw_cursor
            .unwrap_or(self.inline_raw_text.len())
            .min(self.inline_raw_text.len());
        let safe_pos = if self.inline_raw_text.is_char_boundary(pos) {
            pos
        } else {
            self.inline_raw_text.len()
        };
        self.inline_raw_text.insert(safe_pos, '\n');
        self.inline_raw_cursor = Some(safe_pos + 1);
    }

    /// Convert `self.inline_raw_text` into CR-joined record bytes (`EDIT-2`).
    fn inline_raw_text_to_cr_bytes(&self) -> Vec<u8> {
        let segments: Vec<&str> = self
            .inline_raw_text
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        segments.join("\r").into_bytes()
    }

    /// Apply edits from `self.inline_raw_text` to the active record in real time so the
    /// Segment Accordion, active delimiters, and `Records` table columns stay synchronized as the
    /// user types—without resetting `inline_raw_text` or cursor position, while tracking the
    /// pre-edit baseline snapshot for [`UndoStack`] coalescing (`EDIT-2`, `D-26`).
    pub fn apply_live_inline_raw_edit(&mut self, settings: &AppSettings) {
        let Some(rec_idx) = self.selected_record else {
            return;
        };
        if rec_idx >= self.doc.len() {
            return;
        }
        if self
            .pending_inline_raw_edit
            .as_ref()
            .is_some_and(|p| p.record_index != rec_idx)
        {
            let _ = self.flush_pending_inline_raw_edit(settings);
        }

        let new_bytes = self.inline_raw_text_to_cr_bytes();
        let now = Instant::now();

        if let Some(ref mut pending) = self.pending_inline_raw_edit {
            pending.latest_bytes = new_bytes.clone();
            pending.last_modified_at = now;
        } else {
            let rec = &self.doc.records[rec_idx];
            let baseline_text = self
                .record_cache
                .as_ref()
                .map_or_else(String::new, |c| c.raw_text.clone());
            self.pending_inline_raw_edit = Some(PendingInlineRawEdit {
                record_index: rec_idx,
                baseline_bytes: rec.bytes().to_vec(),
                baseline_kind: rec.kind(),
                baseline_delimiters: rec.delimiters(),
                baseline_edited: rec.is_edited(),
                baseline_text,
                latest_bytes: new_bytes.clone(),
                last_modified_at: now,
            });
        }

        if let Some(rec) = self.doc.records.get_mut(rec_idx) {
            let _ = rec.replace_all_bytes(new_bytes);
            if let Some(slot) = self.summaries.get_mut(rec_idx) {
                *slot = rec.summary();
            }
        }
        self.recompute_filter_if_active();
        self.invalidate_search();
        self.rebuild_selected_record_cache_internal(settings, false);
    }

    /// Flush any active live raw-editor typing burst as a single coalesced [`EditRecordCmd`]
    /// onto [`UndoStack`] (`EDIT-2`, `D-26`).
    pub fn flush_pending_inline_raw_edit(
        &mut self,
        settings: &AppSettings,
    ) -> Result<Option<EditOutcome>, EditError> {
        let Some(pending) = self.pending_inline_raw_edit.take() else {
            return Ok(None);
        };
        let Some(rec) = self.doc.records.get_mut(pending.record_index) else {
            return Ok(None);
        };

        // Restore pre-edit baseline momentarily so `EditRecordCmd` captures the true baseline snapshot.
        rec.restore_raw_state(
            pending.baseline_bytes.clone(),
            pending.baseline_kind,
            pending.baseline_delimiters,
            pending.baseline_edited,
        );

        if pending.latest_bytes == pending.baseline_bytes {
            if let Some(slot) = self.summaries.get_mut(pending.record_index) {
                *slot = self.doc.records[pending.record_index].summary();
            }
            self.rebuild_selected_record_cache_internal(settings, false);
            return Ok(None);
        }

        let cmd = Box::new(EditRecordCmd::new(
            pending.record_index,
            pending.latest_bytes,
        ));
        self.execute_command_internal(cmd, settings, false)
            .map(Some)
    }

    /// Poll the live raw-editor debounce timer and flush the coalesced [`EditRecordCmd`]
    /// onto [`UndoStack`] once [`INLINE_RAW_DEBOUNCE_DURATION`] has elapsed (`EDIT-2`).
    pub fn poll_inline_raw_debounce(
        &mut self,
        settings: &AppSettings,
    ) -> Result<Option<EditOutcome>, EditError> {
        let should_flush = self
            .pending_inline_raw_edit
            .as_ref()
            .is_some_and(|p| p.last_modified_at.elapsed() >= INLINE_RAW_DEBOUNCE_DURATION);
        if should_flush {
            self.flush_pending_inline_raw_edit(settings)
        } else {
            Ok(None)
        }
    }

    /// Commit edits made in the in-place Raw Message Editor via [`EditRecordCmd`] onto [`UndoStack`] (`EDIT-2`, `D-26`).
    pub fn commit_inline_raw_edit(
        &mut self,
        settings: &AppSettings,
    ) -> Result<Option<EditOutcome>, EditError> {
        if self.pending_inline_raw_edit.is_none() {
            let Some(_rec_idx) = self.selected_record else {
                return Ok(None);
            };
            let differs = self
                .record_cache
                .as_ref()
                .is_some_and(|c| c.raw_text != self.inline_raw_text);
            if !differs {
                return Ok(None);
            }
            self.apply_live_inline_raw_edit(settings);
        }
        self.flush_pending_inline_raw_edit(settings)
    }

    /// Revert any uncommitted edits in the in-place Raw Message Editor back to the pre-edit baseline (`EDIT-2`).
    pub fn revert_inline_raw_edit(&mut self) {
        if let Some(pending) = self.pending_inline_raw_edit.take() {
            if let Some(rec) = self.doc.records.get_mut(pending.record_index) {
                rec.restore_raw_state(
                    pending.baseline_bytes,
                    pending.baseline_kind,
                    pending.baseline_delimiters,
                    pending.baseline_edited,
                );
                if let Some(slot) = self.summaries.get_mut(pending.record_index) {
                    *slot = rec.summary();
                }
            }
            self.inline_raw_text = pending.baseline_text;
            self.inline_raw_cursor = None;
            let default_settings = AppSettings::default();
            self.rebuild_selected_record_cache_internal(&default_settings, true);
        } else if let Some(ref cache) = self.record_cache {
            self.inline_raw_text = cache.raw_text.clone();
        }
    }

    /// Revert any uncommitted live edits in the Raw Message Editor using `settings` (or undo the
    /// most recent committed edit if no uncommitted burst is active) (`EDIT-2`).
    pub fn revert_inline_raw_edit_with_settings(&mut self, settings: &AppSettings) {
        if let Some(pending) = self.pending_inline_raw_edit.take() {
            if let Some(rec) = self.doc.records.get_mut(pending.record_index) {
                rec.restore_raw_state(
                    pending.baseline_bytes,
                    pending.baseline_kind,
                    pending.baseline_delimiters,
                    pending.baseline_edited,
                );
                if let Some(slot) = self.summaries.get_mut(pending.record_index) {
                    *slot = rec.summary();
                }
            }
            self.recompute_filter_if_active();
            self.invalidate_search();
            self.rebuild_selected_record_cache_internal(settings, true);
            self.status_message = "Reverted uncommitted raw edits.".to_string();
        } else if let Some(ref cache) = self.record_cache {
            self.inline_raw_text = cache.raw_text.clone();
        }
    }

    /// Return `true` when one or more filters are currently active.
    pub fn has_active_filter(&self) -> bool {
        self.filtered_record_indices.is_some() || !self.active_filters.is_empty()
    }

    /// Replace any active filters with `<PATH>=<VALUE>`, open the Filter bar, apply immediately,
    /// and request keyboard focus on the Filter input box with the cursor at the end.
    pub fn open_filter_for_path(&mut self, path: &str, value: &str, settings: &AppSettings) {
        self.filter_bar_open = true;
        self.filter_help_open = false;
        self.filter_use_regex = false;
        self.filter_invert_match = false;
        self.active_filters.clear();
        self.filter_query = format_filter_by_path_query(path, value);
        self.filter_builder_value.clear();
        self.focus_filter_input = true;
        self.apply_filter(settings);
    }

    /// Append `<PATH>=<VALUE>` as an additional stacked filter rule (promoting any currently
    /// applied single query into `active_filters` first) and immediately re-evaluate.
    pub fn add_filter_for_path(&mut self, path: &str, value: &str, settings: &AppSettings) {
        self.filter_bar_open = true;
        self.filter_help_open = false;
        self.promote_input_query_to_rule_if_needed();
        let expr = format_filter_by_path_query(path, value);
        let rule = ActiveFilterRule {
            query: expr,
            case_sensitive: self.filter_case_sensitive,
            use_regex: false,
            invert_match: false,
        };
        if !self.active_filters.contains(&rule) {
            self.active_filters.push(rule);
        }
        self.filter_query.clear();
        self.focus_filter_input = true;
        self.apply_filter(settings);
    }

    /// Resolve the unified single-row Filter bar inputs (`filter_query` + `filter_builder_op` +
    /// `filter_builder_value`) into `(expression, effective_use_regex)`:
    /// - If `filter_query` already contains a compound (`&&`/`||`) or complete inline field predicate
    ///   (e.g. `PID-3(*).1=123`, `MSH-7>=20260101`, `PID-5.1=~^SM.*`, `ROL-4 exists`) and
    ///   `filter_builder_value` is empty, use `filter_query` verbatim (smart inline fallback).
    /// - If `filter_query` is a bare HL7 field path (e.g. `PID-3(*).1`, `ROL-4`) and either
    ///   `filter_builder_op == Exists` or `filter_builder_value` is non-empty, compose
    ///   `<PATH> <OP> <VALUE>` (or `<PATH> exists`) with clear operator spacing.
    /// - Otherwise treat `filter_query` (or `filter_builder_value` when `filter_query` is empty)
    ///   as a plain-text or regex query across the record.
    pub fn resolve_unified_filter_input(&self) -> Option<(String, bool)> {
        let q = self.filter_query.trim();
        let v = self.filter_builder_value.trim();
        let op = self.filter_builder_op;

        if q == DEFAULT_FILTER_PATH {
            if op == FilterBuilderOp::Exists {
                return Some((format!("{DEFAULT_FILTER_PATH} exists"), false));
            }
            if v.is_empty() {
                return None;
            }
            if let Some(expr) = op.format_expression(DEFAULT_FILTER_PATH, v) {
                return Some((expr, self.filter_use_regex || op == FilterBuilderOp::Regex));
            }
            return None;
        }

        if q.is_empty() {
            if !v.is_empty() && op.requires_value() {
                return Some((
                    v.to_string(),
                    self.filter_use_regex || op == FilterBuilderOp::Regex,
                ));
            }
            return None;
        }

        // Smart inline parsing fallback when the user typed a complete expression into the first box
        if v.is_empty() {
            if split_compound_filter_query(q).is_some()
                || RecordFilter::parse_field_predicate(q).is_ok()
            {
                return Some((q.to_string(), self.filter_use_regex));
            }
            if op == FilterBuilderOp::Exists && hl7_core::parse_filter_field_path(q).is_ok() {
                return Some((format!("{q} exists"), false));
            }
            return Some((
                q.to_string(),
                self.filter_use_regex || op == FilterBuilderOp::Regex,
            ));
        }

        // Both Path/Query and Value are populated: compose with the selected operator dropdown
        if let Some(expr) = op.format_expression(q, v) {
            return Some((expr, self.filter_use_regex || op == FilterBuilderOp::Regex));
        }
        Some((q.to_string(), self.filter_use_regex))
    }

    /// Append the current unified Filter bar input (supporting both `Path` + `Operator` + `Value`
    /// and inline expressions including `&&` / `||`) as stacked filter rule chip(s), clear the
    /// inputs, and re-evaluate.
    pub fn add_filter_from_input(&mut self, settings: &AppSettings) {
        let Some((resolved, effective_regex)) = self.resolve_unified_filter_input() else {
            return;
        };
        let (parsed_mode, parts) = match split_compound_filter_query(&resolved) {
            Some((mode, parts)) => (Some(mode), parts),
            None => (None, vec![resolved]),
        };

        let opts = FilterMatchOptions {
            case_sensitive: self.filter_case_sensitive,
            use_regex: effective_regex,
            invert_match: self.filter_invert_match,
        };
        for part in &parts {
            if let Err(err) = RecordFilter::parse_with_options(part, opts) {
                self.filter_error = Some(err);
                return;
            }
        }

        if let Some(mode) = parsed_mode {
            if self.active_filters.is_empty() {
                self.filter_combine_mode = mode;
            }
        }
        for part in parts {
            let rule = ActiveFilterRule {
                query: part,
                case_sensitive: self.filter_case_sensitive,
                use_regex: effective_regex,
                invert_match: self.filter_invert_match,
            };
            if !self.active_filters.contains(&rule) {
                self.active_filters.push(rule);
            }
        }
        self.filter_query = DEFAULT_FILTER_PATH.to_string();
        self.filter_builder_value.clear();
        self.filter_error = None;
        self.apply_filter(settings);
    }

    /// Remove one stacked filter rule by index and re-evaluate (or clear if none remain).
    pub fn remove_filter_rule(&mut self, index: usize, settings: &AppSettings) {
        if index < self.active_filters.len() {
            self.active_filters.remove(index);
        }
        if self.active_filters.is_empty() && self.resolve_unified_filter_input().is_none() {
            self.clear_filter(settings);
        } else {
            self.apply_filter(settings);
        }
    }

    /// Switch how multiple stacked filter rules are combined (`AND` vs. `OR`) and re-evaluate.
    pub fn set_filter_combine_mode(&mut self, mode: FilterCombineMode, settings: &AppSettings) {
        if self.filter_combine_mode != mode {
            self.filter_combine_mode = mode;
            if self.has_active_filter() {
                self.apply_filter(settings);
            }
        }
    }

    fn promote_input_query_to_rule_if_needed(&mut self) {
        if self.active_filters.is_empty() && self.filtered_record_indices.is_some() {
            if let Some((resolved, effective_regex)) = self.resolve_unified_filter_input() {
                let (parsed_mode, parts) = match split_compound_filter_query(&resolved) {
                    Some((mode, parts)) => (Some(mode), parts),
                    None => (None, vec![resolved]),
                };
                if let Some(mode) = parsed_mode {
                    self.filter_combine_mode = mode;
                }
                for part in parts {
                    let rule = ActiveFilterRule {
                        query: part,
                        case_sensitive: self.filter_case_sensitive,
                        use_regex: effective_regex,
                        invert_match: self.filter_invert_match,
                    };
                    if !self.active_filters.contains(&rule) {
                        self.active_filters.push(rule);
                    }
                }
                self.filter_query.clear();
                self.filter_builder_value.clear();
            }
        }
    }

    fn compile_active_filters(
        &self,
    ) -> Result<Option<(FilterCombineMode, Vec<RecordFilter>)>, String> {
        let mut compiled = Vec::new();
        let mut mode = self.filter_combine_mode;

        for rule in &self.active_filters {
            let opts = FilterMatchOptions {
                case_sensitive: rule.case_sensitive,
                use_regex: rule.use_regex,
                invert_match: rule.invert_match,
            };
            compiled.push(RecordFilter::parse_with_options(&rule.query, opts)?);
        }

        if let Some((resolved, effective_regex)) = self.resolve_unified_filter_input() {
            let opts = FilterMatchOptions {
                case_sensitive: self.filter_case_sensitive,
                use_regex: effective_regex,
                invert_match: self.filter_invert_match,
            };
            if let Some((parsed_mode, parts)) = split_compound_filter_query(&resolved) {
                if self.active_filters.is_empty() {
                    mode = parsed_mode;
                }
                for part in parts {
                    compiled.push(RecordFilter::parse_with_options(&part, opts)?);
                }
            } else {
                compiled.push(RecordFilter::parse_with_options(&resolved, opts)?);
            }
        }

        if compiled.is_empty() {
            Ok(None)
        } else {
            Ok(Some((mode, compiled)))
        }
    }

    fn record_matches_filters(
        record: &Record,
        mode: FilterCombineMode,
        filters: &[RecordFilter],
    ) -> bool {
        match mode {
            FilterCombineMode::And => filters.iter().all(|f| f.matches_record(record)),
            FilterCombineMode::Or => filters.iter().any(|f| f.matches_record(record)),
        }
    }

    /// Apply the non-destructive Record List Filter in `self.active_filters` and/or `self.filter_query` (`§5.1`, `D-26`).
    pub fn apply_filter(&mut self, settings: &AppSettings) {
        match self.compile_active_filters() {
            Ok(None) => {
                self.clear_filter(settings);
            }
            Ok(Some((mode, filters))) => {
                self.filter_error = None;
                let matching: Vec<usize> = self
                    .doc
                    .records
                    .iter()
                    .enumerate()
                    .filter_map(|(i, rec)| {
                        Self::record_matches_filters(rec, mode, &filters).then_some(i)
                    })
                    .collect();
                let count = matching.len();
                let total = self.doc.len();
                self.filtered_record_indices = Some(matching.clone());
                self.record_page_index = 0;
                self.status_message = format!("Showing {count} of {total} messages");
                if let Some(cur) = self.selected_record {
                    if !matching.contains(&cur) {
                        if let Some(&first) = matching.first() {
                            self.select_record(first, false, settings);
                            self.scroll_to_record = true;
                        }
                    } else {
                        self.sync_record_page_to_selected(settings);
                    }
                } else if let Some(&first) = matching.first() {
                    self.select_record(first, false, settings);
                    self.scroll_to_record = true;
                }
            }
            Err(err) => {
                self.filter_error = Some(err);
            }
        }
    }

    /// Clear all active Record List Filters without modifying `Document` or `UndoStack` (`§5.1`, `D-26`).
    pub fn clear_filter(&mut self, settings: &AppSettings) {
        self.filter_query = DEFAULT_FILTER_PATH.to_string();
        self.filter_builder_value.clear();
        self.active_filters.clear();
        self.filter_error = None;
        self.filtered_record_indices = None;
        self.record_page_index = 0;
        self.sync_record_page_to_selected(settings);
        self.status_message = format!("Showing all {} message(s).", self.doc.len());
    }

    fn recompute_filter_if_active(&mut self) {
        if self.filtered_record_indices.is_none() {
            return;
        }
        if let Ok(Some((mode, filters))) = self.compile_active_filters() {
            let matching: Vec<usize> = self
                .doc
                .records
                .iter()
                .enumerate()
                .filter_map(|(i, rec)| {
                    Self::record_matches_filters(rec, mode, &filters).then_some(i)
                })
                .collect();
            self.filtered_record_indices = Some(matching);
        }
    }

    fn invalidate_search(&mut self) {
        self.last_searched_query.clear();
        self.search_matches.clear();
        self.search_field_breakdown.clear();
        self.search_match_cursor = None;
        self.search_status.clear();
    }

    /// Return the currently visible 0-based document record indices (filtered or full list) (`D-26`).
    pub fn visible_record_indices(&self) -> Vec<usize> {
        match &self.filtered_record_indices {
            Some(list) => list.clone(),
            None => (0..self.doc.len()).collect(),
        }
    }

    /// Return `(current_1_based_visible_position, visible_count, total_count)` for the `Rec # / Total` control (`D-26`).
    pub fn record_position_counts(&self) -> (usize, usize, usize) {
        let total = self.doc.len();
        let visible = self.visible_record_indices();
        let vis_count = visible.len();
        if vis_count == 0 {
            return (0, 0, total);
        }
        let cur_doc = self.selected_record.unwrap_or(visible[0]);
        let pos = visible
            .iter()
            .position(|&i| i == cur_doc)
            .map_or(1, |p| p + 1);
        (pos, vis_count, total)
    }

    /// Format the `Msg # / Total` badge text (e.g. `"Msg 1 / 250"` or `"Msg 1 / 42 (250 total)"` when filtered) (`D-26`, `D-29`).
    pub fn format_record_position_label(&self) -> String {
        let (pos, vis, total) = self.record_position_counts();
        if self.filtered_record_indices.is_some() {
            format!("Msg {pos} / {vis} ({total} total)")
        } else {
            format!("Msg {pos} / {total}")
        }
    }

    /// Open and pre-fill the clickable `Rec # / Total` inline numeric jump input (`D-26`).
    pub fn begin_record_jump(&mut self) {
        let (pos, vis, _) = self.record_position_counts();
        if vis == 0 {
            return;
        }
        self.record_jump_input = pos.max(1).to_string();
        self.record_jump_active = true;
        self.focus_record_jump_input = true;
    }

    /// Jump to 1-based visible record number `target_1_based` (clamped to `1..=visible_count`),
    /// selecting it and requesting a table scroll (`D-26`).
    pub fn jump_to_visible_record_number(
        &mut self,
        target_1_based: usize,
        settings: &AppSettings,
    ) -> Option<usize> {
        let visible = self.visible_record_indices();
        if visible.is_empty() {
            return None;
        }
        let clamped_pos = target_1_based.clamp(1, visible.len()) - 1;
        let doc_idx = visible[clamped_pos];
        self.select_record(doc_idx, false, settings);
        self.scroll_to_record = true;
        self.record_jump_active = false;
        Some(doc_idx)
    }

    /// Commit the current `record_jump_input` string (`Enter` in the inline jump box) (`D-26`).
    pub fn commit_record_jump_input(&mut self, settings: &AppSettings) -> Option<usize> {
        let trimmed = self.record_jump_input.trim();
        let Ok(num) = trimmed.parse::<usize>() else {
            self.record_jump_active = false;
            return None;
        };
        self.jump_to_visible_record_number(num.max(1), settings)
    }

    /// Navigate to the First (`|<`) visible record (`D-26`).
    pub fn navigate_first_record(&mut self, settings: &AppSettings) -> Option<usize> {
        self.jump_to_visible_record_number(1, settings)
    }

    /// Navigate to the Last (`>|`) visible record (`D-26`).
    pub fn navigate_last_record(&mut self, settings: &AppSettings) -> Option<usize> {
        let vis_len = self.visible_record_indices().len();
        if vis_len == 0 {
            return None;
        }
        self.jump_to_visible_record_number(vis_len, settings)
    }

    /// Navigate to the Previous (`<`) visible record (`D-26`).
    pub fn navigate_prev_record(&mut self, settings: &AppSettings) -> Option<usize> {
        let (pos, vis, _) = self.record_position_counts();
        if vis == 0 {
            return None;
        }
        self.jump_to_visible_record_number(pos.saturating_sub(1).max(1), settings)
    }

    /// Navigate to the Next (`>`) visible record (`D-26`).
    pub fn navigate_next_record(&mut self, settings: &AppSettings) -> Option<usize> {
        let (pos, vis, _) = self.record_position_counts();
        if vis == 0 {
            return None;
        }
        self.jump_to_visible_record_number((pos + 1).min(vis), settings)
    }

    /// Return the effective page-size limit (`None` when Unlimited or overridden via `Load All`) (`§5.1`, `NFR-1`).
    pub fn effective_records_per_page(&self, settings: &AppSettings) -> Option<usize> {
        if self.load_all_pages_override || settings.max_records_per_page == 0 {
            None
        } else {
            Some(settings.max_records_per_page)
        }
    }

    /// Return the total number of pages for the currently visible (filtered or full) record list (`§5.1`).
    pub fn total_record_pages(&self, settings: &AppSettings) -> usize {
        let vis_len = self.visible_record_indices().len();
        if vis_len == 0 {
            return 1;
        }
        match self.effective_records_per_page(settings) {
            Some(per_page) => vis_len.div_ceil(per_page).max(1),
            None => 1,
        }
    }

    /// Return `true` when the visible record list exceeds `max_records_per_page` and is actively paged (`§5.1`).
    pub fn is_record_list_paged(&self, settings: &AppSettings) -> bool {
        self.total_record_pages(settings) > 1
    }

    /// Return `(active_page_doc_indices, page_start_visible_offset)` for the bounded `Records` table (`§5.1`).
    pub fn current_page_visible_indices(&self, settings: &AppSettings) -> (Vec<usize>, usize) {
        let visible = self.visible_record_indices();
        let Some(per_page) = self.effective_records_per_page(settings) else {
            return (visible, 0);
        };
        if visible.len() <= per_page {
            return (visible, 0);
        }
        let total_pages = visible.len().div_ceil(per_page).max(1);
        let page_idx = self.record_page_index.min(total_pages - 1);
        let start = page_idx * per_page;
        let end = (start + per_page).min(visible.len());
        (visible[start..end].to_vec(), start)
    }

    /// Synchronize `record_page_index` to whichever page contains `selected_record` (`§5.1`).
    pub fn sync_record_page_to_selected(&mut self, settings: &AppSettings) {
        let Some(per_page) = self.effective_records_per_page(settings) else {
            self.record_page_index = 0;
            return;
        };
        let visible = self.visible_record_indices();
        if visible.is_empty() {
            self.record_page_index = 0;
            return;
        }
        if let Some(sel) = self.selected_record {
            if let Some(vis_pos) = visible.iter().position(|&i| i == sel) {
                self.record_page_index = vis_pos / per_page;
                return;
            }
        }
        let total_pages = visible.len().div_ceil(per_page).max(1);
        self.record_page_index = self.record_page_index.min(total_pages - 1);
    }

    /// Navigate to the previous page (`Prev Page`) in the bounded `Records` list (`§5.1`).
    pub fn navigate_prev_record_page(&mut self, settings: &AppSettings) -> Option<usize> {
        let per_page = self.effective_records_per_page(settings)?;
        if self.record_page_index == 0 {
            return self.selected_record;
        }
        let target_page = self.record_page_index - 1;
        let target_1_based = target_page * per_page + 1;
        self.jump_to_visible_record_number(target_1_based, settings)
    }

    /// Navigate to the next page (`Next Page`) in the bounded `Records` list (`§5.1`).
    pub fn navigate_next_record_page(&mut self, settings: &AppSettings) -> Option<usize> {
        let per_page = self.effective_records_per_page(settings)?;
        let total_pages = self.total_record_pages(settings);
        if self.record_page_index + 1 >= total_pages {
            return self.selected_record;
        }
        let target_page = self.record_page_index + 1;
        let target_1_based = target_page * per_page + 1;
        self.jump_to_visible_record_number(target_1_based, settings)
    }

    /// Override bounded paging for the current session (`Load All` button) (`§5.1`).
    pub fn load_all_record_pages(&mut self) {
        self.load_all_pages_override = true;
        self.record_page_index = 0;
        self.scroll_to_record = true;
    }

    /// Open and focus the Find & Replace panel on the `Find` tab (`Ctrl+F`, `D-27`).
    pub fn open_find_panel(&mut self) {
        self.search_bar_open = true;
        self.find_replace_tab = FindReplaceTab::Find;
        self.focus_search_input = true;
    }

    /// Open and focus the Find & Replace panel on the `Replace` tab (`Ctrl+H`, `D-27`).
    pub fn open_replace_panel(&mut self) {
        self.search_bar_open = true;
        self.find_replace_tab = FindReplaceTab::Replace;
        self.focus_search_input = true;
    }

    /// Close the Find & Replace panel (`Escape`, `D-27`).
    pub fn close_find_replace_panel(&mut self) {
        self.search_bar_open = false;
    }

    fn push_history(list: &mut Vec<String>, item: &str) {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            return;
        }
        list.retain(|existing| existing != trimmed);
        list.insert(0, trimmed.to_string());
        list.truncate(15);
    }

    /// Return the 0-based document record indices targeted by `self.find_replace_scope` (`D-27`).
    pub fn target_indices_for_find_scope(&self) -> Vec<usize> {
        match self.find_replace_scope {
            FindReplaceScope::CurrentRecord => self.selected_record.into_iter().collect(),
            FindReplaceScope::FilteredRecords => self.visible_record_indices(),
            FindReplaceScope::AllRecords => (0..self.doc.len()).collect(),
        }
    }

    /// Return the currently active [`SearchMatch`], if any (`D-26`).
    pub fn current_search_match(&self) -> Option<&SearchMatch> {
        let idx = self.search_match_cursor?;
        self.search_matches.get(idx)
    }

    /// Jump to the next (`forward = true`, `F3`) or previous (`forward = false`, `Shift+F3`)
    /// matching `(record_index, segment_index, Option<FieldAddress>)` without hiding rows (`§5.1`, `D-26`, `D-27`).
    pub fn jump_search(&mut self, forward: bool, settings: &AppSettings) -> Option<SearchMatch> {
        let query_trimmed = self.search_query.trim().to_string();
        let field_trimmed = self.search_field_input.trim().to_string();
        if query_trimmed.is_empty() && field_trimmed.is_empty() {
            self.search_status = "Enter search text or field path.".to_string();
            return None;
        }
        Self::push_history(&mut self.find_history, &query_trimmed);

        let field_restriction = if field_trimmed.is_empty() {
            None
        } else {
            match field_trimmed.parse::<FieldAddress>() {
                Ok(addr) => Some(addr),
                Err(e) => {
                    self.search_status = format!("Invalid field '{field_trimmed}': {e}");
                    return None;
                }
            }
        };

        let sig = format!(
            "{query_trimmed}|f={field_trimmed}|cs={}|re={}|wh={}",
            self.search_case_sensitive, self.search_use_regex, self.search_match_whole
        );

        if self.last_searched_query != sig {
            let opts = SearchQueryOptions {
                query: query_trimmed.clone(),
                field_restriction,
                case_sensitive: self.search_case_sensitive,
                use_regex: self.search_use_regex,
                match_whole: self.search_match_whole,
            };
            match find_search_matches_with_options(&self.doc.records, &opts) {
                Ok(matches) => {
                    self.search_field_breakdown = summarize_matches_by_field(&matches);
                    self.search_matches = matches;
                    self.last_searched_query = sig;
                    self.search_match_cursor = None;
                }
                Err(err) => {
                    self.search_status = err;
                    return None;
                }
            }
        }

        let total = self.search_matches.len();
        if total == 0 {
            self.search_status = "0 matches".to_string();
            self.status_message = format!("No matches found for '{query_trimmed}'.");
            return None;
        }

        let mut wrapped = false;
        let next_idx = match self.search_match_cursor {
            None => {
                if forward {
                    0
                } else {
                    total - 1
                }
            }
            Some(cur) => {
                if forward {
                    if cur + 1 >= total {
                        if !self.search_wrap_around {
                            self.search_status =
                                format!("End of matches ({total} total — wrap off)");
                            return None;
                        }
                        wrapped = true;
                        0
                    } else {
                        cur + 1
                    }
                } else if cur == 0 {
                    if !self.search_wrap_around {
                        self.search_status = format!("Start of matches ({total} total — wrap off)");
                        return None;
                    }
                    wrapped = true;
                    total - 1
                } else {
                    cur - 1
                }
            }
        };

        self.search_match_cursor = Some(next_idx);
        let hit = self.search_matches[next_idx].clone();

        // Select the record, scroll the record list, expand the matching segment in the accordion, and select the field row.
        self.select_record(hit.record_index, false, settings);
        self.scroll_to_record = true;
        self.selected_segment = Some(hit.segment_index);
        self.expanded_segments.insert(hit.segment_index);
        self.scroll_to_segment = true;
        self.scroll_accordion_to_selection = true;

        if let Some(ref cache) = self.record_cache {
            if let Some(addr) = hit.field_address {
                if let Some(frow) = cache
                    .field_rows
                    .iter()
                    .find(|f| f.seg_index == hit.segment_index && f.address == Some(addr))
                {
                    self.selected_field_row = Some(frow.row_index);
                    self.scroll_to_field_row = true;
                }
            } else if let Some(seg) = cache.segments.get(hit.segment_index) {
                if !seg.field_rows_range.is_empty() {
                    self.selected_field_row = Some(seg.field_rows_range.start);
                    self.scroll_to_field_row = true;
                }
            }
        }

        let wrap_suffix = if wrapped { " (wrapped)" } else { "" };
        self.search_status = format!("Match {} of {total}{wrap_suffix}", next_idx + 1);
        self.status_message = format!(
            "Find: {} — Record #{}, Segment #{}",
            self.search_status,
            hit.record_index + 1,
            hit.segment_index + 1
        );
        Some(hit)
    }

    /// Count all matches within `self.find_replace_scope`, updating `search_field_breakdown` and
    /// `search_status` (`"N matches across M records"`) (`D-27`).
    pub fn count_find_matches(&mut self) -> Result<(usize, usize), String> {
        let query_trimmed = self.search_query.trim().to_string();
        let field_trimmed = self.search_field_input.trim().to_string();
        if query_trimmed.is_empty() && field_trimmed.is_empty() {
            let err = "Enter search text or field path.".to_string();
            self.search_status = err.clone();
            return Err(err);
        }
        Self::push_history(&mut self.find_history, &query_trimmed);

        let field_restriction = if field_trimmed.is_empty() {
            None
        } else {
            Some(
                field_trimmed
                    .parse::<FieldAddress>()
                    .map_err(|e| format!("Invalid field '{field_trimmed}': {e}"))?,
            )
        };

        let opts = SearchQueryOptions {
            query: query_trimmed,
            field_restriction,
            case_sensitive: self.search_case_sensitive,
            use_regex: self.search_use_regex,
            match_whole: self.search_match_whole,
        };
        let all_matches = find_search_matches_with_options(&self.doc.records, &opts)?;
        let allowed: BTreeSet<usize> = self.target_indices_for_find_scope().into_iter().collect();
        let scoped_matches: Vec<SearchMatch> = all_matches
            .into_iter()
            .filter(|m| allowed.contains(&m.record_index))
            .collect();

        let unique_records: BTreeSet<usize> =
            scoped_matches.iter().map(|m| m.record_index).collect();
        let match_count = scoped_matches.len();
        let rec_count = unique_records.len();

        self.search_field_breakdown = summarize_matches_by_field(&scoped_matches);
        self.search_matches = scoped_matches;
        self.search_match_cursor = None;
        self.search_status = format!("{match_count} matches across {rec_count} records");
        self.status_message = format!("Count: {}", self.search_status);
        Ok((match_count, rec_count))
    }

    /// Build [`Hl7ReplaceOptions`] from the current Find & Replace panel inputs (`D-27`).
    pub fn build_hl7_replace_options(&self) -> Result<Hl7ReplaceOptions, String> {
        let field_trimmed = self.search_field_input.trim();
        let field_restriction = if field_trimmed.is_empty() {
            None
        } else {
            Some(
                field_trimmed
                    .parse::<FieldAddress>()
                    .map_err(|e| format!("Invalid field '{field_trimmed}': {e}"))?,
            )
        };
        if self.search_query.is_empty() && field_restriction.is_none() {
            return Err(
                "Enter 'Find what' text or specify a Target Field Path (e.g. PID-3.1.4)."
                    .to_string(),
            );
        }
        Ok(Hl7ReplaceOptions {
            find: self.search_query.clone(),
            replace: self.replace_with_input.clone(),
            field_restriction,
            case_sensitive: self.search_case_sensitive,
            use_regex: self.search_use_regex,
            match_whole: self.search_match_whole,
        })
    }

    /// Replace the currently highlighted match in-place (pushing an atomic [`EditRecordCmd`]
    /// onto [`UndoStack`]) and automatically advance to the next match (`D-27`).
    pub fn replace_current_and_advance(
        &mut self,
        settings: &AppSettings,
    ) -> Result<Option<SearchMatch>, String> {
        let opts = self.build_hl7_replace_options()?;
        Self::push_history(&mut self.find_history, &opts.find);
        Self::push_history(&mut self.replace_history, &opts.replace);

        let active_hit = match self.current_search_match().cloned() {
            Some(hit) => hit,
            None => match self.jump_search(true, settings) {
                Some(hit) => hit,
                None => {
                    self.replace_status_banner = "No match found to replace.".to_string();
                    return Ok(None);
                }
            },
        };

        let Some(rec) = self.doc.records.get(active_hit.record_index) else {
            return Ok(None);
        };

        let target_addr = opts.field_restriction.or(active_hit.field_address);
        let replacement_res = match target_addr {
            Some(addr) if !addr.is_header_delimiter_field() => {
                replace_in_single_field(rec, active_hit.segment_index, &addr, &opts)?
            }
            _ => replace_in_record(rec, &opts)?,
        };

        if let Some((new_bytes, occ)) = replacement_res {
            let desc = match target_addr {
                Some(addr) => format!("Replace in {addr}"),
                None => format!("Replace in Record #{}", active_hit.record_index + 1),
            };
            let cmd = Box::new(EditRecordCmd::with_context(
                active_hit.record_index,
                new_bytes,
                desc,
                Some(active_hit.segment_index),
                target_addr,
            ));
            self.execute_command(cmd, settings)
                .map_err(|e| e.to_string())?;
            self.replace_status_banner = format!(
                "Replaced {occ} occurrence(s) in Record #{}",
                active_hit.record_index + 1
            );
            self.status_message = self.replace_status_banner.clone();
        } else {
            self.replace_status_banner = "No replaceable occurrence at active match.".to_string();
        }

        Ok(self.jump_search(true, settings))
    }

    /// Replace all matching occurrences within `self.find_replace_scope` as a single atomic
    /// undoable command on [`UndoStack`] (`D-27`).
    pub fn replace_all_in_scope(
        &mut self,
        settings: &AppSettings,
    ) -> Result<ReplaceSummary, String> {
        let opts = self.build_hl7_replace_options()?;
        Self::push_history(&mut self.find_history, &opts.find);
        Self::push_history(&mut self.replace_history, &opts.replace);

        let target_indices = self.target_indices_for_find_scope();
        let mut updates: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut total_occurrences = 0usize;

        for rec_idx in target_indices {
            let Some(rec) = self.doc.records.get(rec_idx) else {
                continue;
            };
            if let Some((new_bytes, count)) = replace_in_record(rec, &opts)? {
                updates.push((rec_idx, new_bytes));
                total_occurrences += count;
            }
        }

        let summary = ReplaceSummary {
            occurrences: total_occurrences,
            records_modified: updates.len(),
        };

        if updates.is_empty() {
            self.replace_status_banner = "Replaced 0 occurrences across 0 records".to_string();
            self.status_message = self.replace_status_banner.clone();
            return Ok(summary);
        }

        let desc = match opts.field_restriction {
            Some(addr) => format!("Replace All in {addr} ({total_occurrences} occurrences)"),
            None => format!("Replace All ({total_occurrences} occurrences)"),
        };

        let first_rec = updates[0].0;
        let first_anchor = Some(SelectionAnchor {
            record_index: first_rec,
            segment_index: Some(0),
            address: opts.field_restriction,
        });

        let cmd: Box<dyn EditCommand> = if updates.len() == 1 {
            let (rec_idx, new_bytes) = updates.remove(0);
            Box::new(EditRecordCmd::with_context(
                rec_idx,
                new_bytes,
                desc,
                Some(0),
                opts.field_restriction,
            ))
        } else {
            Box::new(BatchEditRecordsCmd::new(
                desc,
                updates,
                total_occurrences,
                first_anchor,
            ))
        };

        self.execute_command(cmd, settings)
            .map_err(|e| e.to_string())?;
        self.replace_status_banner = format!(
            "Replaced {} occurrences across {} records",
            summary.occurrences, summary.records_modified
        );
        self.status_message = self.replace_status_banner.clone();
        Ok(summary)
    }

    /// Restrict the active search to `field_path` (from the Hit Breakdown popover) and jump to its first hit (`D-26`).
    pub fn restrict_search_to_field_path(
        &mut self,
        field_path: &str,
        settings: &AppSettings,
    ) -> Option<SearchMatch> {
        if field_path != "(segment)" {
            self.search_field_input = field_path.to_string();
        }
        self.last_searched_query.clear();
        self.jump_search(true, settings)
    }

    /// Open the Assign Value to Field dialog, defaulting to the selected field address (EDIT-5).
    pub fn open_assign_field_dialog(&mut self) {
        let default_addr = self
            .selected_field_address()
            .filter(|(_, _, a)| !a.is_header_delimiter_field())
            .map_or_else(|| "PID-3".to_string(), |(_, _, a)| a.to_string());
        self.assign_field_dialog = Some(AssignFieldDialog {
            address_input: default_addr,
            selected_records_only: self.selected_records.len() > 1,
            ..AssignFieldDialog::default()
        });
    }

    /// Open the Replace dialog (EDIT-6).
    pub fn open_replace_dialog(&mut self) {
        self.replace_dialog = Some(ReplaceDialog::default());
    }

    /// Open the Change Message Delimiters dialog (EDIT-9).
    pub fn open_change_delimiters_dialog(&mut self) {
        let cur = self
            .selected_record
            .and_then(|i| self.doc.records.get(i))
            .map_or(Delimiters::STANDARD, Record::delimiters)
            .display_chars();
        self.change_delimiters_dialog = Some(ChangeDelimitersDialog {
            current_delimiters: cur.clone(),
            new_delimiters_input: cur,
            error: None,
        });
    }

    /// Request deletion of the selected segment(s), enforcing the MSH / header guard and
    /// honoring `settings.prompt_before_deleting` (EDIT-7, D-10).
    pub fn request_delete_selected_segment(
        &mut self,
        settings: &AppSettings,
    ) -> Result<(), EditError> {
        let Some(seg_idx) = self.selected_segment else {
            return Ok(());
        };
        self.request_delete_segment_at(seg_idx, settings)
    }

    /// Request deletion of segment `seg_index`, enforcing the `MSH` / header guard at index 0
    /// and honoring `settings.prompt_before_deleting` (`EDIT-7`, `D-10`).
    pub fn request_delete_segment_at(
        &mut self,
        seg_index: usize,
        settings: &AppSettings,
    ) -> Result<(), EditError> {
        let Some(rec_idx) = self.selected_record else {
            return Ok(());
        };
        let Some(rec) = self.doc.records.get(rec_idx) else {
            return Ok(());
        };

        if seg_index == 0 {
            if rec.kind() == RecordKind::Msh {
                self.modal_alert = Some(strings::ERR_CANT_DELETE_MSH.to_string());
                return Err(EditError::CannotDeleteMsh);
            }
            let tag = String::from_utf8_lossy(&rec.kind().tag()).into_owned();
            let err = EditError::CannotDeleteHeaderSegment { tag };
            self.modal_alert = Some(err.to_string());
            return Err(err);
        }

        self.selected_segment = Some(seg_index);
        if settings.prompt_before_deleting {
            self.delete_confirm_dialog = Some(DeleteConfirmDialog::Segments {
                record_index: rec_idx,
                segment_indices: vec![seg_index],
            });
            Ok(())
        } else {
            let cmd = Box::new(DeleteSegmentsCmd::new(rec_idx, vec![seg_index]));
            self.execute_command(cmd, settings).map(|_| ())
        }
    }

    /// Open the modal `Edit Segment...` dialog pre-filled with `seg_index`'s current raw line (`EDIT-2`, `EDIT-10`).
    pub fn open_edit_segment_dialog(&mut self, seg_index: usize) {
        let Some(ref cache) = self.record_cache else {
            return;
        };
        let Some(seg_row) = cache.segments.get(seg_index) else {
            return;
        };
        self.selected_segment = Some(seg_index);
        self.edit_segment_dialog = Some(EditSegmentDialog {
            record_index: cache.record_index,
            segment_index: seg_index,
            text: seg_row.preview.clone(),
            error: None,
        });
    }

    /// Replace segment `seg_index`'s entire contents with `new_segment_text`, committing via
    /// [`EditRecordCmd`] onto [`UndoStack`] and re-reading delimiters if `MSH` was modified (`EDIT-2`, `EDIT-10`).
    pub fn edit_segment_raw(
        &mut self,
        seg_index: usize,
        new_segment_text: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let Some(rec_idx) = self.selected_record else {
            return Err(EditError::RecordOutOfBounds { index: 0, len: 0 });
        };
        let doc_len = self.doc.len();
        let rec = self
            .doc
            .records
            .get(rec_idx)
            .ok_or(EditError::RecordOutOfBounds {
                index: rec_idx,
                len: doc_len,
            })?;
        let seg_len = rec.segments().len();
        if seg_index >= seg_len {
            return Err(EditError::SegmentOutOfBounds {
                index: seg_index,
                len: seg_len,
            });
        }

        let normalized = new_segment_text
            .replace("\r\n", "\r")
            .replace('\n', "\r")
            .trim_matches('\r')
            .to_string();
        if normalized.trim().is_empty() {
            return Err(EditError::CannotEditField {
                address: format!("Segment #{} (cannot be empty)", seg_index + 1),
            });
        }

        let mut new_bytes = Vec::with_capacity(rec.bytes().len() + normalized.len());
        for i in 0..seg_len {
            if i > 0 {
                new_bytes.push(b'\r');
            }
            if i == seg_index {
                new_bytes.extend_from_slice(normalized.as_bytes());
            } else if let Some(existing) = rec.segment_bytes(i) {
                new_bytes.extend_from_slice(existing);
            }
        }

        let tag_label = normalized.get(0..3).unwrap_or("SEG");
        let cmd = Box::new(EditRecordCmd::with_context(
            rec_idx,
            new_bytes,
            format!("Edit Segment #{} ({tag_label})", seg_index + 1),
            Some(seg_index),
            None,
        ));
        let out = self.execute_command(cmd, settings)?;
        self.selected_segment = Some(seg_index);
        self.expanded_segments.insert(seg_index);
        Ok(out)
    }

    /// Return `true` if a new segment can be inserted immediately above `seg_index`
    /// (disabled when `seg_index == 0` and segment 0 is `MSH` or an envelope header).
    pub fn can_insert_segment_above(&self, seg_index: usize) -> bool {
        let Some(rec_idx) = self.selected_record else {
            return false;
        };
        let Some(rec) = self.doc.records.get(rec_idx) else {
            return false;
        };
        if seg_index > rec.segments().len() {
            return false;
        }
        if seg_index == 0
            && matches!(
                rec.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            )
        {
            return false;
        }
        true
    }

    /// Open the compact `Insert Segment Above` prompt for `seg_index` (`EDIT-10`).
    pub fn open_insert_segment_above_dialog(&mut self, seg_index: usize) {
        if !self.can_insert_segment_above(seg_index) {
            return;
        }
        let Some(rec_idx) = self.selected_record else {
            return;
        };
        self.selected_segment = Some(seg_index);
        self.insert_segment_dialog = Some(InsertSegmentDialog {
            record_index: rec_idx,
            mode: InsertSegmentMode::Above(seg_index),
            text: "NTE|1|L|".to_string(),
            error: None,
        });
    }

    /// Open the compact `Insert Segment Below` prompt for `seg_index` (`EDIT-10`).
    pub fn open_insert_segment_below_dialog(&mut self, seg_index: usize) {
        let Some(rec_idx) = self.selected_record else {
            return;
        };
        self.selected_segment = Some(seg_index);
        self.insert_segment_dialog = Some(InsertSegmentDialog {
            record_index: rec_idx,
            mode: InsertSegmentMode::Below(seg_index),
            text: "NTE|1|L|".to_string(),
            error: None,
        });
    }

    /// Open the compact `Insert Segment at End` prompt (`EDIT-10`).
    pub fn open_insert_segment_at_end_dialog(&mut self) {
        let Some(rec_idx) = self.selected_record else {
            return;
        };
        self.insert_segment_dialog = Some(InsertSegmentDialog {
            record_index: rec_idx,
            mode: InsertSegmentMode::AtEnd,
            text: "NTE|1|L|".to_string(),
            error: None,
        });
    }

    /// Insert `segment_text` immediately above `seg_index` (disabled above `MSH` at index 0),
    /// undoable via [`EditRecordCmd`] on [`UndoStack`] (`EDIT-10`).
    pub fn insert_segment_above(
        &mut self,
        seg_index: usize,
        segment_text: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        if !self.can_insert_segment_above(seg_index) {
            return Err(EditError::CannotEditField {
                address: "Cannot insert segment above MSH at index 0".to_string(),
            });
        }
        self.insert_segment_at(seg_index, segment_text, settings)
    }

    /// Insert `segment_text` immediately below `seg_index`, undoable via [`EditRecordCmd`] on [`UndoStack`] (`EDIT-10`).
    pub fn insert_segment_below(
        &mut self,
        seg_index: usize,
        segment_text: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        self.insert_segment_at(seg_index + 1, segment_text, settings)
    }

    /// Append `segment_text` at the end of the active record, undoable via [`EditRecordCmd`] on [`UndoStack`] (`EDIT-10`).
    pub fn insert_segment_at_end(
        &mut self,
        segment_text: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let Some(rec_idx) = self.selected_record else {
            return Err(EditError::RecordOutOfBounds { index: 0, len: 0 });
        };
        let seg_len = self
            .doc
            .records
            .get(rec_idx)
            .map_or(0, |r| r.segments().len());
        self.insert_segment_at(seg_len, segment_text, settings)
    }

    /// Insert `segment_text` at 0-based position `insert_index` (`0..=seg_len`), committing via
    /// [`EditRecordCmd`] onto [`UndoStack`] (`EDIT-10`).
    pub fn insert_segment_at(
        &mut self,
        insert_index: usize,
        segment_text: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let Some(rec_idx) = self.selected_record else {
            return Err(EditError::RecordOutOfBounds { index: 0, len: 0 });
        };
        let doc_len = self.doc.len();
        let rec = self
            .doc
            .records
            .get(rec_idx)
            .ok_or(EditError::RecordOutOfBounds {
                index: rec_idx,
                len: doc_len,
            })?;
        let seg_len = rec.segments().len();
        if insert_index > seg_len {
            return Err(EditError::SegmentOutOfBounds {
                index: insert_index,
                len: seg_len,
            });
        }
        if insert_index == 0
            && matches!(
                rec.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            )
        {
            return Err(EditError::CannotEditField {
                address: "Cannot insert segment above MSH at index 0".to_string(),
            });
        }

        let normalized = segment_text
            .replace("\r\n", "\r")
            .replace('\n', "\r")
            .trim_matches('\r')
            .to_string();
        if normalized.trim().is_empty() {
            return Err(EditError::CannotEditField {
                address: "Segment text cannot be empty".to_string(),
            });
        }

        let mut seg_list: Vec<Vec<u8>> = (0..seg_len)
            .filter_map(|i| rec.segment_bytes(i).map(|b| b.to_vec()))
            .collect();
        seg_list.insert(insert_index, normalized.as_bytes().to_vec());

        let mut new_bytes = Vec::new();
        for (i, seg) in seg_list.iter().enumerate() {
            if i > 0 {
                new_bytes.push(b'\r');
            }
            new_bytes.extend_from_slice(seg);
        }

        let tag_label = normalized.get(0..3).unwrap_or("SEG");
        let cmd = Box::new(EditRecordCmd::with_context(
            rec_idx,
            new_bytes,
            format!("Insert Segment {tag_label} at #{}", insert_index + 1),
            Some(insert_index),
            None,
        ));
        let out = self.execute_command(cmd, settings)?;
        self.selected_segment = Some(insert_index);
        self.expanded_segments.insert(insert_index);
        self.scroll_to_segment = true;
        Ok(out)
    }

    /// Return `true` if `seg_index` can be moved up by one position
    /// (disabled at top bound `0`, or when `seg_index == 1` and segment 0 is `MSH`/header).
    pub fn can_move_segment_up(&self, seg_index: usize) -> bool {
        let Some(rec_idx) = self.selected_record else {
            return false;
        };
        let Some(rec) = self.doc.records.get(rec_idx) else {
            return false;
        };
        let seg_len = rec.segments().len();
        if seg_index == 0 || seg_index >= seg_len {
            return false;
        }
        if seg_index == 1
            && matches!(
                rec.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            )
        {
            return false;
        }
        true
    }

    /// Return `true` if `seg_index` can be moved down by one position
    /// (disabled at bottom bound `seg_len - 1`, or when `seg_index == 0` and segment 0 is `MSH`/header).
    pub fn can_move_segment_down(&self, seg_index: usize) -> bool {
        let Some(rec_idx) = self.selected_record else {
            return false;
        };
        let Some(rec) = self.doc.records.get(rec_idx) else {
            return false;
        };
        let seg_len = rec.segments().len();
        if seg_index + 1 >= seg_len {
            return false;
        }
        if seg_index == 0
            && matches!(
                rec.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            )
        {
            return false;
        }
        true
    }

    /// Move segment `seg_index` up by one position (`seg_index - 1`), committing via
    /// [`EditRecordCmd`] onto [`UndoStack`] (`EDIT-10`).
    pub fn move_segment_up(
        &mut self,
        seg_index: usize,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        if !self.can_move_segment_up(seg_index) {
            return Err(EditError::CannotEditField {
                address: format!("Cannot move segment #{} up", seg_index + 1),
            });
        }
        self.swap_segments(seg_index, seg_index - 1, "Up", settings)
    }

    /// Move segment `seg_index` down by one position (`seg_index + 1`), committing via
    /// [`EditRecordCmd`] onto [`UndoStack`] (`EDIT-10`).
    pub fn move_segment_down(
        &mut self,
        seg_index: usize,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        if !self.can_move_segment_down(seg_index) {
            return Err(EditError::CannotEditField {
                address: format!("Cannot move segment #{} down", seg_index + 1),
            });
        }
        self.swap_segments(seg_index, seg_index + 1, "Down", settings)
    }

    fn swap_segments(
        &mut self,
        from_index: usize,
        to_index: usize,
        dir_label: &str,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let Some(rec_idx) = self.selected_record else {
            return Err(EditError::RecordOutOfBounds { index: 0, len: 0 });
        };
        let doc_len = self.doc.len();
        let rec = self
            .doc
            .records
            .get(rec_idx)
            .ok_or(EditError::RecordOutOfBounds {
                index: rec_idx,
                len: doc_len,
            })?;
        let seg_len = rec.segments().len();
        let mut seg_list: Vec<Vec<u8>> = (0..seg_len)
            .filter_map(|i| rec.segment_bytes(i).map(|b| b.to_vec()))
            .collect();
        seg_list.swap(from_index, to_index);

        let mut new_bytes = Vec::new();
        for (i, seg) in seg_list.iter().enumerate() {
            if i > 0 {
                new_bytes.push(b'\r');
            }
            new_bytes.extend_from_slice(seg);
        }

        let cmd = Box::new(EditRecordCmd::with_context(
            rec_idx,
            new_bytes,
            format!("Move Segment #{} {dir_label}", from_index + 1),
            Some(to_index),
            None,
        ));
        let out = self.execute_command(cmd, settings)?;
        self.selected_segment = Some(to_index);
        self.expanded_segments.insert(to_index);
        self.scroll_to_segment = true;
        Ok(out)
    }

    /// Clear/delete the value of `address` in `seg_index` (trimming trailing empty separators cleanly)
    /// and commit via [`EditRecordCmd`] onto [`UndoStack`] (`EDIT-3`, `EDIT-10`).
    pub fn clear_field_at(
        &mut self,
        seg_index: usize,
        address: FieldAddress,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        if settings.decode_escape_sequences || address.is_header_delimiter_field() {
            self.modal_alert = Some(explain_cant_edit_address(address, settings));
            return Err(EditError::CannotEditField {
                address: address.to_string(),
            });
        }
        let Some(rec_idx) = self.selected_record else {
            return Err(EditError::RecordOutOfBounds { index: 0, len: 0 });
        };
        let doc_len = self.doc.len();
        let rec = self
            .doc
            .records
            .get(rec_idx)
            .ok_or(EditError::RecordOutOfBounds {
                index: rec_idx,
                len: doc_len,
            })?;
        let seg_len = rec.segments().len();
        let old_seg = rec
            .segment_bytes(seg_index)
            .ok_or(EditError::SegmentOutOfBounds {
                index: seg_index,
                len: seg_len,
            })?;
        let new_seg = clear_segment_field(old_seg, rec.delimiters(), &address)?;

        let mut new_bytes = Vec::with_capacity(rec.bytes().len());
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

        let cmd = Box::new(EditRecordCmd::with_context(
            rec_idx,
            new_bytes,
            format!("Clear Field {address}"),
            Some(seg_index),
            Some(address),
        ));
        self.execute_command(cmd, settings)
    }

    /// Request clearing/deleting the currently selected field row, honoring `settings.prompt_before_deleting` (`EDIT-7`).
    pub fn request_clear_selected_field(
        &mut self,
        settings: &AppSettings,
    ) -> Result<(), EditError> {
        let Some(row_idx) = self.selected_field_row else {
            return Ok(());
        };
        self.request_clear_field_row(row_idx, settings)
    }

    /// Request clearing/deleting `row_index` in `record_cache.field_rows`, honoring `settings.prompt_before_deleting` (`EDIT-7`).
    pub fn request_clear_field_row(
        &mut self,
        row_index: usize,
        settings: &AppSettings,
    ) -> Result<(), EditError> {
        self.select_field_row(row_index);
        let Some(ref cache) = self.record_cache else {
            return Ok(());
        };
        let Some(frow) = cache.field_rows.get(row_index) else {
            return Ok(());
        };
        if settings.decode_escape_sequences || frow.is_parsed_marker || !frow.is_editable {
            let addr_label = frow
                .address
                .map_or_else(|| "marker".to_string(), |a| a.to_string());
            let reason = explain_cant_edit_field_row(cache, frow, settings);
            self.modal_alert = Some(reason);
            return Err(EditError::CannotEditField {
                address: addr_label,
            });
        }
        let Some(address) = frow.address else {
            let reason = explain_cant_edit_field_row(cache, frow, settings);
            self.modal_alert = Some(reason);
            return Err(EditError::CannotEditField {
                address: "marker".to_string(),
            });
        };
        if address.is_header_delimiter_field() {
            self.modal_alert = Some(explain_cant_edit_address(address, settings));
            return Err(EditError::CannotEditField {
                address: address.to_string(),
            });
        }

        let rec_idx = cache.record_index;
        let seg_idx = frow.seg_index;
        if settings.prompt_before_deleting {
            self.delete_confirm_dialog = Some(DeleteConfirmDialog::ClearField {
                record_index: rec_idx,
                segment_index: seg_idx,
                address,
            });
            Ok(())
        } else {
            self.clear_field_at(seg_idx, address, settings).map(|_| ())
        }
    }

    /// Request deletion of the specified record indices, honoring `settings.prompt_before_deleting` (EDIT-8).
    pub fn request_delete_records(
        &mut self,
        mut indices: Vec<usize>,
        settings: &AppSettings,
    ) -> Result<(), EditError> {
        indices.sort_unstable();
        indices.dedup();
        indices.retain(|&i| i < self.doc.len());
        if indices.is_empty() {
            return Ok(());
        }

        if settings.prompt_before_deleting {
            self.delete_confirm_dialog = Some(DeleteConfirmDialog::Records { indices });
            Ok(())
        } else {
            let cmd = Box::new(DeleteRecordsCmd::selected(indices));
            self.execute_command(cmd, settings).map(|_| ())
        }
    }

    /// Stable sort of all loaded records by `MSH-7` timestamp (`SORT-1`).
    ///
    /// Undoable and marks `is_sorted = true` so the window title shows `"(sorted)"`.
    pub fn sort_records_by_msh7(
        &mut self,
        direction: SortDirection,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let cmd = Box::new(SortRecordsCmd::by_msh7(direction));
        let out = self.execute_command(cmd, settings)?;
        self.is_sorted = true;
        Ok(out)
    }

    /// Stable sort of all loaded records by any [`FieldAddress`] and [`SortKeyType`] (`SORT-2`).
    pub fn sort_records_by_address(
        &mut self,
        address: FieldAddress,
        key_type: SortKeyType,
        direction: SortDirection,
        settings: &AppSettings,
    ) -> Result<EditOutcome, EditError> {
        let cmd = Box::new(SortRecordsCmd::by_address(address, key_type, direction));
        let out = self.execute_command(cmd, settings)?;
        self.is_sorted = true;
        Ok(out)
    }

    /// Open the "Sort Records by Field..." dialog (`SORT-2`).
    pub fn open_sort_records_dialog(&mut self) {
        let default_addr = self
            .selected_field_address()
            .map_or_else(|| "MSH-7".to_string(), |(_, _, a)| a.to_string());
        let key_type = if default_addr == "MSH-7" {
            SortKeyType::Date
        } else {
            SortKeyType::String
        };
        self.sort_records_dialog = Some(SortRecordsDialog {
            address_input: default_addr,
            key_type,
            direction: SortDirection::Ascending,
            error: None,
        });
    }

    /// Open the Report Builder window, optionally adding the currently selected field (`RPT-1`).
    pub fn open_report_from_selected_field(&mut self) {
        let initial = self.selected_field_address().map(|(_, _, addr)| addr);
        self.report_window
            .open_with_field(initial, &self.doc.records, &self.dictionary);
    }

    /// Poll background MLLP sender and listener tasks, appending any newly received records
    /// with source label `"TCP/IP Listener"` (`NET-13`, `G-5`).
    pub fn poll_network(&mut self, settings: &AppSettings) {
        let incoming = self.network.poll_events();
        if !incoming.is_empty() {
            let auto_scroll = self.network.auto_scroll_received;
            self.append_records(incoming, DataSource::TcpIp, settings);
            if auto_scroll && !self.doc.is_empty() {
                self.select_record(self.doc.len() - 1, false, settings);
            }
        }
    }
}

type LoadedFilesWithSources = (Vec<Record>, Vec<Option<PathBuf>>, Vec<PathBuf>);

fn load_files_with_per_record_sources(
    paths: &[PathBuf],
    recovery_mode: RecoveryMode,
    settings: &AppSettings,
) -> Result<LoadedFilesWithSources, String> {
    let mut sorted_paths = paths.to_vec();
    sorted_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then_with(|| a.cmp(b)));

    let config = ScannerConfig {
        envelope: settings.mllp_envelope(),
        recovery_mode,
        ..ScannerConfig::default()
    };
    let cancel = AtomicBool::new(false);
    let mut all_records = Vec::new();
    let mut all_sources = Vec::new();

    for path in &sorted_paths {
        validate_file_not_binary(path)?;
        let out = load_file(path, config, &cancel).map_err(|e| e.to_string())?;
        let n = out.records.len();
        all_records.extend(out.records);
        all_sources.extend(std::iter::repeat_n(Some(path.clone()), n));
    }
    Ok((all_records, all_sources, sorted_paths))
}

/// Detect binary / non-HL7 file magic headers (`.docx`/OOXML `PK\x03\x04`, `%PDF-`, PNG/JPEG/GIF,
/// ELF/PE/OLE2) or embedded NUL bytes (`\0`, excluding HL7 `CR NUL` segment boundaries in files).
///
/// Legitimate `.zip` and `.gz` HL7 archives are permitted when opened via file path.
pub fn detect_binary_payload(bytes: &[u8], path: Option<&std::path::Path>) -> Option<&'static str> {
    let ext_lower = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    // Allow legitimate HL7 archive files (.zip, .gz) to pass through to transparent decompression.
    if matches!(ext_lower.as_deref(), Some("zip" | "gz")) {
        return None;
    }

    if matches!(
        ext_lower.as_deref(),
        Some(
            "docx"
                | "xlsx"
                | "pptx"
                | "doc"
                | "xls"
                | "ppt"
                | "pdf"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "bmp"
                | "ico"
                | "webp"
                | "exe"
                | "dll"
                | "so"
                | "dylib"
                | "bin"
                | "wasm"
        )
    ) {
        return Some("Unsupported binary file or non-HL7 content.");
    }

    let is_magic_binary = bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
        || bytes.starts_with(b"%PDF-")
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"\x7FELF")
        || bytes.starts_with(b"\xD0\xCF\x11\xE0");
    if is_magic_binary {
        return Some("Unsupported binary file or non-HL7 content.");
    }

    // Reject embedded NUL bytes (for files, allow FO-6 `CR NUL` `\r\0` terminators).
    let has_disallowed_nul = if path.is_some() {
        bytes
            .iter()
            .enumerate()
            .any(|(i, &b)| b == 0 && !(i > 0 && bytes[i - 1] == b'\r'))
    } else {
        bytes.contains(&0)
    };
    if has_disallowed_nul {
        return Some("Unsupported binary file or non-HL7 content.");
    }

    None
}

fn validate_file_not_binary(path: &std::path::Path) -> Result<(), String> {
    if let Some(reason) = detect_binary_payload(&[], Some(path)) {
        return Err(format!("{}: {reason}", path.display()));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open '{}': {e}", path.display()))?;
    let mut prefix = [0u8; 512];
    let n = std::io::Read::read(&mut file, &mut prefix)
        .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;
    if let Some(reason) = detect_binary_payload(&prefix[..n], Some(path)) {
        return Err(format!("{}: {reason}", path.display()));
    }
    Ok(())
}

/// Normalize stray Word/Outlook rich-text artifacts around segment boundaries (UTF-8 BOM `\u{FEFF}`,
/// zero-width spaces `\u{200B}`, and non-breaking spaces `\u{00A0}` before `MSH`/`FHS`/`BHS`).
fn sanitize_pasted_hl7_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, raw_line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let stripped = raw_line.trim_start_matches(['\u{FEFF}', '\u{200B}', '\u{00A0}']);
        let trimmed_ws = stripped.trim_start_matches([' ', '\t']);
        let line = if starts_with_hl7_header_bytes(trimmed_ws.as_bytes()) {
            trimmed_ws
        } else {
            stripped
        };
        out.push_str(line);
    }
    out
}

fn starts_with_hl7_header_bytes(bytes: &[u8]) -> bool {
    hl7_core::Delimiters::from_header_bytes(bytes).is_some()
}

fn has_valid_hl7_header(text: &str, envelope: hl7_core::MllpEnvelope) -> bool {
    for raw_line in text.split(['\r', '\n']) {
        let mut bytes = raw_line.as_bytes();
        while bytes.first().copied() == Some(envelope.start_block) {
            bytes = &bytes[1..];
        }
        if starts_with_hl7_header_bytes(bytes) {
            return true;
        }
    }
    false
}

/// Auto-escape any delimiter characters that would corrupt HL7 segment structure at `addr`'s
/// hierarchy level (while preserving valid lower-level composite delimiters on parent fields):
/// - Whole-field edit (`component == None`): escape field separator (`|`) -> `\F\`.
/// - Component edit (`component == Some`, `subcomponent == None`): escape `|` -> `\F\`,
///   `^` -> `\S\`, `~` -> `\R\`.
/// - Subcomponent edit (`subcomponent == Some`): escape `|`, `^`, `~`, `&` -> `\F\`, `\S\`, `\R\`, `\T\`.
fn escape_disallowed_inline_delimiters(
    value: &str,
    addr: FieldAddress,
    delims: Delimiters,
) -> String {
    let field_ch = char::from(delims.field);
    let comp_ch = char::from(delims.component);
    let rep_ch = char::from(delims.repetition);
    let sub_ch = char::from(delims.subcomponent);
    let esc_ch = char::from(delims.escape);

    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        let code = if ch == field_ch {
            Some('F')
        } else if addr.component.is_some() && ch == comp_ch {
            Some('S')
        } else if (addr.component.is_some() || addr.repetition.is_some()) && ch == rep_ch {
            Some('R')
        } else if addr.subcomponent.is_some() && ch == sub_ch {
            Some('T')
        } else {
            None
        };
        if let Some(c) = code {
            out.push(esc_ch);
            out.push(c);
            out.push(esc_ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Compute the byte range of `addr` inside a decoded single-segment string for raw-pane highlighting.
fn locate_field_in_segment_text(
    seg_text: &str,
    base_offset: usize,
    addr: Option<&FieldAddress>,
    delims: hl7_core::Delimiters,
) -> Option<Range<usize>> {
    let addr = addr?;
    let bytes = seg_text.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    let is_header = addr.segment == *b"MSH" || addr.segment == *b"FHS" || addr.segment == *b"BHS";
    if is_header {
        if addr.field.get() == 1 {
            return Some((base_offset + 3)..(base_offset + 4));
        }
        if addr.field.get() == 2 {
            let end = bytes.len().min(8);
            return Some((base_offset + 4)..(base_offset + end));
        }
    }

    let target_idx = if is_header {
        usize::from(addr.field.get().saturating_sub(2))
    } else {
        usize::from(addr.field.get().saturating_sub(1))
    };

    let mut cur_idx = 0usize;
    let mut field_start = 4usize;
    for i in 4..=bytes.len() {
        if i == bytes.len() || bytes[i] == delims.field {
            if cur_idx == target_idx {
                return Some((base_offset + field_start)..(base_offset + i));
            }
            cur_idx += 1;
            field_start = i + 1;
        }
    }
    None
}

/// Threshold above which `Keep Only Filtered` prompts for confirmation and frees dropped
/// records directly without retaining them in `UndoStack`.
pub const PRUNE_CONFIRM_THRESHOLD: usize = 1_000;

/// Unified operator dropdown choices for the single-row Filter bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterBuilderOp {
    /// Exact equality (`PATH = VALUE`).
    #[default]
    Equals,
    /// Inequality (`PATH != VALUE`).
    NotEquals,
    /// Substring containment (`PATH ~= VALUE`).
    Contains,
    /// Regular expression match (`PATH =~ PATTERN`).
    Regex,
    /// Strictly greater than (`PATH > VALUE`).
    GreaterThan,
    /// Greater than or equal (`PATH >= VALUE`).
    GreaterOrEqual,
    /// Strictly less than (`PATH < VALUE`).
    LessThan,
    /// Less than or equal (`PATH <= VALUE`).
    LessOrEqual,
    /// Non-empty presence check (`PATH exists`).
    Exists,
}

impl FilterBuilderOp {
    /// All supported operators in dropdown display order.
    pub const ALL: [Self; 9] = [
        Self::Equals,
        Self::NotEquals,
        Self::Contains,
        Self::Regex,
        Self::GreaterThan,
        Self::GreaterOrEqual,
        Self::LessThan,
        Self::LessOrEqual,
        Self::Exists,
    ];

    /// Display label for the Filter bar operator dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Self::Equals => "Equals (=)",
            Self::NotEquals => "Not Equals (!=)",
            Self::Contains => "Contains (~=)",
            Self::Regex => "Regex (=~)",
            Self::GreaterThan => "Greater Than (>)",
            Self::GreaterOrEqual => "Greater or Equal (>=)",
            Self::LessThan => "Less Than (<)",
            Self::LessOrEqual => "Less or Equal (<=)",
            Self::Exists => "Is Present (non-empty)",
        }
    }

    /// Returns `true` when this operator takes a right-hand value argument.
    pub fn requires_value(self) -> bool {
        !matches!(self, Self::Exists)
    }

    /// Serialize `(path, value)` into a canonical `RecordFilter` expression with clear spacing.
    pub fn format_expression(self, path: &str, value: &str) -> Option<String> {
        let p = path.trim();
        if p.is_empty() {
            return None;
        }
        let v = value.trim();
        match self {
            Self::Equals => Some(format!("{p} = {v}")),
            Self::NotEquals => Some(format!("{p} != {v}")),
            Self::Contains => Some(format!("{p} ~= {v}")),
            Self::Regex => Some(format!("{p} =~ {v}")),
            Self::GreaterThan => Some(format!("{p} > {v}")),
            Self::GreaterOrEqual => Some(format!("{p} >= {v}")),
            Self::LessThan => Some(format!("{p} < {v}")),
            Self::LessOrEqual => Some(format!("{p} <= {v}")),
            Self::Exists => Some(format!("{p} exists")),
        }
    }
}

/// How multiple stacked filter rules in the Filter bar are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterCombineMode {
    /// A message matches only if it satisfies **all** active filter rules (`AND`).
    #[default]
    And,
    /// A message matches if it satisfies **any** active filter rule (`OR`).
    Or,
}

/// One stacked filter rule rendered as a removable pill/chip in the Filter bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveFilterRule {
    /// Filter expression string (e.g. `PID-5.1=TESTPATIENT` or `MSH-9.2=A01`).
    pub query: String,
    /// Case-sensitive flag for this rule.
    pub case_sensitive: bool,
    /// Regular expression flag for this rule.
    pub use_regex: bool,
    /// Invert match (`!v`) flag for this rule.
    pub invert_match: bool,
}

/// Format a single [`FieldViewRow`] as `<PATH> (<Field/Component Name>): <Value>`.
pub fn format_field_row_summary_line(row: &FieldViewRow) -> String {
    let path = row.address.map(|a| a.to_string()).unwrap_or_default();
    match (&row.description, path.is_empty()) {
        (Some(desc), false) => format!("{path} ({desc}): {}", row.display_value),
        (None, false) => format!("{path}: {}", row.display_value),
        (Some(desc), true) => format!("({desc}): {}", row.display_value),
        (None, true) => row.display_value.clone(),
    }
}

/// Format a canonical HL7 filter expression (`<PATH>=<VALUE>`) for the Filter bar,
/// quoting/escaping values so `RecordFilter::parse_with_options` round-trips cleanly.
pub fn format_filter_by_path_query(path: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{path}=\"\"")
    } else if value == "\"\"" {
        format!("{path}=\\\"\\\"")
    } else if value.chars().any(char::is_whitespace)
        || value.starts_with('"')
        || value.ends_with('"')
        || value.starts_with('\'')
        || value.ends_with('\'')
    {
        if !value.contains('"') {
            format!("{path}=\"{value}\"")
        } else if !value.contains('\'') {
            format!("{path}='{value}'")
        } else {
            format!("{path}=\"{value}\"")
        }
    } else {
        format!("{path}={value}")
    }
}

/// Split a compound filter query on top-level `&&` or `||` outside single/double quotes.
fn split_compound_filter_query(input: &str) -> Option<(FilterCombineMode, Vec<String>)> {
    for (delim, mode) in [
        ("&&", FilterCombineMode::And),
        ("||", FilterCombineMode::Or),
    ] {
        let mut parts = Vec::new();
        let mut start = 0usize;
        let mut in_double = false;
        let mut in_single = false;
        let bytes = input.as_bytes();
        let dbytes = delim.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'"' && !in_single {
                in_double = !in_double;
                i += 1;
                continue;
            }
            if b == b'\'' && !in_double {
                in_single = !in_single;
                i += 1;
                continue;
            }
            if !in_double
                && !in_single
                && i + 1 < bytes.len()
                && bytes[i] == dbytes[0]
                && bytes[i + 1] == dbytes[1]
            {
                let part = input[start..i].trim();
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
                i += 2;
                start = i;
                continue;
            }
            i += 1;
        }
        if !parts.is_empty() {
            let tail = input[start..].trim();
            if !tail.is_empty() {
                parts.push(tail.to_string());
            }
            if parts.len() >= 2 {
                return Some((mode, parts));
            }
        }
    }
    None
}

/// Build a specific, actionable explanation for why `address` cannot be edited or cleared.
fn explain_cant_edit_address(address: FieldAddress, settings: &AppSettings) -> String {
    if address.is_header_delimiter_field() {
        let tag = std::str::from_utf8(&address.segment).unwrap_or("MSH");
        return format!(
            "Can't edit {address}: structural message delimiters ({tag}-1 Field Separator and {tag}-2 Encoding Characters) cannot be edited inline.\n\nUse Edit → Change Message Delimiters... or edit directly in the Raw Message Editor."
        );
    }
    if settings.decode_escape_sequences {
        return format!(
            "Can't edit {address} while Decode Escape Sequences (\\*) is active.\n\nInline editing in the Segment Accordion is disabled in decoded preview mode so decoded text doesn't overwrite raw HL7 escape sequences (such as \\F\\, \\S\\, \\T\\, \\R\\, \\E\\, or \\.br\\).\n\nToggle Decode Escape Sequences (\\*) off in the Segment Accordion header (or View menu) to edit inline, or edit directly in the Raw Message Editor."
        );
    }
    format!(
        "Can't edit {address}: this field is currently read-only. Edit directly in the Raw Message Editor instead."
    )
}

/// Build a specific, actionable explanation for why a Segment Accordion row cannot be edited.
fn explain_cant_edit_field_row(
    cache: &SelectedRecordCache,
    frow: &FieldViewRow,
    settings: &AppSettings,
) -> String {
    if let Some(address) = frow.address {
        if address.is_header_delimiter_field() {
            return explain_cant_edit_address(address, settings);
        }
    }
    if settings.decode_escape_sequences {
        let addr_label = frow
            .address
            .map_or_else(|| "this field".to_string(), |a| a.to_string());
        return format!(
            "Can't edit {addr_label} while Decode Escape Sequences (\\*) is active.\n\nInline editing in the Segment Accordion is disabled in decoded preview mode so decoded text doesn't overwrite raw HL7 escape sequences (such as \\F\\, \\S\\, \\T\\, \\R\\, \\E\\, or \\.br\\).\n\nToggle Decode Escape Sequences (\\*) off in the Segment Accordion header (or View menu) to edit inline, or edit directly in the Raw Message Editor."
        );
    }
    if frow.is_parsed_marker || frow.address.is_none() {
        return "Can't edit this row: it is a read-only structural marker rather than an editable HL7 field.\n\nSelect an individual field row below it, or edit directly in the Raw Message Editor."
            .to_string();
    }
    if let Some(address) = frow.address {
        let has_nested_children = cache.field_rows.iter().any(|other| {
            other.seg_index == frow.seg_index
                && other.row_index != frow.row_index
                && other.address.is_some_and(|oa| {
                    oa.field == address.field
                        && ((address.component.is_none()
                            && (oa.component.is_some() || oa.repetition.is_some()))
                            || (address.component.is_some()
                                && address.subcomponent.is_none()
                                && oa.component == address.component
                                && oa.repetition == address.repetition
                                && oa.subcomponent.is_some()))
                })
        });
        if has_nested_children {
            return format!(
                "Can't edit {address} directly because it contains nested components, subcomponents, or repetitions.\n\nExpand the segment and edit the individual child component/subcomponent rows, or edit directly in the Raw Message Editor."
            );
        }
        return explain_cant_edit_address(address, settings);
    }
    "Can't edit this row: it is currently read-only. Edit directly in the Raw Message Editor instead."
        .to_string()
}

/// Detect a Base64 / HL7 `ED` attachment for a Segment Accordion row.
///
/// When `address` points to component 5 of a structured `ED` field (e.g. `OBX-5.5` where
/// `OBX-5` is `^Application^PDF^Base64^JVBERi...`), inspects the parent field so the
/// `ED.2`/`ED.3` subtype metadata (`.pdf`, `.rtf`, `.jpg`, etc.) is preserved.
fn detect_attachment_for_entry(
    rec: &Record,
    seg_index: usize,
    address: Option<FieldAddress>,
    display_value: &str,
    delims: Delimiters,
) -> Option<DetectedAttachment> {
    let comp_sep = delims.component as char;
    if let Some(addr) = address {
        if addr.component.is_some_and(|c| c.get() == 5) && addr.subcomponent.is_none() {
            let parent_addr = FieldAddress {
                segment: addr.segment,
                field: addr.field,
                repetition: addr.repetition,
                component: None,
                subcomponent: None,
            };
            if let Some(parent_bytes) = rec.field_bytes(seg_index, &parent_addr) {
                let parent_str = String::from_utf8_lossy(parent_bytes);
                if let Some(att) = detect_and_decode_attachment(&parent_str, comp_sep) {
                    if att.is_ed_format {
                        return Some(att);
                    }
                }
            }
        }
    }
    detect_and_decode_attachment(display_value, comp_sep)
}

/// Normalize HL7 segment terminators (`\r\n` and `\r`) to `\n` for clean multiline display/editing.
pub fn normalize_hl7_for_multiline(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}
