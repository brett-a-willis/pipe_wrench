//! In-memory document editing commands and unlimited-depth undo/redo stack
//! (EDIT-1..EDIT-11, FS-8, G-4, D-8..D-12).

pub mod commands;

pub use commands::{
    clear_segment_field, rewrite_segment_field, AssignFieldCmd, BatchEditRecordsCmd,
    ChangeDelimitersCmd, DeleteRecordsCmd, DeleteSegmentsCmd, EditFieldCmd, EditRecordCmd,
    InsertRecordsCmd, ReorderRecordsCmd, ReplaceCmd, ReplaceScope, SortRecordsCmd,
};

use crate::address::FieldAddress;
use crate::error::EditError;
use crate::record::Record;

/// Default soft memory warning threshold for [`UndoStack`] (128 MiB, D-12).
pub const DEFAULT_UNDO_WARNING_BYTES: usize = 128 * 1024 * 1024;

/// Identifies the user's current selection so it can be restored after edits/undo/redo (EDIT-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionAnchor {
    /// Selected record index.
    pub record_index: usize,
    /// Selected segment index within the record, if any.
    pub segment_index: Option<usize>,
    /// Selected field address within the segment, if any.
    pub address: Option<FieldAddress>,
}

/// Summary returned after applying or undoing an [`EditCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditOutcome {
    /// Number of fields/occurrences/delimiters modified.
    pub replacements: usize,
    /// Selection target to restore in the UI (EDIT-11).
    pub selection_hint: Option<SelectionAnchor>,
}

/// In-memory document operated on by [`EditCommand`]s (G-4).
#[derive(Debug, Default)]
pub struct Document {
    /// Loaded records in display order.
    pub records: Vec<Record>,
}

impl Document {
    /// Create an empty document.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Wrap an existing `Vec<Record>` in a `Document`.
    pub fn from_records(records: Vec<Record>) -> Self {
        Self { records }
    }

    /// Number of records in the document.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns `true` if the document contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Command pattern interface for all document mutations (EDIT-1).
pub trait EditCommand: Send + std::fmt::Debug {
    /// Human-readable label for Undo/Redo UI menus.
    fn description(&self) -> String;

    /// Apply (or re-apply on redo) the mutation to `doc`.
    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError>;

    /// Revert the mutation on `doc`, restoring exact prior bytes.
    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError>;

    /// Approximate heap bytes retained by this command for memory-bound warnings (EDIT-1).
    fn estimated_bytes(&self) -> usize;
}

/// Soft warning emitted when undo history memory crosses the configured threshold (EDIT-1, D-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndoWarning {
    /// Current estimated bytes retained across undo + redo stacks.
    pub current_bytes: usize,
    /// Configured warning threshold in bytes.
    pub limit_bytes: usize,
}

/// Unlimited-depth, in-memory undo/redo history with soft memory warning and dirty tracking
/// (EDIT-1, FS-8, D-12).
#[derive(Debug)]
pub struct UndoStack {
    undo_entries: Vec<(u64, Box<dyn EditCommand>)>,
    redo_entries: Vec<(u64, Box<dyn EditCommand>)>,
    next_generation: u64,
    saved_generation: Option<u64>,
    estimated_bytes: usize,
    memory_warning_bytes: usize,
    warning_emitted: bool,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new(DEFAULT_UNDO_WARNING_BYTES)
    }
}

impl UndoStack {
    /// Create a new empty undo stack with a configurable soft memory warning threshold.
    pub fn new(memory_warning_bytes: usize) -> Self {
        Self {
            undo_entries: Vec::new(),
            redo_entries: Vec::new(),
            next_generation: 0,
            saved_generation: Some(0),
            estimated_bytes: 0,
            memory_warning_bytes,
            warning_emitted: false,
        }
    }

    /// Execute `cmd` against `doc`, push it onto the undo stack, clear redo history, and return
    /// an optional [`UndoWarning`] if memory crosses the soft threshold.
    pub fn execute(
        &mut self,
        mut cmd: Box<dyn EditCommand>,
        doc: &mut Document,
    ) -> Result<(EditOutcome, Option<UndoWarning>), EditError> {
        let outcome = cmd.apply(doc)?;

        // Discard redo branch and update estimated bytes.
        for (gen, old_cmd) in self.redo_entries.drain(..) {
            self.estimated_bytes = self
                .estimated_bytes
                .saturating_sub(old_cmd.estimated_bytes());
            if self.saved_generation == Some(gen) {
                self.saved_generation = None;
            }
        }

        self.next_generation += 1;
        let gen = self.next_generation;
        self.estimated_bytes = self.estimated_bytes.saturating_add(cmd.estimated_bytes());
        self.undo_entries.push((gen, cmd));

        let warning = if !self.warning_emitted && self.estimated_bytes >= self.memory_warning_bytes
        {
            self.warning_emitted = true;
            Some(UndoWarning {
                current_bytes: self.estimated_bytes,
                limit_bytes: self.memory_warning_bytes,
            })
        } else {
            None
        };

        Ok((outcome, warning))
    }

    /// Undo the most recent command, moving it to the redo stack.
    pub fn undo(&mut self, doc: &mut Document) -> Result<Option<EditOutcome>, EditError> {
        let Some((gen, mut cmd)) = self.undo_entries.pop() else {
            return Ok(None);
        };
        let outcome = cmd.undo(doc)?;
        self.redo_entries.push((gen, cmd));
        Ok(Some(outcome))
    }

    /// Redo the most recently undone command, moving it back to the undo stack.
    pub fn redo(&mut self, doc: &mut Document) -> Result<Option<EditOutcome>, EditError> {
        let Some((gen, mut cmd)) = self.redo_entries.pop() else {
            return Ok(None);
        };
        let outcome = cmd.apply(doc)?;
        self.undo_entries.push((gen, cmd));
        Ok(Some(outcome))
    }

    /// Returns `true` if at least one command can be undone.
    pub fn can_undo(&self) -> bool {
        !self.undo_entries.is_empty()
    }

    /// Returns `true` if at least one command can be redone.
    pub fn can_redo(&self) -> bool {
        !self.redo_entries.is_empty()
    }

    /// Description of the next command to undo.
    pub fn undo_description(&self) -> Option<String> {
        self.undo_entries.last().map(|(_, c)| c.description())
    }

    /// Description of the next command to redo.
    pub fn redo_description(&self) -> Option<String> {
        self.redo_entries.last().map(|(_, c)| c.description())
    }

    /// Mark the current document generation as saved/clean (FS-8).
    pub fn mark_saved(&mut self) {
        self.saved_generation = Some(self.current_generation());
    }

    /// Returns `true` if the document has unsaved changes relative to the last [`Self::mark_saved`] (FS-8).
    pub fn is_dirty(&self) -> bool {
        self.saved_generation != Some(self.current_generation())
    }

    /// Current estimated heap memory held by undo/redo commands.
    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }

    /// Update the soft memory warning threshold and re-arm the warning if now below it.
    pub fn set_memory_warning_bytes(&mut self, limit_bytes: usize) {
        self.memory_warning_bytes = limit_bytes;
        if self.estimated_bytes < self.memory_warning_bytes {
            self.warning_emitted = false;
        }
    }

    /// Clear all undo and redo history. Preserves dirty state if the document was already dirty.
    pub fn clear(&mut self) {
        let was_dirty = self.is_dirty();
        self.undo_entries.clear();
        self.redo_entries.clear();
        self.estimated_bytes = 0;
        self.warning_emitted = false;
        self.saved_generation = if was_dirty { None } else { Some(0) };
        self.next_generation = 0;
    }

    fn current_generation(&self) -> u64 {
        self.undo_entries.last().map_or(0, |(gen, _)| *gen)
    }
}
