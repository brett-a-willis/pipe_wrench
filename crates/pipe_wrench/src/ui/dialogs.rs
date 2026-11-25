//! UI dialogs for editing records, fields, assigning values, find/replace,
//! deleting segments/records, and changing message delimiters (EDIT-1..EDIT-9).

use egui::{Color32, Context, TextEdit, Window};
use hl7_core::{
    AssignFieldCmd, ChangeDelimitersCmd, DeleteRecordsCmd, DeleteSegmentsCmd, EditError,
    EditFieldCmd, EditRecordCmd, FieldAddress, ReplaceCmd, ReplaceScope,
};

use crate::config::AppSettings;
use crate::state::SessionState;
use crate::strings;
use crate::ui::icons::{icon_labeled_button, IconKind};
use crate::ui::theme::{add_action_button_job, ThemePalette};

/// State for the Edit Record (raw multiline) dialog (EDIT-2).
#[derive(Debug, Clone)]
pub struct EditRecordDialog {
    /// Target 0-based record index.
    pub record_index: usize,
    /// Multiline text buffer (segments separated by `\n` in the editor; normalized to `\r` on commit).
    pub text: String,
    /// Last byte cursor position inside `text` for "Insert End-of-Segment".
    pub cursor_byte_pos: Option<usize>,
    /// Inline error message if commit fails.
    pub error: Option<String>,
}

impl EditRecordDialog {
    /// Insert an End-of-Segment marker (newline in the multiline editor, committed as CR `0x0D`)
    /// at the current cursor offset (or at the end if no cursor position is recorded) (EDIT-2).
    pub fn insert_end_of_segment(&mut self) {
        let pos = self
            .cursor_byte_pos
            .unwrap_or(self.text.len())
            .min(self.text.len());
        let safe_pos = if self.text.is_char_boundary(pos) {
            pos
        } else {
            self.text.len()
        };
        self.text.insert(safe_pos, '\n');
        self.cursor_byte_pos = Some(safe_pos + 1);
    }

    /// Convert editor text into CR-separated bytes for `EditRecordCmd` (EDIT-2).
    pub fn to_cr_bytes(&self) -> Vec<u8> {
        let normalized = self.text.replace("\r\n", "\r").replace('\n', "\r");
        let trimmed = normalized.trim_end_matches('\r');
        trimmed.as_bytes().to_vec()
    }
}

/// State for the Edit Segment (raw single-segment) dialog (`EDIT-2`, `EDIT-10`, `D-26`).
#[derive(Debug, Clone)]
pub struct EditSegmentDialog {
    /// Target 0-based record index.
    pub record_index: usize,
    /// Target 0-based segment index.
    pub segment_index: usize,
    /// Raw segment text buffer.
    pub text: String,
    /// Inline error message if commit fails.
    pub error: Option<String>,
}

/// Target insertion placement for [`InsertSegmentDialog`] (`EDIT-10`, `D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertSegmentMode {
    /// Insert immediately above the specified 0-based segment index.
    Above(usize),
    /// Insert immediately below the specified 0-based segment index.
    Below(usize),
    /// Append to the end of the active record.
    AtEnd,
}

/// State for the Insert Segment dialog (`EDIT-10`, `D-26`).
#[derive(Debug, Clone)]
pub struct InsertSegmentDialog {
    /// Target 0-based record index.
    pub record_index: usize,
    /// Target insertion mode (`Above`, `Below`, `AtEnd`).
    pub mode: InsertSegmentMode,
    /// Raw segment text to insert (e.g. `"NTE|1|L|"`).
    pub text: String,
    /// Inline error message if commit fails.
    pub error: Option<String>,
}

/// State for the Edit Field dialog (EDIT-3, EDIT-4).
#[derive(Debug, Clone)]
pub struct EditFieldDialog {
    /// Target record index.
    pub record_index: usize,
    /// Target segment index.
    pub segment_index: usize,
    /// Target field address.
    pub address: FieldAddress,
    /// User-editable value string.
    pub value: String,
    /// Set to `true` when the user entered an active delimiter, unlocking the escape button (EDIT-4).
    pub show_escape_option: bool,
    /// Inline validation error message.
    pub error: Option<String>,
}

/// State for the Assign Value to Field dialog (EDIT-5, D-8).
#[derive(Debug, Clone)]
pub struct AssignFieldDialog {
    /// Field address string entered by the user (e.g. `"PID-5-1"`).
    pub address_input: String,
    /// Value to assign.
    pub value_input: String,
    /// `true` = selected records only, `false` = all records.
    pub selected_records_only: bool,
    /// `true` = only first occurrence of segment in each record, `false` = all occurrences (D-8).
    pub first_occurrence_only: bool,
    /// Inline validation error message.
    pub error: Option<String>,
}

impl Default for AssignFieldDialog {
    fn default() -> Self {
        Self {
            address_input: "PID-3".to_string(),
            value_input: String::new(),
            selected_records_only: false,
            first_occurrence_only: false,
            error: None,
        }
    }
}

/// UI scope selector for the Replace dialog (EDIT-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplaceUiScope {
    /// Current selected field only.
    CurrentField,
    /// Current selected segment only.
    CurrentSegment,
    /// Current record list (all or selected records).
    #[default]
    RecordList,
}

/// State for the Replace dialog (EDIT-6).
#[derive(Debug, Clone, Default)]
pub struct ReplaceDialog {
    /// Chosen scope.
    pub ui_scope: ReplaceUiScope,
    /// When `ui_scope == RecordList`, restrict to selected records only.
    pub selected_records_only: bool,
    /// Find text.
    pub find_text: String,
    /// Replacement text.
    pub replace_text: String,
    /// Case-sensitive matching toggle.
    pub case_sensitive: bool,
    /// Inline validation or result status message.
    pub status_or_error: Option<String>,
}

/// State for the Change Message Delimiters dialog (EDIT-9, D-11).
#[derive(Debug, Clone)]
pub struct ChangeDelimitersDialog {
    /// Display string of current delimiters (e.g. `"|^~\\&"`).
    pub current_delimiters: String,
    /// User input for the 5 new delimiter characters.
    pub new_delimiters_input: String,
    /// Inline validation error message.
    pub error: Option<String>,
}

/// Pending delete confirmation dialog (EDIT-7, EDIT-8).
#[derive(Debug, Clone)]
pub enum DeleteConfirmDialog {
    /// Confirm deleting segments from a record (EDIT-7).
    Segments {
        /// Target record index.
        record_index: usize,
        /// Segment indices to delete.
        segment_indices: Vec<usize>,
    },
    /// Confirm deleting records from the document (EDIT-8).
    Records {
        /// Record indices to delete.
        indices: Vec<usize>,
    },
    /// Confirm clearing/deleting a field, component, or subcomponent (`EDIT-3`, `EDIT-7`).
    ClearField {
        /// Target record index.
        record_index: usize,
        /// Target segment index.
        segment_index: usize,
        /// Target field address.
        address: FieldAddress,
    },
    /// Confirm pruning more than [`crate::state::PRUNE_CONFIRM_THRESHOLD`] unmatched messages from memory.
    PruneUnmatched {
        /// Number of matching messages that will be kept.
        keep_count: usize,
        /// Number of non-matching messages that will be dropped from memory.
        drop_count: usize,
    },
}

/// Render all open Phase 6 and Phase 12 editing dialogs and confirmation modals.
pub fn show_editing_dialogs(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    show_edit_record_window(ctx, state, settings);
    show_edit_segment_window(ctx, state, settings);
    show_insert_segment_window(ctx, state, settings);
    show_edit_field_window(ctx, state, settings);
    show_assign_field_window(ctx, state, settings);
    show_replace_window(ctx, state, settings);
    show_change_delimiters_window(ctx, state, settings);
    show_delete_confirm_window(ctx, state, settings);
    show_modal_alert_window(ctx, state);
    show_undo_warning_window(ctx, state);
}

fn show_edit_segment_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.edit_segment_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut commit = false;

    Window::new(format!(
        "Edit Segment #{} (Record #{})",
        dlg.segment_index + 1,
        dlg.record_index + 1
    ))
    .collapsible(false)
    .resizable(true)
    .default_width(560.0)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        ui.label("Replace the raw contents of this segment:");
        let resp = ui.add(
            TextEdit::singleline(&mut dlg.text)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            commit = true;
        }
        if let Some(ref err) = dlg.error {
            ui.add_space(4.0);
            ui.colored_label(Color32::LIGHT_RED, err);
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() {
                commit = true;
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });

    if commit {
        match state.edit_segment_raw(dlg.segment_index, &dlg.text, settings) {
            Ok(_) => close = true,
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.edit_segment_dialog = None;
    } else {
        state.edit_segment_dialog = Some(dlg);
    }
}

fn show_insert_segment_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.insert_segment_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut commit = false;

    let title = match dlg.mode {
        InsertSegmentMode::Above(idx) => format!("Insert Segment Above #{}", idx + 1),
        InsertSegmentMode::Below(idx) => format!("Insert Segment Below #{}", idx + 1),
        InsertSegmentMode::AtEnd => "Insert Segment at End".to_string(),
    };

    Window::new(title)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Presets:");
                for (label, preset) in [
                    ("NTE", "NTE|1|L|"),
                    ("OBX", "OBX|1|ST|CODE||VALUE"),
                    ("PID", "PID|1||"),
                    ("PV1", "PV1|1|I|"),
                    ("ORC", "ORC|NW|"),
                    ("OBR", "OBR|1||"),
                    ("DG1", "DG1|1||"),
                    ("AL1", "AL1|1||"),
                    ("ZXX", "ZXX|1|"),
                ] {
                    if ui.small_button(label).clicked() {
                        dlg.text = preset.to_string();
                    }
                }
            });
            ui.add_space(4.0);
            ui.label("Segment string:");
            let resp = ui.add(
                TextEdit::singleline(&mut dlg.text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                commit = true;
            }
            if let Some(ref err) = dlg.error {
                ui.add_space(4.0);
                ui.colored_label(Color32::LIGHT_RED, err);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button(add_action_button_job(ui, "Insert", true))
                    .clicked()
                {
                    commit = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if commit {
        let res = match dlg.mode {
            InsertSegmentMode::Above(idx) => state.insert_segment_above(idx, &dlg.text, settings),
            InsertSegmentMode::Below(idx) => state.insert_segment_below(idx, &dlg.text, settings),
            InsertSegmentMode::AtEnd => state.insert_segment_at_end(&dlg.text, settings),
        };
        match res {
            Ok(_) => close = true,
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.insert_segment_dialog = None;
    } else {
        state.insert_segment_dialog = Some(dlg);
    }
}

fn show_edit_record_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.edit_record_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut commit = false;

    Window::new(format!("Edit Record #{}", dlg.record_index + 1))
        .collapsible(false)
        .resizable(true)
        .default_width(640.0)
        .default_height(360.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(strings::BTN_INSERT_EOS).clicked() {
                    dlg.insert_end_of_segment();
                }
                ui.weak("Inserts a segment terminator (CR) at the cursor.");
            });
            ui.separator();

            let out = TextEdit::multiline(&mut dlg.text)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(12)
                .show(ui);

            if let Some(range) = out.cursor_range {
                let char_idx = range.primary.ccursor.index;
                let byte_pos = dlg
                    .text
                    .char_indices()
                    .nth(char_idx)
                    .map_or(dlg.text.len(), |(b, _)| b);
                dlg.cursor_byte_pos = Some(byte_pos);
            }

            if let Some(ref err) = dlg.error {
                ui.colored_label(Color32::LIGHT_RED, err);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    commit = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if commit {
        let new_bytes = dlg.to_cr_bytes();
        let cmd = Box::new(EditRecordCmd::new(dlg.record_index, new_bytes));
        match state.execute_command(cmd, settings) {
            Ok(_) => close = true,
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.edit_record_dialog = None;
    } else {
        state.edit_record_dialog = Some(dlg);
    }
}

fn show_edit_field_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.edit_field_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut submit_mode: Option<bool> = None; // Some(escape_delimiters)

    Window::new(format!("Edit Field {}", dlg.address))
        .collapsible(false)
        .resizable(true)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("Address: {}", dlg.address));
            let resp = ui.add(
                TextEdit::singleline(&mut dlg.value)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit_mode = Some(false);
            }

            if let Some(ref err) = dlg.error {
                ui.add_space(4.0);
                ui.colored_label(Color32::LIGHT_RED, err);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    submit_mode = Some(false);
                }
                if dlg.show_escape_option && ui.button(strings::BTN_ENCODE_DELIMITERS).clicked() {
                    submit_mode = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if let Some(escape_delims) = submit_mode {
        let cmd = Box::new(EditFieldCmd::new(
            dlg.record_index,
            dlg.segment_index,
            dlg.address,
            dlg.value.as_bytes().to_vec(),
            escape_delims,
        ));
        match state.execute_command(cmd, settings) {
            Ok(_) => close = true,
            Err(EditError::DelimiterInValue { message, .. }) => {
                dlg.show_escape_option = true;
                dlg.error = Some(message);
            }
            Err(e) => {
                dlg.error = Some(e.to_string());
            }
        }
    }

    if close {
        state.edit_field_dialog = None;
    } else {
        state.edit_field_dialog = Some(dlg);
    }
}

fn show_assign_field_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.assign_field_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut submit = false;

    Window::new("Assign Value to Field")
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Field Address (e.g. PID-5-1):");
                ui.text_edit_singleline(&mut dlg.address_input);
            });
            ui.horizontal(|ui| {
                ui.label("New Value:");
                ui.text_edit_singleline(&mut dlg.value_input);
            });
            ui.checkbox(&mut dlg.selected_records_only, "Selected records only");
            ui.checkbox(
                &mut dlg.first_occurrence_only,
                "First occurrence of segment only",
            );

            if let Some(ref err) = dlg.error {
                ui.colored_label(Color32::LIGHT_RED, err);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Assign").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if submit {
        match dlg.address_input.trim().parse::<FieldAddress>() {
            Ok(addr) => {
                let targets = dlg
                    .selected_records_only
                    .then(|| state.selected_records.iter().copied().collect());
                let cmd = Box::new(AssignFieldCmd::new(
                    targets,
                    addr,
                    dlg.value_input.as_bytes().to_vec(),
                    dlg.first_occurrence_only,
                ));
                match state.execute_command(cmd, settings) {
                    Ok(out) => {
                        state.status_message =
                            format!("Assigned {} in {} segment(s).", addr, out.replacements);
                        close = true;
                    }
                    Err(e) => dlg.error = Some(e.to_string()),
                }
            }
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.assign_field_dialog = None;
    } else {
        state.assign_field_dialog = Some(dlg);
    }
}

fn show_replace_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.replace_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut submit = false;

    Window::new("Replace")
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find:");
                ui.text_edit_singleline(&mut dlg.find_text);
            });
            ui.horizontal(|ui| {
                ui.label("Replace with:");
                ui.text_edit_singleline(&mut dlg.replace_text);
            });
            ui.separator();
            ui.label("Scope:");
            ui.radio_value(
                &mut dlg.ui_scope,
                ReplaceUiScope::CurrentField,
                "Current Field",
            );
            ui.radio_value(
                &mut dlg.ui_scope,
                ReplaceUiScope::CurrentSegment,
                "Current Segment",
            );
            ui.radio_value(
                &mut dlg.ui_scope,
                ReplaceUiScope::RecordList,
                "Current Record List",
            );
            if dlg.ui_scope == ReplaceUiScope::RecordList {
                ui.checkbox(&mut dlg.selected_records_only, "Selected records only");
            }
            ui.checkbox(&mut dlg.case_sensitive, "Case sensitive");

            if let Some(ref msg) = dlg.status_or_error {
                ui.colored_label(Color32::LIGHT_YELLOW, msg);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    submit = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });

    if submit {
        let resolved_scope = match dlg.ui_scope {
            ReplaceUiScope::CurrentField => {
                state
                    .selected_field_address()
                    .map(|(rec_idx, seg_idx, addr)| ReplaceScope::Field {
                        record_index: rec_idx,
                        segment_index: seg_idx,
                        address: addr,
                    })
            }
            ReplaceUiScope::CurrentSegment => state
                .selected_record
                .zip(state.selected_segment)
                .map(|(record_index, segment_index)| ReplaceScope::Segment {
                    record_index,
                    segment_index,
                }),
            ReplaceUiScope::RecordList => Some(ReplaceScope::RecordList {
                record_indices: dlg
                    .selected_records_only
                    .then(|| state.selected_records.iter().copied().collect()),
            }),
        };

        match resolved_scope {
            Some(scope) => {
                let cmd = Box::new(ReplaceCmd::new(
                    scope,
                    dlg.find_text.as_bytes().to_vec(),
                    dlg.replace_text.as_bytes().to_vec(),
                    dlg.case_sensitive,
                ));
                match state.execute_command(cmd, settings) {
                    Ok(out) => {
                        let msg = format!("Made {} replacement(s).", out.replacements);
                        state.status_message = msg.clone();
                        dlg.status_or_error = Some(msg);
                    }
                    Err(e) => dlg.status_or_error = Some(e.to_string()),
                }
            }
            None => {
                dlg.status_or_error =
                    Some("Select a target field or segment for the chosen scope.".to_string());
            }
        }
    }

    if close {
        state.replace_dialog = None;
    } else {
        state.replace_dialog = Some(dlg);
    }
}

fn show_change_delimiters_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.change_delimiters_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut submit = false;

    Window::new("Change Message Delimiters")
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.colored_label(
                Color32::from_rgb(230, 160, 40),
                strings::WARN_CHANGE_DELIMITERS,
            );
            ui.add_space(6.0);
            ui.label(format!(
                "Current delimiters (field, component, repetition, escape, subcomponent): {}",
                dlg.current_delimiters
            ));
            ui.horizontal(|ui| {
                ui.label("New 5 delimiters:");
                ui.add(
                    TextEdit::singleline(&mut dlg.new_delimiters_input)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(120.0),
                );
            });
            if let Some(ref err) = dlg.error {
                ui.colored_label(Color32::LIGHT_RED, err);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if submit {
        let cmd = Box::new(ChangeDelimitersCmd::new(
            dlg.new_delimiters_input.as_bytes().to_vec(),
        ));
        match state.execute_command(cmd, settings) {
            Ok(out) => {
                let msg = format!("Changed delimiters ({} replacements).", out.replacements);
                state.status_message = msg.clone();
                state.modal_alert = Some(msg);
                close = true;
            }
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.change_delimiters_dialog = None;
    } else {
        state.change_delimiters_dialog = Some(dlg);
    }
}

fn show_delete_confirm_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(confirm) = state.delete_confirm_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut confirmed = false;

    let (title, confirm_label, prompt) = match &confirm {
        DeleteConfirmDialog::Segments {
            segment_indices, ..
        } => (
            "Confirm Delete",
            "Delete",
            strings::confirm_delete_segments(segment_indices.len()),
        ),
        DeleteConfirmDialog::Records { indices } => (
            "Confirm Delete",
            "Delete",
            strings::confirm_delete_records(indices.len()),
        ),
        DeleteConfirmDialog::ClearField { address, .. } => (
            "Confirm Delete",
            "Delete",
            format!("Are you sure that you want to clear/delete field {address}?"),
        ),
        DeleteConfirmDialog::PruneUnmatched {
            keep_count,
            drop_count,
        } => (
            "Confirm Keep Only Filtered",
            "Keep Only Filtered",
            format!(
                "Remove {drop_count} unmatched message(s) from memory and keep only the {keep_count} filtered message(s)?\n\nBecause more than {} messages are being dropped, this frees their memory immediately and clears Undo history.",
                crate::state::PRUNE_CONFIRM_THRESHOLD
            ),
        ),
    };

    Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(prompt);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let palette = ThemePalette::for_visuals(ui.visuals());
                if icon_labeled_button(
                    ui,
                    IconKind::Delete,
                    confirm_label,
                    true,
                    Some(palette.accent_red),
                )
                .clicked()
                {
                    confirmed = true;
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if confirmed {
        match confirm {
            DeleteConfirmDialog::Segments {
                record_index,
                segment_indices,
            } => {
                let cmd = Box::new(DeleteSegmentsCmd::new(record_index, segment_indices));
                if let Err(e) = state.execute_command(cmd, settings) {
                    state.modal_alert = Some(e.to_string());
                }
            }
            DeleteConfirmDialog::Records { indices } => {
                let cmd = Box::new(DeleteRecordsCmd::selected(indices));
                if let Err(e) = state.execute_command(cmd, settings) {
                    state.modal_alert = Some(e.to_string());
                }
            }
            DeleteConfirmDialog::ClearField {
                segment_index,
                address,
                ..
            } => {
                if let Err(e) = state.clear_field_at(segment_index, address, settings) {
                    state.modal_alert = Some(e.to_string());
                }
            }
            DeleteConfirmDialog::PruneUnmatched { .. } => {
                if let Err(e) = state.keep_only_filtered(settings) {
                    state.modal_alert = Some(e.to_string());
                }
            }
        }
    }

    if close {
        state.delete_confirm_dialog = None;
    }
}

fn show_modal_alert_window(ctx: &Context, state: &mut SessionState) {
    let Some(msg) = state.modal_alert.clone() else {
        return;
    };
    let mut close = false;
    Window::new("Notice")
        .collapsible(false)
        .resizable(false)
        .max_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(&msg);
            ui.add_space(8.0);
            if ui.button("OK").clicked() {
                close = true;
            }
        });
    if close {
        state.modal_alert = None;
    }
}

fn show_undo_warning_window(ctx: &Context, state: &mut SessionState) {
    let Some(warn) = state.undo_warning else {
        return;
    };
    let mut dismiss = false;
    let mut clear_history = false;
    Window::new("Undo Memory Limit Warning")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Undo history is using {} KiB (configured warning threshold: {} KiB).",
                warn.current_bytes / 1024,
                warn.limit_bytes / 1024
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Clear Undo History").clicked() {
                    clear_history = true;
                    dismiss = true;
                }
                if ui.button("Continue").clicked() {
                    dismiss = true;
                }
            });
        });
    if clear_history {
        state.undo_stack.clear();
    }
    if dismiss {
        state.undo_warning = None;
    }
}
