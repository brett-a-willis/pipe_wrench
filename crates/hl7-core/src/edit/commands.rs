//! Concrete [`EditCommand`] implementations for Phase 2 (EDIT-2..EDIT-11).

use crate::address::FieldAddress;
use crate::delimiters::Delimiters;
use crate::edit::{Document, EditCommand, EditOutcome, SelectionAnchor};
use crate::error::EditError;
use crate::record::{Record, RecordKind};
use crate::timestamp::{compare_typed_values, SortDirection, SortKeyType};

#[derive(Debug, Clone)]
struct RecordSnapshot {
    bytes: Vec<u8>,
    kind: RecordKind,
    delimiters: Delimiters,
    edited: bool,
}

impl RecordSnapshot {
    fn capture(rec: &Record) -> Self {
        Self {
            bytes: rec.bytes().to_vec(),
            kind: rec.kind(),
            delimiters: rec.delimiters(),
            edited: rec.is_edited(),
        }
    }

    fn restore(&self, rec: &mut Record) {
        rec.restore_raw_state(self.bytes.clone(), self.kind, self.delimiters, self.edited);
    }
}

#[derive(Debug, Clone)]
struct SegmentDiff {
    record_index: usize,
    segment_index: usize,
    old_segment: Vec<u8>,
    was_edited: bool,
    new_segment: Vec<u8>,
}

// ---------------------------------------------------------------------------
// EDIT-2: EditRecordCmd (raw multiline record edit)
// ---------------------------------------------------------------------------

/// Replace an entire record's raw bytes (EDIT-2). Delimiters are re-read on commit.
#[derive(Debug)]
pub struct EditRecordCmd {
    record_index: usize,
    new_bytes: Vec<u8>,
    old_snapshot: Option<RecordSnapshot>,
    custom_description: Option<String>,
    target_segment_index: Option<usize>,
    target_address: Option<FieldAddress>,
}

impl EditRecordCmd {
    /// Create a command to replace `record_index` with CR-separated `new_bytes`.
    pub fn new(record_index: usize, new_bytes: Vec<u8>) -> Self {
        Self {
            record_index,
            new_bytes,
            old_snapshot: None,
            custom_description: None,
            target_segment_index: Some(0),
            target_address: None,
        }
    }

    /// Create a command to replace `record_index` with CR-separated `new_bytes`, specifying
    /// a custom undo description and target segment/field selection anchor (`EDIT-2`, `EDIT-10`, `EDIT-11`).
    pub fn with_context(
        record_index: usize,
        new_bytes: Vec<u8>,
        description: impl Into<String>,
        target_segment_index: Option<usize>,
        target_address: Option<FieldAddress>,
    ) -> Self {
        Self {
            record_index,
            new_bytes,
            old_snapshot: None,
            custom_description: Some(description.into()),
            target_segment_index,
            target_address,
        }
    }
}

impl EditCommand for EditRecordCmd {
    fn description(&self) -> String {
        self.custom_description
            .clone()
            .unwrap_or_else(|| format!("Edit Record #{}", self.record_index + 1))
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        let rec = doc
            .records
            .get_mut(self.record_index)
            .ok_or(EditError::RecordOutOfBounds {
                index: self.record_index,
                len,
            })?;
        if self.old_snapshot.is_none() {
            self.old_snapshot = Some(RecordSnapshot::capture(rec));
        }
        let _ = rec.replace_all_bytes(self.new_bytes.clone());
        let clamped_seg = self
            .target_segment_index
            .map(|idx| idx.min(rec.segments().len().saturating_sub(1)));
        Ok(EditOutcome {
            replacements: 1,
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: clamped_seg,
                address: self.target_address,
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        let rec = doc
            .records
            .get_mut(self.record_index)
            .ok_or(EditError::RecordOutOfBounds {
                index: self.record_index,
                len,
            })?;
        if let Some(ref snap) = self.old_snapshot {
            snap.restore(rec);
        }
        let clamped_seg = self
            .target_segment_index
            .map(|idx| idx.min(rec.segments().len().saturating_sub(1)));
        Ok(EditOutcome {
            replacements: 1,
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: clamped_seg,
                address: self.target_address,
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.new_bytes.len() + self.old_snapshot.as_ref().map_or(0, |s| s.bytes.len())
    }
}

/// Atomic multi-record raw replacement command for batch `Replace All` (`EDIT-6`, `D-27`).
///
/// Captures pre-edit [`RecordSnapshot`]s only for modified records so a single `Undo`
/// restores every affected record cleanly and `Redo` reapplies the batch.
#[derive(Debug)]
pub struct BatchEditRecordsCmd {
    description: String,
    updates: Vec<(usize, Vec<u8>)>,
    snapshots: Vec<(usize, RecordSnapshot)>,
    total_occurrences: usize,
    selection_hint: Option<SelectionAnchor>,
}

impl BatchEditRecordsCmd {
    /// Create a batch command applying `(record_index, new_cr_bytes)` updates atomically.
    pub fn new(
        description: impl Into<String>,
        updates: Vec<(usize, Vec<u8>)>,
        total_occurrences: usize,
        selection_hint: Option<SelectionAnchor>,
    ) -> Self {
        Self {
            description: description.into(),
            updates,
            snapshots: Vec::new(),
            total_occurrences,
            selection_hint,
        }
    }
}

impl EditCommand for BatchEditRecordsCmd {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        for &(idx, _) in &self.updates {
            if idx >= len {
                return Err(EditError::RecordOutOfBounds { index: idx, len });
            }
        }
        if self.snapshots.is_empty() {
            self.snapshots = self
                .updates
                .iter()
                .map(|(idx, _)| (*idx, RecordSnapshot::capture(&doc.records[*idx])))
                .collect();
        }
        for (idx, new_bytes) in &self.updates {
            let _ = doc.records[*idx].replace_all_bytes(new_bytes.clone());
        }
        Ok(EditOutcome {
            replacements: self.total_occurrences,
            selection_hint: self.selection_hint.clone(),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        for (idx, snap) in &self.snapshots {
            let rec = doc
                .records
                .get_mut(*idx)
                .ok_or(EditError::RecordOutOfBounds { index: *idx, len })?;
            snap.restore(rec);
        }
        Ok(EditOutcome {
            replacements: self.total_occurrences,
            selection_hint: self.selection_hint.clone(),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.updates.iter().map(|(_, b)| b.len()).sum::<usize>()
            + self
                .snapshots
                .iter()
                .map(|(_, s)| s.bytes.len())
                .sum::<usize>()
    }
}

// ---------------------------------------------------------------------------
// Phase 15: InsertRecordsCmd & ReorderRecordsCmd (D-29)
// ---------------------------------------------------------------------------

/// Insert one or more [`Record`]s at `insert_index` (`0..=doc.len()`) (`EDIT-10`, `D-29`).
///
/// Used by both global clipboard paste (`Ctrl+V`) and the bottom Record List `+` button.
#[derive(Debug)]
pub struct InsertRecordsCmd {
    insert_index: usize,
    records: Vec<Record>,
    description: String,
    inserted_count: usize,
}

impl InsertRecordsCmd {
    /// Create an insert-records command inserting `records` at `insert_index`.
    pub fn new(insert_index: usize, records: Vec<Record>, description: impl Into<String>) -> Self {
        let inserted_count = records.len();
        Self {
            insert_index,
            records,
            description: description.into(),
            inserted_count,
        }
    }
}

impl EditCommand for InsertRecordsCmd {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        if self.insert_index > len {
            return Err(EditError::RecordOutOfBounds {
                index: self.insert_index,
                len,
            });
        }
        let count = self.records.len();
        self.inserted_count = count;
        doc.records
            .splice(self.insert_index..self.insert_index, self.records.drain(..));

        Ok(EditOutcome {
            replacements: count,
            selection_hint: (!doc.is_empty()).then_some(SelectionAnchor {
                record_index: self.insert_index.min(doc.len() - 1),
                segment_index: Some(0),
                address: None,
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let len = doc.len();
        let end = self.insert_index.saturating_add(self.inserted_count);
        if end > len {
            return Err(EditError::RecordOutOfBounds { index: end, len });
        }
        self.records = doc.records.drain(self.insert_index..end).collect();
        let next_sel = if doc.is_empty() {
            None
        } else {
            Some(SelectionAnchor {
                record_index: self.insert_index.saturating_sub(1).min(doc.len() - 1),
                segment_index: Some(0),
                address: None,
            })
        };
        Ok(EditOutcome {
            replacements: self.inserted_count,
            selection_hint: next_sel,
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.records.iter().map(|r| r.bytes().len()).sum()
    }
}

/// Reorder records via a permutation `permutation[new_idx] = old_idx` (`G-4`, `D-29`).
///
/// Used by Move Record Up (`↑`), Move Record Down (`↓`), and Drag-and-Drop Row Reordering.
#[derive(Debug)]
pub struct ReorderRecordsCmd {
    description: String,
    permutation: Vec<usize>,
    selection_after_apply: Option<usize>,
    selection_after_undo: Option<usize>,
}

impl ReorderRecordsCmd {
    /// Create a reorder command where `permutation[new_idx] = old_idx`.
    pub fn new(
        description: impl Into<String>,
        permutation: Vec<usize>,
        selection_after_apply: Option<usize>,
        selection_after_undo: Option<usize>,
    ) -> Self {
        Self {
            description: description.into(),
            permutation,
            selection_after_apply,
            selection_after_undo,
        }
    }
}

impl EditCommand for ReorderRecordsCmd {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let n = doc.len();
        if self.permutation.len() != n {
            return Err(EditError::RecordOutOfBounds {
                index: self.permutation.len(),
                len: n,
            });
        }
        let mut slots: Vec<Option<Record>> = doc.records.drain(..).map(Some).collect();
        for &orig_idx in &self.permutation {
            if let Some(rec) = slots.get_mut(orig_idx).and_then(Option::take) {
                doc.records.push(rec);
            }
        }
        Ok(EditOutcome {
            replacements: 1,
            selection_hint: self.selection_after_apply.and_then(|idx| {
                (!doc.is_empty()).then_some(SelectionAnchor {
                    record_index: idx.min(doc.len() - 1),
                    segment_index: Some(0),
                    address: None,
                })
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let n = doc.len();
        if self.permutation.len() == n && n > 1 {
            let mut slots: Vec<Option<Record>> = (0..n).map(|_| None).collect();
            for (new_idx, rec) in doc.records.drain(..).enumerate() {
                let orig_idx = self.permutation[new_idx];
                if orig_idx < n {
                    slots[orig_idx] = Some(rec);
                }
            }
            doc.records = slots.into_iter().flatten().collect();
        }
        Ok(EditOutcome {
            replacements: 1,
            selection_hint: self.selection_after_undo.and_then(|idx| {
                (!doc.is_empty()).then_some(SelectionAnchor {
                    record_index: idx.min(doc.len() - 1),
                    segment_index: Some(0),
                    address: None,
                })
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.permutation.len() * std::mem::size_of::<usize>()
    }
}

// ---------------------------------------------------------------------------
// EDIT-3 / EDIT-4: EditFieldCmd
// ---------------------------------------------------------------------------

/// Edit a single field, repetition, component, or subcomponent (EDIT-3, EDIT-4, D-9).
#[derive(Debug)]
pub struct EditFieldCmd {
    record_index: usize,
    segment_index: usize,
    address: FieldAddress,
    new_value: Vec<u8>,
    escape_delimiters: bool,
    diff: Option<SegmentDiff>,
}

impl EditFieldCmd {
    /// Create a field-edit command.
    ///
    /// When `escape_delimiters` is `false`, any active delimiter byte in `new_value` returns
    /// [`EditError::DelimiterInValue`]. When `true`, active delimiters are encoded via
    /// [`Delimiters::escape_bytes`] (EDIT-4).
    pub fn new(
        record_index: usize,
        segment_index: usize,
        address: FieldAddress,
        new_value: Vec<u8>,
        escape_delimiters: bool,
    ) -> Self {
        Self {
            record_index,
            segment_index,
            address,
            new_value,
            escape_delimiters,
            diff: None,
        }
    }
}

impl EditCommand for EditFieldCmd {
    fn description(&self) -> String {
        format!("Edit {}", self.address)
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        if self.address.is_header_delimiter_field() {
            return Err(EditError::CannotEditField {
                address: self.address.to_string(),
            });
        }
        let doc_len = doc.len();
        let rec = doc
            .records
            .get_mut(self.record_index)
            .ok_or(EditError::RecordOutOfBounds {
                index: self.record_index,
                len: doc_len,
            })?;

        let delims = rec.delimiters();
        let effective_value = if self.escape_delimiters {
            delims.escape_bytes(&self.new_value)
        } else {
            validate_delimiters_for_address(&self.new_value, delims, &self.address)?;
            self.new_value.clone()
        };

        let seg_len = rec.segments().len();
        let old_seg = rec
            .segment_bytes(self.segment_index)
            .ok_or(EditError::SegmentOutOfBounds {
                index: self.segment_index,
                len: seg_len,
            })?
            .to_vec();

        let new_seg = rewrite_segment_field(&old_seg, delims, &self.address, &effective_value)?;
        let was_edited = rec.is_edited();
        let _ = rec.splice_segment(self.segment_index, &new_seg);

        self.diff = Some(SegmentDiff {
            record_index: self.record_index,
            segment_index: self.segment_index,
            old_segment: old_seg,
            was_edited,
            new_segment: new_seg,
        });

        Ok(EditOutcome {
            replacements: 1,
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: Some(self.segment_index),
                address: Some(self.address),
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        if let Some(ref diff) = self.diff {
            let doc_len = doc.len();
            let rec =
                doc.records
                    .get_mut(diff.record_index)
                    .ok_or(EditError::RecordOutOfBounds {
                        index: diff.record_index,
                        len: doc_len,
                    })?;
            let _ = rec.splice_segment(diff.segment_index, &diff.old_segment);
            rec.set_edited(diff.was_edited);
        }
        Ok(EditOutcome {
            replacements: 1,
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: Some(self.segment_index),
                address: Some(self.address),
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.new_value.len()
            + self
                .diff
                .as_ref()
                .map_or(0, |d| d.old_segment.len() + d.new_segment.len())
    }
}

// ---------------------------------------------------------------------------
// EDIT-5: AssignFieldCmd
// ---------------------------------------------------------------------------

/// Assign one value to a field address across selected (or all) records (EDIT-5, D-8).
#[derive(Debug)]
pub struct AssignFieldCmd {
    target_records: Option<Vec<usize>>,
    address: FieldAddress,
    new_value: Vec<u8>,
    first_occurrence_only: bool,
    diffs: Vec<SegmentDiff>,
}

impl AssignFieldCmd {
    /// Create an assign command. `target_records = None` targets all records.
    pub fn new(
        target_records: Option<Vec<usize>>,
        address: FieldAddress,
        new_value: Vec<u8>,
        first_occurrence_only: bool,
    ) -> Self {
        Self {
            target_records,
            address,
            new_value,
            first_occurrence_only,
            diffs: Vec::new(),
        }
    }
}

impl EditCommand for AssignFieldCmd {
    fn description(&self) -> String {
        format!("Assign {}", self.address)
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        if self.address.is_header_delimiter_field() {
            return Err(EditError::CannotEditField {
                address: self.address.to_string(),
            });
        }

        let indices: Vec<usize> = match &self.target_records {
            Some(list) => list.clone(),
            None => (0..doc.len()).collect(),
        };

        // Pre-validate against every target record's delimiters before mutating anything.
        for &rec_idx in &indices {
            let rec = doc
                .records
                .get(rec_idx)
                .ok_or(EditError::RecordOutOfBounds {
                    index: rec_idx,
                    len: doc.len(),
                })?;
            validate_delimiters_for_address(&self.new_value, rec.delimiters(), &self.address)?;
        }

        self.diffs.clear();
        for rec_idx in indices {
            let rec = &mut doc.records[rec_idx];
            let delims = rec.delimiters();
            let was_edited = rec.is_edited();
            let matching_segs: Vec<usize> = rec
                .segments()
                .find_by_tag(&self.address.segment)
                .map(|(i, _)| i)
                .collect();

            for seg_idx in matching_segs {
                let Some(old_seg) = rec.segment_bytes(seg_idx).map(|s| s.to_vec()) else {
                    continue;
                };
                let new_seg =
                    rewrite_segment_field(&old_seg, delims, &self.address, &self.new_value)?;
                let _ = rec.splice_segment(seg_idx, &new_seg);
                self.diffs.push(SegmentDiff {
                    record_index: rec_idx,
                    segment_index: seg_idx,
                    old_segment: old_seg,
                    was_edited,
                    new_segment: new_seg,
                });
                if self.first_occurrence_only {
                    break;
                }
            }
        }

        let first_hint = self.diffs.first().map(|d| SelectionAnchor {
            record_index: d.record_index,
            segment_index: Some(d.segment_index),
            address: Some(self.address),
        });
        Ok(EditOutcome {
            replacements: self.diffs.len(),
            selection_hint: first_hint,
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let count = self.diffs.len();
        for diff in self.diffs.iter().rev() {
            if let Some(rec) = doc.records.get_mut(diff.record_index) {
                let _ = rec.splice_segment(diff.segment_index, &diff.old_segment);
                rec.set_edited(diff.was_edited);
            }
        }
        Ok(EditOutcome {
            replacements: count,
            selection_hint: self.diffs.first().map(|d| SelectionAnchor {
                record_index: d.record_index,
                segment_index: Some(d.segment_index),
                address: Some(self.address),
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.new_value.len()
            + self
                .diffs
                .iter()
                .map(|d| d.old_segment.len() + d.new_segment.len())
                .sum::<usize>()
    }
}

// ---------------------------------------------------------------------------
// EDIT-6: ReplaceCmd (Field, Segment, RecordList scopes)
// ---------------------------------------------------------------------------

/// Target scope for [`ReplaceCmd`] (EDIT-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceScope {
    /// Replace within a single field/component/repetition.
    Field {
        /// Record index.
        record_index: usize,
        /// Segment index.
        segment_index: usize,
        /// Target field address.
        address: FieldAddress,
    },
    /// Replace within all editable leaf values of one segment.
    Segment {
        /// Record index.
        record_index: usize,
        /// Segment index.
        segment_index: usize,
    },
    /// Replace within all editable leaf values across selected (or all) records.
    RecordList {
        /// Record indices (`None` means all loaded records).
        record_indices: Option<Vec<usize>>,
    },
}

/// Find and replace text at field, segment, or record-list scope (EDIT-6).
#[derive(Debug)]
pub struct ReplaceCmd {
    scope: ReplaceScope,
    find: Vec<u8>,
    replace: Vec<u8>,
    case_sensitive: bool,
    diffs: Vec<SegmentDiff>,
    total_replacements: usize,
}

impl ReplaceCmd {
    /// Create a replace command. Both `find` and `replace` are validated against active delimiters.
    pub fn new(scope: ReplaceScope, find: Vec<u8>, replace: Vec<u8>, case_sensitive: bool) -> Self {
        Self {
            scope,
            find,
            replace,
            case_sensitive,
            diffs: Vec::new(),
            total_replacements: 0,
        }
    }
}

impl EditCommand for ReplaceCmd {
    fn description(&self) -> String {
        match self.scope {
            ReplaceScope::Field { address, .. } => format!("Replace in {address}"),
            ReplaceScope::Segment { .. } => "Replace in Segment".to_string(),
            ReplaceScope::RecordList { .. } => "Replace in Records".to_string(),
        }
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        if self.find.is_empty() {
            return Err(EditError::EmptyFindText);
        }

        self.diffs.clear();
        self.total_replacements = 0;

        match self.scope.clone() {
            ReplaceScope::Field {
                record_index,
                segment_index,
                address,
            } => {
                if address.is_header_delimiter_field() {
                    return Err(EditError::CannotEditField {
                        address: address.to_string(),
                    });
                }
                let doc_len = doc.len();
                let rec =
                    doc.records
                        .get_mut(record_index)
                        .ok_or(EditError::RecordOutOfBounds {
                            index: record_index,
                            len: doc_len,
                        })?;
                let delims = rec.delimiters();
                validate_delimiters_for_address(&self.find, delims, &address)?;
                validate_delimiters_for_address(&self.replace, delims, &address)?;

                let seg_len = rec.segments().len();
                let old_seg = rec
                    .segment_bytes(segment_index)
                    .ok_or(EditError::SegmentOutOfBounds {
                        index: segment_index,
                        len: seg_len,
                    })?
                    .to_vec();

                if let Some(current_val) = rec.field_bytes(segment_index, &address) {
                    let (replaced, count) =
                        replace_bytes(current_val, &self.find, &self.replace, self.case_sensitive);
                    if count > 0 {
                        let new_seg = rewrite_segment_field(&old_seg, delims, &address, &replaced)?;
                        let was_edited = rec.is_edited();
                        let _ = rec.splice_segment(segment_index, &new_seg);
                        self.diffs.push(SegmentDiff {
                            record_index,
                            segment_index,
                            old_segment: old_seg,
                            was_edited,
                            new_segment: new_seg,
                        });
                        self.total_replacements = count;
                    }
                }

                Ok(EditOutcome {
                    replacements: self.total_replacements,
                    selection_hint: Some(SelectionAnchor {
                        record_index,
                        segment_index: Some(segment_index),
                        address: Some(address),
                    }),
                })
            }
            ReplaceScope::Segment {
                record_index,
                segment_index,
            } => {
                let doc_len = doc.len();
                let rec =
                    doc.records
                        .get_mut(record_index)
                        .ok_or(EditError::RecordOutOfBounds {
                            index: record_index,
                            len: doc_len,
                        })?;
                let delims = rec.delimiters();
                validate_no_delimiters(&self.find, delims)?;
                validate_no_delimiters(&self.replace, delims)?;

                let seg_len = rec.segments().len();
                let old_seg = rec
                    .segment_bytes(segment_index)
                    .ok_or(EditError::SegmentOutOfBounds {
                        index: segment_index,
                        len: seg_len,
                    })?
                    .to_vec();
                let (new_seg, count) = replace_in_segment(
                    &old_seg,
                    delims,
                    &self.find,
                    &self.replace,
                    self.case_sensitive,
                );
                if count > 0 {
                    let was_edited = rec.is_edited();
                    let _ = rec.splice_segment(segment_index, &new_seg);
                    self.diffs.push(SegmentDiff {
                        record_index,
                        segment_index,
                        old_segment: old_seg,
                        was_edited,
                        new_segment: new_seg,
                    });
                    self.total_replacements = count;
                }

                Ok(EditOutcome {
                    replacements: self.total_replacements,
                    selection_hint: Some(SelectionAnchor {
                        record_index,
                        segment_index: Some(segment_index),
                        address: None,
                    }),
                })
            }
            ReplaceScope::RecordList { record_indices } => {
                let indices: Vec<usize> = match record_indices {
                    Some(list) => list,
                    None => (0..doc.len()).collect(),
                };
                for &rec_idx in &indices {
                    let rec = doc
                        .records
                        .get(rec_idx)
                        .ok_or(EditError::RecordOutOfBounds {
                            index: rec_idx,
                            len: doc.len(),
                        })?;
                    let delims = rec.delimiters();
                    validate_no_delimiters(&self.find, delims)?;
                    validate_no_delimiters(&self.replace, delims)?;
                }

                for rec_idx in indices {
                    let rec = &mut doc.records[rec_idx];
                    let delims = rec.delimiters();
                    let was_edited = rec.is_edited();
                    let seg_count = rec.segments().len();
                    for seg_idx in 0..seg_count {
                        let Some(old_seg) = rec.segment_bytes(seg_idx).map(|s| s.to_vec()) else {
                            continue;
                        };
                        let (new_seg, count) = replace_in_segment(
                            &old_seg,
                            delims,
                            &self.find,
                            &self.replace,
                            self.case_sensitive,
                        );
                        if count > 0 {
                            let _ = rec.splice_segment(seg_idx, &new_seg);
                            self.diffs.push(SegmentDiff {
                                record_index: rec_idx,
                                segment_index: seg_idx,
                                old_segment: old_seg,
                                was_edited,
                                new_segment: new_seg,
                            });
                            self.total_replacements += count;
                        }
                    }
                }

                Ok(EditOutcome {
                    replacements: self.total_replacements,
                    selection_hint: self.diffs.first().map(|d| SelectionAnchor {
                        record_index: d.record_index,
                        segment_index: Some(d.segment_index),
                        address: None,
                    }),
                })
            }
        }
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        for diff in self.diffs.iter().rev() {
            if let Some(rec) = doc.records.get_mut(diff.record_index) {
                let _ = rec.splice_segment(diff.segment_index, &diff.old_segment);
                rec.set_edited(diff.was_edited);
            }
        }
        Ok(EditOutcome {
            replacements: self.total_replacements,
            selection_hint: self.diffs.first().map(|d| SelectionAnchor {
                record_index: d.record_index,
                segment_index: Some(d.segment_index),
                address: None,
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.find.len()
            + self.replace.len()
            + self
                .diffs
                .iter()
                .map(|d| d.old_segment.len() + d.new_segment.len())
                .sum::<usize>()
    }
}

// ---------------------------------------------------------------------------
// EDIT-7: DeleteSegmentsCmd
// ---------------------------------------------------------------------------

/// Delete one or more segments from a record (EDIT-7, D-10).
///
/// Segment index `0` (`MSH` or envelope header) can never be deleted.
#[derive(Debug)]
pub struct DeleteSegmentsCmd {
    record_index: usize,
    segment_indices: Vec<usize>,
    old_snapshot: Option<RecordSnapshot>,
}

impl DeleteSegmentsCmd {
    /// Create a delete-segments command for `record_index`.
    pub fn new(record_index: usize, segment_indices: Vec<usize>) -> Self {
        Self {
            record_index,
            segment_indices,
            old_snapshot: None,
        }
    }
}

impl EditCommand for DeleteSegmentsCmd {
    fn description(&self) -> String {
        format!("Delete {} Segment(s)", self.segment_indices.len())
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let doc_len = doc.len();
        let rec = doc
            .records
            .get_mut(self.record_index)
            .ok_or(EditError::RecordOutOfBounds {
                index: self.record_index,
                len: doc_len,
            })?;

        let mut sorted = self.segment_indices.clone();
        sorted.sort_unstable();
        sorted.dedup();

        let seg_len = rec.segments().len();
        for &idx in &sorted {
            if idx >= seg_len {
                return Err(EditError::SegmentOutOfBounds {
                    index: idx,
                    len: seg_len,
                });
            }
            if idx == 0 {
                if rec.kind() == RecordKind::Msh {
                    return Err(EditError::CannotDeleteMsh);
                }
                let tag = String::from_utf8_lossy(&rec.kind().tag()).into_owned();
                return Err(EditError::CannotDeleteHeaderSegment { tag });
            }
        }

        if self.old_snapshot.is_none() {
            self.old_snapshot = Some(RecordSnapshot::capture(rec));
        }

        let mut new_bytes = Vec::with_capacity(rec.bytes().len());
        let mut first = true;
        for i in 0..seg_len {
            if sorted.binary_search(&i).is_ok() {
                continue;
            }
            if let Some(seg) = rec.segment_bytes(i) {
                if !first {
                    new_bytes.push(b'\r');
                }
                new_bytes.extend_from_slice(seg);
                first = false;
            }
        }
        let _ = rec.replace_all_bytes(new_bytes);

        let next_seg = sorted
            .first()
            .copied()
            .unwrap_or(0)
            .min(rec.segments().len().saturating_sub(1));

        Ok(EditOutcome {
            replacements: sorted.len(),
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: Some(next_seg),
                address: None,
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let doc_len = doc.len();
        let rec = doc
            .records
            .get_mut(self.record_index)
            .ok_or(EditError::RecordOutOfBounds {
                index: self.record_index,
                len: doc_len,
            })?;
        if let Some(ref snap) = self.old_snapshot {
            snap.restore(rec);
        }
        Ok(EditOutcome {
            replacements: self.segment_indices.len(),
            selection_hint: Some(SelectionAnchor {
                record_index: self.record_index,
                segment_index: self.segment_indices.first().copied(),
                address: None,
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.old_snapshot.as_ref().map_or(0, |s| s.bytes.len())
    }
}

// ---------------------------------------------------------------------------
// EDIT-8: DeleteRecordsCmd
// ---------------------------------------------------------------------------

/// Delete selected records, or range deletes ("this through first" / "this through last") (EDIT-8).
#[derive(Debug)]
pub struct DeleteRecordsCmd {
    indices: Vec<usize>,
    removed: Vec<(usize, Record)>,
}

impl DeleteRecordsCmd {
    /// Delete the specified record indices.
    pub fn selected(mut indices: Vec<usize>) -> Self {
        indices.sort_unstable();
        indices.dedup();
        Self {
            indices,
            removed: Vec::new(),
        }
    }

    /// Delete from index `0` through `anchor` inclusive ("this through first", EDIT-8).
    pub fn through_first(anchor: usize) -> Self {
        Self::selected((0..=anchor).collect())
    }

    /// Delete from `anchor` through the final record inclusive ("this through last", EDIT-8).
    pub fn through_last(anchor: usize, doc_len: usize) -> Self {
        if anchor >= doc_len {
            Self::selected(Vec::new())
        } else {
            Self::selected((anchor..doc_len).collect())
        }
    }
}

impl EditCommand for DeleteRecordsCmd {
    fn description(&self) -> String {
        format!("Delete {} Record(s)", self.indices.len())
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let doc_len = doc.len();
        for &idx in &self.indices {
            if idx >= doc_len {
                return Err(EditError::RecordOutOfBounds {
                    index: idx,
                    len: doc_len,
                });
            }
        }

        self.removed.clear();
        for &idx in self.indices.iter().rev() {
            let rec = doc.records.remove(idx);
            self.removed.push((idx, rec));
        }
        self.removed.reverse();

        let next_rec = if doc.is_empty() {
            None
        } else {
            let first = self.indices.first().copied().unwrap_or(0);
            Some(SelectionAnchor {
                record_index: first.min(doc.len() - 1),
                segment_index: Some(0),
                address: None,
            })
        };

        Ok(EditOutcome {
            replacements: self.removed.len(),
            selection_hint: next_rec,
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let count = self.removed.len();
        let first_idx = self.removed.first().map(|(i, _)| *i);
        for (idx, rec) in self.removed.drain(..) {
            doc.records.insert(idx, rec);
        }
        Ok(EditOutcome {
            replacements: count,
            selection_hint: first_idx.map(|record_index| SelectionAnchor {
                record_index,
                segment_index: Some(0),
                address: None,
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.removed.iter().map(|(_, r)| r.bytes().len()).sum()
    }
}

// ---------------------------------------------------------------------------
// EDIT-9: ChangeDelimitersCmd
// ---------------------------------------------------------------------------

/// Change the five message delimiters across all loaded records (EDIT-9, D-11).
#[derive(Debug)]
pub struct ChangeDelimitersCmd {
    new_delims_input: Vec<u8>,
    snapshots: Vec<RecordSnapshot>,
    total_replacements: usize,
}

impl ChangeDelimitersCmd {
    /// Create a delimiter-change command from a user-entered byte slice (must be 5 bytes).
    pub fn new(new_delims_input: Vec<u8>) -> Self {
        Self {
            new_delims_input,
            snapshots: Vec::new(),
            total_replacements: 0,
        }
    }
}

impl EditCommand for ChangeDelimitersCmd {
    fn description(&self) -> String {
        "Change Message Delimiters".to_string()
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        if self.new_delims_input.len() != 5 {
            return Err(EditError::InvalidDelimiterLength {
                got: self.new_delims_input.len(),
            });
        }
        let five: [u8; 5] = [
            self.new_delims_input[0],
            self.new_delims_input[1],
            self.new_delims_input[2],
            self.new_delims_input[3],
            self.new_delims_input[4],
        ];
        let new_delims = Delimiters::from_five_bytes(five)?;

        self.snapshots = doc.records.iter().map(RecordSnapshot::capture).collect();
        self.total_replacements = 0;

        for rec in &mut doc.records {
            let old = rec.delimiters();
            if old == new_delims {
                continue;
            }
            let old_bytes = rec.bytes();
            let mut updated = old_bytes.to_vec();
            let mut count = 0usize;

            let is_header = matches!(
                rec.kind(),
                RecordKind::Msh | RecordKind::Fhs | RecordKind::Bhs
            );
            let start_translate = if is_header && updated.len() >= 8 {
                let new_five = new_delims.as_bytes();
                for i in 0..5 {
                    if updated[3 + i] != new_five[i] {
                        updated[3 + i] = new_five[i];
                        count += 1;
                    }
                }
                8
            } else {
                3
            };

            for b in updated.iter_mut().skip(start_translate) {
                if *b == old.field {
                    *b = new_delims.field;
                    count += 1;
                } else if *b == old.component {
                    *b = new_delims.component;
                    count += 1;
                } else if *b == old.repetition {
                    *b = new_delims.repetition;
                    count += 1;
                } else if *b == old.escape {
                    *b = new_delims.escape;
                    count += 1;
                } else if *b == old.subcomponent {
                    *b = new_delims.subcomponent;
                    count += 1;
                }
            }

            let _ = rec.replace_all_bytes(updated);
            rec.set_delimiters(new_delims);
            self.total_replacements += count;
        }

        Ok(EditOutcome {
            replacements: self.total_replacements,
            selection_hint: (!doc.is_empty()).then_some(SelectionAnchor {
                record_index: 0,
                segment_index: Some(0),
                address: None,
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        for (rec, snap) in doc.records.iter_mut().zip(self.snapshots.iter()) {
            snap.restore(rec);
        }
        Ok(EditOutcome {
            replacements: self.total_replacements,
            selection_hint: (!doc.is_empty()).then_some(SelectionAnchor {
                record_index: 0,
                segment_index: Some(0),
                address: None,
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.snapshots.iter().map(|s| s.bytes.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_no_delimiters(bytes: &[u8], delims: Delimiters) -> Result<(), EditError> {
    if delims.first_delimiter_in(bytes).is_some()
        || bytes.contains(&b'\r')
        || bytes.contains(&b'\n')
    {
        let delimiters = delims.display_chars();
        return Err(EditError::DelimiterInValue {
            message: format!(
                "Values cannot contain any of the active delimiters '{delimiters}' or line breaks."
            ),
            delimiters,
        });
    }
    Ok(())
}

/// Hierarchy-aware delimiter validation based on the target [`FieldAddress`] granularity:
/// - **Field (`SEG-F`):** Allows `~`, `^`, `&`, `\`; rejects field separator (`|`) and `\r`/`\n`.
/// - **Repetition (`SEG-F(R)`):** Allows `^`, `&`, `\`; rejects `|`, `~`, and `\r`/`\n`.
/// - **Component (`SEG-F.C`):** Allows `&`, `\`; rejects `|`, `~`, `^`, and `\r`/`\n`.
/// - **Subcomponent (`SEG-F.C.S`):** Allows `\`; rejects `|`, `~`, `^`, `&`, and `\r`/`\n`.
pub fn validate_delimiters_for_address(
    bytes: &[u8],
    delims: Delimiters,
    addr: &FieldAddress,
) -> Result<(), EditError> {
    if addr.is_header_delimiter_field() {
        return Ok(());
    }

    let (level_label, disallowed_bytes): (&str, Vec<u8>) = if addr.subcomponent.is_some() {
        (
            "Subcomponent values",
            vec![
                delims.field,
                delims.repetition,
                delims.component,
                delims.subcomponent,
            ],
        )
    } else if addr.component.is_some() {
        (
            "Component values",
            vec![delims.field, delims.repetition, delims.component],
        )
    } else if addr.repetition.is_some() {
        ("Repetition values", vec![delims.field, delims.repetition])
    } else {
        ("Field values", vec![delims.field])
    };

    let has_disallowed = bytes
        .iter()
        .any(|b| *b == b'\r' || *b == b'\n' || disallowed_bytes.contains(b));

    if has_disallowed {
        let disallowed_str: String = disallowed_bytes.iter().map(|&b| char::from(b)).collect();
        let message = if disallowed_bytes.len() == 1 {
            format!("{level_label} cannot contain the field separator '{disallowed_str}' or line breaks.")
        } else {
            format!(
                "{level_label} cannot contain the separators '{disallowed_str}' or line breaks."
            )
        };
        return Err(EditError::DelimiterInValue {
            delimiters: disallowed_str,
            message,
        });
    }

    Ok(())
}

/// Rewrite the addressed field, repetition, component, or subcomponent inside `seg` with `new_value`,
/// preserving neighboring components, subcomponents, repetitions, and active delimiters (`EDIT-3`, `D-27`).
pub fn rewrite_segment_field(
    seg: &[u8],
    delims: Delimiters,
    addr: &FieldAddress,
    new_value: &[u8],
) -> Result<Vec<u8>, EditError> {
    if addr.is_header_delimiter_field() {
        return Err(EditError::CannotEditField {
            address: addr.to_string(),
        });
    }
    let is_header = addr.segment == *b"MSH" || addr.segment == *b"FHS" || addr.segment == *b"BHS";

    let mut prefix = Vec::with_capacity(4);
    if seg.len() >= 4 {
        prefix.extend_from_slice(&seg[0..4]);
    } else {
        prefix.extend_from_slice(&addr.segment);
        prefix.push(delims.field);
    }

    let body = seg.get(4..).unwrap_or(&[]);
    let mut fields: Vec<Vec<u8>> = body
        .split(|&b| b == delims.field)
        .map(|s| s.to_vec())
        .collect();

    let field_idx = if is_header {
        usize::from(addr.field.get() - 2)
    } else {
        usize::from(addr.field.get() - 1)
    };
    while fields.len() <= field_idx {
        fields.push(Vec::new());
    }

    fields[field_idx] = rewrite_field_value(&fields[field_idx], delims, addr, new_value);

    let mut out = prefix;
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(delims.field);
        }
        out.extend_from_slice(field);
    }
    Ok(out)
}

fn rewrite_field_value(
    field_bytes: &[u8],
    delims: Delimiters,
    addr: &FieldAddress,
    new_value: &[u8],
) -> Vec<u8> {
    if addr.repetition.is_none() && addr.component.is_none() {
        return new_value.to_vec();
    }

    let mut reps: Vec<Vec<u8>> = field_bytes
        .split(|&b| b == delims.repetition)
        .map(|s| s.to_vec())
        .collect();
    let rep_idx = addr.repetition.map_or(0, |r| usize::from(r.get() - 1));
    while reps.len() <= rep_idx {
        reps.push(Vec::new());
    }

    if let Some(comp) = addr.component {
        let mut comps: Vec<Vec<u8>> = reps[rep_idx]
            .split(|&b| b == delims.component)
            .map(|s| s.to_vec())
            .collect();
        let comp_idx = usize::from(comp.get() - 1);
        while comps.len() <= comp_idx {
            comps.push(Vec::new());
        }

        if let Some(sub) = addr.subcomponent {
            let mut subs: Vec<Vec<u8>> = comps[comp_idx]
                .split(|&b| b == delims.subcomponent)
                .map(|s| s.to_vec())
                .collect();
            let sub_idx = usize::from(sub.get() - 1);
            while subs.len() <= sub_idx {
                subs.push(Vec::new());
            }
            subs[sub_idx] = new_value.to_vec();
            comps[comp_idx] = join_bytes(&subs, delims.subcomponent);
        } else {
            comps[comp_idx] = new_value.to_vec();
        }
        reps[rep_idx] = join_bytes(&comps, delims.component);
    } else {
        reps[rep_idx] = new_value.to_vec();
    }

    join_bytes(&reps, delims.repetition)
}

fn join_bytes(parts: &[Vec<u8>], sep: u8) -> Vec<u8> {
    let cap: usize = parts.iter().map(Vec::len).sum::<usize>() + parts.len().saturating_sub(1);
    let mut out = Vec::with_capacity(cap);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.extend_from_slice(part);
    }
    out
}

/// Clear a field, repetition, component, or subcomponent inside `seg`, trimming trailing
/// empty subcomponent, component, repetition, and field separators cleanly (`EDIT-3`, `EDIT-10`).
pub fn clear_segment_field(
    seg: &[u8],
    delims: Delimiters,
    addr: &FieldAddress,
) -> Result<Vec<u8>, EditError> {
    if addr.is_header_delimiter_field() {
        return Err(EditError::CannotEditField {
            address: addr.to_string(),
        });
    }
    let is_header = addr.segment == *b"MSH" || addr.segment == *b"FHS" || addr.segment == *b"BHS";

    let mut prefix = Vec::with_capacity(4);
    if seg.len() >= 4 {
        prefix.extend_from_slice(&seg[0..4]);
    } else {
        prefix.extend_from_slice(&addr.segment);
        prefix.push(delims.field);
    }

    let body = seg.get(4..).unwrap_or(&[]);
    let mut fields: Vec<Vec<u8>> = body
        .split(|&b| b == delims.field)
        .map(|s| s.to_vec())
        .collect();

    let field_idx = if is_header {
        usize::from(addr.field.get() - 2)
    } else {
        usize::from(addr.field.get() - 1)
    };

    if field_idx < fields.len() {
        fields[field_idx] = clear_field_value(&fields[field_idx], delims, addr);
    }

    // Trim trailing empty fields (for MSH/FHS/BHS, always preserve field index 0 which is MSH-2 encoding characters).
    let min_fields = usize::from(is_header);
    while fields.len() > min_fields && fields.last().is_some_and(Vec::is_empty) {
        fields.pop();
    }

    let mut out = prefix;
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(delims.field);
        }
        out.extend_from_slice(field);
    }
    Ok(out)
}

fn clear_field_value(field_bytes: &[u8], delims: Delimiters, addr: &FieldAddress) -> Vec<u8> {
    if addr.repetition.is_none() && addr.component.is_none() {
        return Vec::new();
    }

    let mut reps: Vec<Vec<u8>> = field_bytes
        .split(|&b| b == delims.repetition)
        .map(|s| s.to_vec())
        .collect();
    let rep_idx = addr.repetition.map_or(0, |r| usize::from(r.get() - 1));
    if rep_idx >= reps.len() {
        while reps.last().is_some_and(Vec::is_empty) {
            reps.pop();
        }
        return join_bytes(&reps, delims.repetition);
    }

    if let Some(comp) = addr.component {
        let mut comps: Vec<Vec<u8>> = reps[rep_idx]
            .split(|&b| b == delims.component)
            .map(|s| s.to_vec())
            .collect();
        let comp_idx = usize::from(comp.get() - 1);
        if comp_idx < comps.len() {
            if let Some(sub) = addr.subcomponent {
                let mut subs: Vec<Vec<u8>> = comps[comp_idx]
                    .split(|&b| b == delims.subcomponent)
                    .map(|s| s.to_vec())
                    .collect();
                let sub_idx = usize::from(sub.get() - 1);
                if sub_idx < subs.len() {
                    subs[sub_idx].clear();
                }
                while subs.last().is_some_and(Vec::is_empty) {
                    subs.pop();
                }
                comps[comp_idx] = join_bytes(&subs, delims.subcomponent);
            } else {
                comps[comp_idx].clear();
            }
        }
        while comps.last().is_some_and(Vec::is_empty) {
            comps.pop();
        }
        reps[rep_idx] = join_bytes(&comps, delims.component);
    } else {
        reps[rep_idx].clear();
    }

    while reps.last().is_some_and(Vec::is_empty) {
        reps.pop();
    }
    join_bytes(&reps, delims.repetition)
}

fn replace_in_segment(
    seg: &[u8],
    delims: Delimiters,
    find: &[u8],
    replace: &[u8],
    case_sensitive: bool,
) -> (Vec<u8>, usize) {
    if seg.len() < 4 {
        return (seg.to_vec(), 0);
    }
    let is_header = &seg[0..3] == b"MSH" || &seg[0..3] == b"FHS" || &seg[0..3] == b"BHS";
    let start_offset = if is_header {
        // Never touch segment tag (0..3), field sep (3), or encoding chars (4..8).
        seg.len().min(8)
    } else {
        4
    };

    let mut out = Vec::with_capacity(seg.len());
    out.extend_from_slice(&seg[..start_offset]);

    let mut total = 0usize;
    let mut token_start = start_offset;
    for i in start_offset..=seg.len() {
        let is_boundary = i == seg.len() || delims.contains(seg[i]);
        if is_boundary {
            let token = &seg[token_start..i];
            let (replaced, count) = replace_bytes(token, find, replace, case_sensitive);
            out.extend_from_slice(&replaced);
            total += count;
            if i < seg.len() {
                out.push(seg[i]);
            }
            token_start = i + 1;
        }
    }
    (out, total)
}

fn replace_bytes(
    haystack: &[u8],
    find: &[u8],
    replace: &[u8],
    case_sensitive: bool,
) -> (Vec<u8>, usize) {
    if find.is_empty() || haystack.len() < find.len() {
        return (haystack.to_vec(), 0);
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut count = 0usize;
    let mut i = 0usize;

    while i < haystack.len() {
        if i + find.len() <= haystack.len()
            && bytes_match(&haystack[i..i + find.len()], find, case_sensitive)
        {
            out.extend_from_slice(replace);
            count += 1;
            i += find.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    (out, count)
}

fn bytes_match(a: &[u8], b: &[u8], case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(b)
    }
}

// ---------------------------------------------------------------------------
// SORT-1 / SORT-2: SortRecordsCmd (stable, undoable via stored permutation)
// ---------------------------------------------------------------------------

/// Stable record-list sort command (`SORT-1`, `SORT-2`, `D-8`).
///
/// Stores the origin permutation `Vec<usize>` so undo restores the exact pre-sort
/// order in O(N) without cloning record buffers (`G-4`).
#[derive(Debug)]
pub struct SortRecordsCmd {
    address: FieldAddress,
    key_type: SortKeyType,
    direction: SortDirection,
    /// `permutation[sorted_idx] = original_idx`
    permutation: Option<Vec<usize>>,
}

impl SortRecordsCmd {
    /// Create a command to sort records by `MSH-7` timestamp (`SORT-1`).
    pub fn by_msh7(direction: SortDirection) -> Self {
        let msh7 = FieldAddress {
            segment: *b"MSH",
            field: std::num::NonZeroU16::new(7).unwrap_or(std::num::NonZeroU16::MIN),
            repetition: None,
            component: None,
            subcomponent: None,
        };
        Self {
            address: msh7,
            key_type: SortKeyType::Date,
            direction,
            permutation: None,
        }
    }

    /// Create a command to sort records by any [`FieldAddress`] and [`SortKeyType`] (`SORT-2`, `D-8`).
    pub fn by_address(
        address: FieldAddress,
        key_type: SortKeyType,
        direction: SortDirection,
    ) -> Self {
        Self {
            address,
            key_type,
            direction,
            permutation: None,
        }
    }
}

impl EditCommand for SortRecordsCmd {
    fn description(&self) -> String {
        format!("Sort Records by {}", self.address)
    }

    fn apply(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let n = doc.len();
        if n <= 1 {
            self.permutation = Some((0..n).collect());
            return Ok(EditOutcome {
                replacements: n,
                selection_hint: (n == 1).then_some(SelectionAnchor {
                    record_index: 0,
                    segment_index: Some(0),
                    address: Some(self.address),
                }),
            });
        }

        let keys: Vec<String> = doc
            .records
            .iter()
            .map(|rec| extract_first_occurrence_key(rec, &self.address))
            .collect();

        let mut perm: Vec<usize> = (0..n).collect();
        let key_type = self.key_type;
        let direction = self.direction;
        // Rust's `slice::sort_by` is guaranteed stable (SORT-1).
        perm.sort_by(|&a, &b| compare_typed_values(&keys[a], &keys[b], key_type, direction));

        let mut slots: Vec<Option<Record>> = doc.records.drain(..).map(Some).collect();
        for &orig_idx in &perm {
            if let Some(rec) = slots[orig_idx].take() {
                doc.records.push(rec);
            }
        }
        self.permutation = Some(perm);

        Ok(EditOutcome {
            replacements: n,
            selection_hint: Some(SelectionAnchor {
                record_index: 0,
                segment_index: Some(0),
                address: Some(self.address),
            }),
        })
    }

    fn undo(&mut self, doc: &mut Document) -> Result<EditOutcome, EditError> {
        let n = doc.len();
        if let Some(ref perm) = self.permutation {
            if perm.len() == n && n > 1 {
                let mut slots: Vec<Option<Record>> = (0..n).map(|_| None).collect();
                for (sorted_idx, rec) in doc.records.drain(..).enumerate() {
                    let orig_idx = perm[sorted_idx];
                    slots[orig_idx] = Some(rec);
                }
                doc.records = slots.into_iter().flatten().collect();
            }
        }
        Ok(EditOutcome {
            replacements: n,
            selection_hint: (!doc.is_empty()).then_some(SelectionAnchor {
                record_index: 0,
                segment_index: Some(0),
                address: Some(self.address),
            }),
        })
    }

    fn estimated_bytes(&self) -> usize {
        self.permutation
            .as_ref()
            .map_or(0, |p| p.len() * std::mem::size_of::<usize>())
    }
}

fn extract_first_occurrence_key(rec: &Record, addr: &FieldAddress) -> String {
    let Some((seg_idx, _)) = rec.segments().find_by_tag(&addr.segment).next() else {
        return String::new();
    };
    let Some(bytes) = rec.field_bytes(seg_idx, addr) else {
        return String::new();
    };
    rec.charset().decode_display(bytes).into_owned()
}
