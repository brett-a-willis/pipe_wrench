//! UI windows for Record Sorting (`SORT-1`, `SORT-2`) and the Report Builder,
//! Sortable Report Table, Multi-Column Sort dialog, and Exporters (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`).

use std::collections::BTreeSet;

use egui::{Color32, Context, RichText, Sense, Ui, Window};
use egui_extras::{Column, TableBuilder};
use hl7_core::{
    DelimitedExportOptions, DelimitedQuoteMode, ExportRowScope, FieldAddress, FixedWidthAlign,
    FixedWidthColumnSpec, FixedWidthExportOptions, Hl7Dictionary, Record, ReportColumn,
    ReportDefinition, ReportError, ReportRowGrain, ReportTable, SortDirection, SortKeySpec,
    SortKeyType,
};

use crate::config::{load_report_definition_file, save_report_definition_in, AppSettings};
use crate::state::SessionState;
use crate::ui::icons::{icon_button, IconKind};
use crate::ui::theme::{add_action_button_job, ThemePalette};

/// Dialog state for "Sort Records by Field..." (`SORT-2`).
#[derive(Debug, Clone)]
pub struct SortRecordsDialog {
    /// Target field address string (e.g. `"PID-5-1"`).
    pub address_input: String,
    /// Comparison key type (`String`, `Number`, `Date`).
    pub key_type: SortKeyType,
    /// Sort direction.
    pub direction: SortDirection,
    /// Inline error message if address parsing fails.
    pub error: Option<String>,
}

impl Default for SortRecordsDialog {
    fn default() -> Self {
        Self {
            address_input: "MSH-7".to_string(),
            key_type: SortKeyType::Date,
            direction: SortDirection::Ascending,
            error: None,
        }
    }
}

/// Dialog state for the 3-key Multi-Column Report Sort dialog (`SORT-4`).
#[derive(Debug, Clone)]
pub struct MultiColumnSortDialog {
    /// Whether key 2 is enabled.
    pub use_key2: bool,
    /// Whether key 3 is enabled.
    pub use_key3: bool,
    /// Key 1 specification.
    pub key1: SortKeySpec,
    /// Key 2 specification.
    pub key2: SortKeySpec,
    /// Key 3 specification.
    pub key3: SortKeySpec,
}

impl Default for MultiColumnSortDialog {
    fn default() -> Self {
        Self {
            use_key2: false,
            use_key3: false,
            key1: SortKeySpec {
                column_index: 0,
                direction: SortDirection::Ascending,
            },
            key2: SortKeySpec {
                column_index: 0,
                direction: SortDirection::Ascending,
            },
            key3: SortKeySpec {
                column_index: 0,
                direction: SortDirection::Ascending,
            },
        }
    }
}

/// Interactive Report Builder and Sortable Report Table window state (`RPT-1..RPT-9`, `SORT-3`, `SORT-4`).
#[derive(Debug, Clone)]
pub struct ReportWindowState {
    /// Whether the report window is open.
    pub open: bool,
    /// Active report definition.
    pub definition: ReportDefinition,
    /// Built and sorted report table.
    pub table: Option<ReportTable>,
    /// Selected row indices in the report table.
    pub selected_rows: BTreeSet<usize>,
    /// Input box for adding a new column by field address.
    pub new_column_address: String,
    /// Data type for the new column.
    pub new_column_type: SortKeyType,
    /// Driving segment tag input when per-segment row grain is selected (`D-13`).
    pub driving_segment_input: String,
    /// Whether per-segment grain is active (`D-13`).
    pub per_segment_grain: bool,
    /// Include header row in exports (`RPT-3`, `RPT-4`).
    pub include_header: bool,
    /// Use field descriptions instead of addresses in headers (`RPT-3`).
    pub use_descriptions_in_header: bool,
    /// Restrict export to selected rows only (`RPT-3`, `RPT-4`).
    pub export_selected_only: bool,
    /// Separator string for delimited export (`RPT-3`).
    pub delimited_separator_input: String,
    /// Quoting mode for delimited export (`RPT-3`).
    pub delimited_quote_mode: DelimitedQuoteMode,
    /// Per-column fixed-width settings (`RPT-4`).
    pub fixed_specs: Vec<FixedWidthColumnSpec>,
    /// Optional 3-key multi-column sort dialog (`SORT-4`).
    pub multi_sort_dialog: Option<MultiColumnSortDialog>,
    /// Status or validation message displayed inside the report window.
    pub status_or_error: Option<String>,
}

impl Default for ReportWindowState {
    fn default() -> Self {
        Self {
            open: false,
            definition: ReportDefinition::default(),
            table: None,
            selected_rows: BTreeSet::new(),
            new_column_address: "PID-3".to_string(),
            new_column_type: SortKeyType::String,
            driving_segment_input: "OBX".to_string(),
            per_segment_grain: false,
            include_header: true,
            use_descriptions_in_header: false,
            export_selected_only: false,
            delimited_separator_input: "\\t".to_string(),
            delimited_quote_mode: DelimitedQuoteMode::QuoteContainingSeparator,
            fixed_specs: Vec::new(),
            multi_sort_dialog: None,
            status_or_error: None,
        }
    }
}

impl ReportWindowState {
    /// Open the report window, optionally seeding it with `initial_field` (`RPT-1`).
    pub fn open_with_field(
        &mut self,
        initial_field: Option<FieldAddress>,
        records: &[Record],
        dict: &Hl7Dictionary,
    ) {
        self.open = true;
        if let Some(addr) = initial_field {
            if !self.definition.columns.iter().any(|c| c.address == addr) {
                let col_type = if addr.segment == *b"MSH" && addr.field.get() == 7 {
                    SortKeyType::Date
                } else {
                    SortKeyType::String
                };
                self.definition
                    .columns
                    .push(ReportColumn::new(addr, col_type));
            }
            self.new_column_address = addr.to_string();
        } else if self.definition.columns.is_empty() {
            for default_addr in ["MSH-9", "MSH-10", "MSH-7", "PID-3", "PID-5"] {
                if let Ok(a) = default_addr.parse::<FieldAddress>() {
                    let t = if default_addr == "MSH-7" {
                        SortKeyType::Date
                    } else {
                        SortKeyType::String
                    };
                    self.definition.columns.push(ReportColumn::new(a, t));
                }
            }
        }
        self.rebuild_table(records, dict);
    }

    /// Open the Report Builder pre-populated with multiple selected HL7 field/component paths
    /// in display order (`fields`), replacing any prior columns so the user can immediately
    /// preview or export a tabular report across all loaded messages.
    pub fn open_with_fields(
        &mut self,
        fields: &[FieldAddress],
        records: &[Record],
        dict: &Hl7Dictionary,
    ) {
        self.open = true;
        if !fields.is_empty() {
            self.definition.columns.clear();
            for &addr in fields {
                if !self.definition.columns.iter().any(|c| c.address == addr) {
                    let col_type = if addr.segment == *b"MSH" && addr.field.get() == 7 {
                        SortKeyType::Date
                    } else {
                        SortKeyType::String
                    };
                    self.definition
                        .columns
                        .push(ReportColumn::new(addr, col_type));
                }
            }
            if let Some(last) = fields.last() {
                self.new_column_address = last.to_string();
            }
        }
        self.rebuild_table(records, dict);
    }

    /// Rebuild the report table from `records` and synchronize default fixed-width column specs.
    pub fn rebuild_table(&mut self, records: &[Record], dict: &Hl7Dictionary) {
        if self.per_segment_grain {
            let tag_bytes = self.driving_segment_input.trim().to_ascii_uppercase();
            if tag_bytes.len() == 3 {
                let b = tag_bytes.as_bytes();
                self.definition.grain = ReportRowGrain::PerSegment {
                    driving_segment: [b[0], b[1], b[2]],
                };
            }
        } else {
            self.definition.grain = ReportRowGrain::PerRecord;
        }

        let tbl = ReportTable::build(self.definition.clone(), records, Some(dict));
        let default_widths = tbl.default_column_widths(self.use_descriptions_in_header);
        if self.fixed_specs.len() != default_widths.len() {
            self.fixed_specs = default_widths
                .into_iter()
                .map(|width| FixedWidthColumnSpec {
                    width,
                    align: FixedWidthAlign::LeftSpace,
                })
                .collect();
        }
        self.selected_rows.retain(|&i| i < tbl.rows.len());
        self.table = Some(tbl);
    }

    /// Add a column by field address string and rebuild the table (`RPT-1`).
    pub fn add_column(
        &mut self,
        addr_str: &str,
        col_type: SortKeyType,
        records: &[Record],
        dict: &Hl7Dictionary,
    ) -> Result<(), String> {
        let addr: FieldAddress = addr_str
            .trim()
            .parse()
            .map_err(|e: hl7_core::AddressError| e.to_string())?;
        self.definition
            .columns
            .push(ReportColumn::new(addr, col_type));
        self.fixed_specs.clear();
        self.rebuild_table(records, dict);
        Ok(())
    }

    /// Remove a column by index and rebuild the table (`RPT-1`).
    pub fn remove_column(&mut self, col_idx: usize, records: &[Record], dict: &Hl7Dictionary) {
        if col_idx < self.definition.columns.len() {
            self.definition.columns.remove(col_idx);
            self.fixed_specs.clear();
            self.rebuild_table(records, dict);
        }
    }

    /// Active export row scope (`AllRows` or `SelectedRows`).
    pub fn active_scope(&self) -> ExportRowScope {
        if self.export_selected_only {
            ExportRowScope::SelectedRows(self.selected_rows.iter().copied().collect())
        } else {
            ExportRowScope::AllRows
        }
    }
}

/// Render the Sort Records dialog, Report Builder window, and Multi-Column Sort dialog.
pub fn show_reports_and_sort_windows(
    ctx: &Context,
    state: &mut SessionState,
    settings: &mut AppSettings,
) {
    show_sort_records_window(ctx, state, settings);
    show_report_builder_window(ctx, state, settings);
}

fn show_sort_records_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(mut dlg) = state.sort_records_dialog.clone() else {
        return;
    };
    let mut close = false;
    let mut submit = false;

    Window::new("Sort Records by Field")
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Field Address (e.g. MSH-7, PID-5-1):");
                ui.text_edit_singleline(&mut dlg.address_input);
            });
            ui.horizontal(|ui| {
                ui.label("Key Type:");
                ui.radio_value(&mut dlg.key_type, SortKeyType::String, "String");
                ui.radio_value(&mut dlg.key_type, SortKeyType::Number, "Number");
                ui.radio_value(&mut dlg.key_type, SortKeyType::Date, "Date (HL7)");
            });
            ui.horizontal(|ui| {
                ui.label("Direction:");
                ui.radio_value(&mut dlg.direction, SortDirection::Ascending, "Ascending");
                ui.radio_value(&mut dlg.direction, SortDirection::Descending, "Descending");
            });

            if let Some(ref err) = dlg.error {
                ui.colored_label(Color32::LIGHT_RED, err);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Sort").clicked() {
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
                match state.sort_records_by_address(addr, dlg.key_type, dlg.direction, settings) {
                    Ok(_) => close = true,
                    Err(e) => dlg.error = Some(e.to_string()),
                }
            }
            Err(e) => dlg.error = Some(e.to_string()),
        }
    }

    if close {
        state.sort_records_dialog = None;
    } else {
        state.sort_records_dialog = Some(dlg);
    }
}

fn show_report_builder_window(ctx: &Context, state: &mut SessionState, settings: &mut AppSettings) {
    if !state.report_window.open {
        return;
    }

    let mut open = state.report_window.open;
    let mut rebuild_needed = false;
    let mut header_clicked_col: Option<usize> = None;
    let mut remove_col: Option<usize> = None;
    let mut copy_clipboard = false;
    let mut export_action: Option<&'static str> = None;
    let mut save_def_clicked = false;
    let mut load_def_clicked = false;

    Window::new("Reports")
        .open(&mut open)
        .resizable(true)
        .default_width(880.0)
        .default_height(560.0)
        .show(ctx, |ui| {
            // Top bar: report definition name + Save/Load Definition + Row grain
            ui.horizontal(|ui| {
                ui.label("Report Name:");
                ui.text_edit_singleline(&mut state.report_window.definition.name);
                if ui.button("Save Definition...").clicked() {
                    save_def_clicked = true;
                }
                if ui.button("Load Definition...").clicked() {
                    load_def_clicked = true;
                }
                ui.separator();
                if ui
                    .checkbox(
                        &mut state.report_window.per_segment_grain,
                        "Row per Segment Occurrence",
                    )
                    .changed()
                {
                    rebuild_needed = true;
                }
                if state.report_window.per_segment_grain {
                    ui.label("Driving Segment:");
                    if ui
                        .add(
                            egui::TextEdit::singleline(
                                &mut state.report_window.driving_segment_input,
                            )
                            .desired_width(48.0),
                        )
                        .changed()
                    {
                        rebuild_needed = true;
                    }
                }
            });

            ui.separator();

            // Column management bar (RPT-1)
            ui.horizontal(|ui| {
                ui.label("Add Column Address:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.report_window.new_column_address)
                        .desired_width(110.0),
                );
                egui::ComboBox::from_id_salt("new_col_type_combo")
                    .selected_text(format!("{:?}", state.report_window.new_column_type))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut state.report_window.new_column_type,
                            SortKeyType::String,
                            "String",
                        );
                        ui.selectable_value(
                            &mut state.report_window.new_column_type,
                            SortKeyType::Number,
                            "Number",
                        );
                        ui.selectable_value(
                            &mut state.report_window.new_column_type,
                            SortKeyType::Date,
                            "Date",
                        );
                    });
                if ui
                    .button(add_action_button_job(ui, "Add Column", true))
                    .clicked()
                {
                    let addr_str = state.report_window.new_column_address.clone();
                    let col_type = state.report_window.new_column_type;
                    if let Err(e) = state.report_window.add_column(
                        &addr_str,
                        col_type,
                        &state.doc.records,
                        &state.dictionary,
                    ) {
                        state.report_window.status_or_error = Some(e);
                    }
                }
                ui.separator();
                if ui.button("Multi-Column Sort...").clicked() {
                    state.report_window.multi_sort_dialog = Some(MultiColumnSortDialog::default());
                }
                if ui
                    .checkbox(
                        &mut state.report_window.use_descriptions_in_header,
                        "Use Descriptions in Header",
                    )
                    .changed()
                {
                    rebuild_needed = true;
                }
            });

            // Active columns chips with type selector and remove button
            if !state.report_window.definition.columns.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (idx, col) in state
                        .report_window
                        .definition
                        .columns
                        .iter_mut()
                        .enumerate()
                    {
                        ui.group(|ui| {
                            ui.label(RichText::new(col.address.to_string()).monospace().strong());
                            egui::ComboBox::from_id_salt(format!("col_type_{idx}"))
                                .selected_text(format!("{:?}", col.column_type))
                                .width(72.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut col.column_type,
                                        SortKeyType::String,
                                        "String",
                                    );
                                    ui.selectable_value(
                                        &mut col.column_type,
                                        SortKeyType::Number,
                                        "Number",
                                    );
                                    ui.selectable_value(
                                        &mut col.column_type,
                                        SortKeyType::Date,
                                        "Date",
                                    );
                                });
                            let col_palette = ThemePalette::for_visuals(ui.visuals());
                            if icon_button(
                                ui,
                                IconKind::Delete,
                                true,
                                Some(col_palette.accent_red),
                                egui::vec2(20.0, 20.0),
                                "Remove column",
                            )
                            .clicked()
                            {
                                remove_col = Some(idx);
                            }
                        });
                    }
                });
            }

            ui.separator();

            // Export controls bar (RPT-3, RPT-4, RPT-5, RPT-8, RPT-9)
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut state.report_window.include_header, "Header Row");
                ui.checkbox(
                    &mut state.report_window.export_selected_only,
                    "Selected Rows Only",
                );
                ui.separator();
                ui.label("Separator:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.report_window.delimited_separator_input)
                        .desired_width(48.0),
                );
                egui::ComboBox::from_id_salt("delim_quote_mode")
                    .selected_text(match state.report_window.delimited_quote_mode {
                        DelimitedQuoteMode::QuoteContainingSeparator => "Quote if needed",
                        DelimitedQuoteMode::QuoteAll => "Quote all",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut state.report_window.delimited_quote_mode,
                            DelimitedQuoteMode::QuoteContainingSeparator,
                            "Quote if needed",
                        );
                        ui.selectable_value(
                            &mut state.report_window.delimited_quote_mode,
                            DelimitedQuoteMode::QuoteAll,
                            "Quote all",
                        );
                    });

                if ui.button("Export Delimited...").clicked() {
                    export_action = Some("delimited");
                }
                if ui.button("Export Fixed-Width...").clicked() {
                    export_action = Some("fixed");
                }
                if ui.button("Export CSV...").clicked() {
                    export_action = Some("csv");
                }
                if ui.button("Export JSON...").clicked() {
                    export_action = Some("json");
                }
                if ui.button("Copy to Clipboard").clicked() {
                    copy_clipboard = true;
                }
            });

            if let Some(ref msg) = state.report_window.status_or_error {
                ui.colored_label(Color32::LIGHT_YELLOW, msg);
            }

            ui.separator();

            // Sortable report table (RPT-2, SORT-3)
            show_report_table_grid(ui, &mut state.report_window, &mut header_clicked_col);
        });

    state.report_window.open = open;

    if let Some(col_idx) = remove_col {
        state
            .report_window
            .remove_column(col_idx, &state.doc.records, &state.dictionary);
    } else if rebuild_needed {
        state
            .report_window
            .rebuild_table(&state.doc.records, &state.dictionary);
    }

    if let Some(col_idx) = header_clicked_col {
        if let Some(ref mut tbl) = state.report_window.table {
            let _ = tbl.sort_by_column_toggle(col_idx);
        }
    }

    if copy_clipboard {
        if let Some(ref tbl) = state.report_window.table {
            let text = tbl.format_clipboard_text(
                &state.report_window.active_scope(),
                state.report_window.use_descriptions_in_header,
            );
            ctx.copy_text(text);
            state.report_window.status_or_error =
                Some("Copied report table to clipboard.".to_string());
        }
    }

    if let Some(kind) = export_action {
        handle_report_export(kind, &mut state.report_window, settings);
    }

    if save_def_clicked {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.toml", state.report_window.definition.name))
            .add_filter("Report Definition (*.toml)", &["toml"])
            .save_file()
        {
            if let Some(parent) = path.parent() {
                match save_report_definition_in(&state.report_window.definition, parent) {
                    Ok(saved) => {
                        state.report_window.status_or_error =
                            Some(format!("Saved definition to {}", saved.display()));
                    }
                    Err(e) => state.report_window.status_or_error = Some(e.to_string()),
                }
            }
        }
    }

    if load_def_clicked {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Report Definition (*.toml)", &["toml"])
            .pick_file()
        {
            match load_report_definition_file(&path) {
                Ok(def) => {
                    state.report_window.per_segment_grain =
                        matches!(def.grain, ReportRowGrain::PerSegment { .. });
                    if let ReportRowGrain::PerSegment { driving_segment } = def.grain {
                        state.report_window.driving_segment_input =
                            String::from_utf8_lossy(&driving_segment).into_owned();
                    }
                    state.report_window.definition = def;
                    state.report_window.fixed_specs.clear();
                    state
                        .report_window
                        .rebuild_table(&state.doc.records, &state.dictionary);
                }
                Err(e) => state.report_window.status_or_error = Some(e.to_string()),
            }
        }
    }

    show_multi_column_sort_window(ctx, &mut state.report_window);
}

fn show_report_table_grid(
    ui: &mut Ui,
    rw: &mut ReportWindowState,
    header_clicked_col: &mut Option<usize>,
) {
    let Some(ref tbl) = rw.table else {
        ui.weak("No report built.");
        return;
    };

    let col_count = tbl.definition.columns.len();
    if col_count == 0 {
        ui.weak("Add at least one column to build a report.");
        return;
    }

    let headers = tbl.headers(rw.use_descriptions_in_header).to_vec();
    let total_rows = tbl.rows.len();
    let active_sort = tbl.active_sort.clone();
    let mut clicked_row: Option<(usize, bool)> = None;

    let mut builder = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(48.0).at_least(36.0)); // Row #

    for i in 0..col_count {
        if i + 1 == col_count {
            builder = builder.column(Column::remainder().at_least(90.0));
        } else {
            builder = builder.column(Column::initial(140.0).at_least(80.0));
        }
    }

    builder
        .header(22.0, |mut header| {
            header.col(|ui| {
                ui.strong("#");
            });
            for (col_idx, h_text) in headers.iter().enumerate() {
                header.col(|ui| {
                    let dir_marker = active_sort
                        .iter()
                        .find(|k| k.column_index == col_idx)
                        .map_or("", |k| match k.direction {
                            SortDirection::Ascending => " ▲",
                            SortDirection::Descending => " ▼",
                        });
                    let label = format!("{h_text}{dir_marker}");
                    if ui.button(RichText::new(label).strong()).clicked() {
                        *header_clicked_col = Some(col_idx);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(20.0, total_rows, |mut row| {
                let row_idx = row.index();
                let rdata = &tbl.rows[row_idx];
                let is_sel = rw.selected_rows.contains(&row_idx);
                row.set_selected(is_sel);

                row.col(|ui| {
                    let resp = ui.add(
                        egui::Label::new(RichText::new(format!("{}", row_idx + 1)).monospace())
                            .sense(Sense::click()),
                    );
                    if resp.clicked() {
                        let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                        clicked_row = Some((row_idx, ctrl));
                    }
                });

                for cell in &rdata.cells {
                    row.col(|ui| {
                        let resp = ui.add(
                            egui::Label::new(RichText::new(cell).monospace()).sense(Sense::click()),
                        );
                        if resp.clicked() {
                            let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                            clicked_row = Some((row_idx, ctrl));
                        }
                    });
                }
            });
        });

    if let Some((r_idx, toggle)) = clicked_row {
        if toggle {
            if !rw.selected_rows.remove(&r_idx) {
                rw.selected_rows.insert(r_idx);
            }
        } else {
            rw.selected_rows.clear();
            rw.selected_rows.insert(r_idx);
        }
    }
}

fn show_multi_column_sort_window(ctx: &Context, rw: &mut ReportWindowState) {
    let Some(mut dlg) = rw.multi_sort_dialog.clone() else {
        return;
    };
    let col_names = rw.definition.columns.clone();
    if col_names.is_empty() {
        rw.multi_sort_dialog = None;
        return;
    }

    let mut close = false;
    let mut apply = false;

    Window::new("Multi-Column Sort (up to 3 keys)")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            show_sort_key_row(ui, "Primary Key:", &mut dlg.key1, &col_names);
            ui.checkbox(&mut dlg.use_key2, "Enable 2nd Key");
            if dlg.use_key2 {
                show_sort_key_row(ui, "2nd Key:", &mut dlg.key2, &col_names);
            }
            ui.checkbox(&mut dlg.use_key3, "Enable 3rd Key");
            if dlg.use_key3 {
                show_sort_key_row(ui, "3rd Key:", &mut dlg.key3, &col_names);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Sort").clicked() {
                    apply = true;
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if apply {
        let mut keys = vec![dlg.key1];
        if dlg.use_key2 {
            keys.push(dlg.key2);
        }
        if dlg.use_key3 {
            keys.push(dlg.key3);
        }
        if let Some(ref mut tbl) = rw.table {
            if let Err(e) = tbl.sort_multi(&keys) {
                rw.status_or_error = Some(e.to_string());
            }
        }
    }

    if close {
        rw.multi_sort_dialog = None;
    } else {
        rw.multi_sort_dialog = Some(dlg);
    }
}

fn show_sort_key_row(ui: &mut Ui, label: &str, spec: &mut SortKeySpec, cols: &[ReportColumn]) {
    ui.horizontal(|ui| {
        ui.label(label);
        let current_name = cols
            .get(spec.column_index)
            .map_or_else(|| "?".to_string(), |c| c.address.to_string());
        egui::ComboBox::from_id_salt(format!("multi_sort_col_{label}"))
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                for (idx, col) in cols.iter().enumerate() {
                    ui.selectable_value(&mut spec.column_index, idx, col.address.to_string());
                }
            });
        ui.radio_value(&mut spec.direction, SortDirection::Ascending, "Asc");
        ui.radio_value(&mut spec.direction, SortDirection::Descending, "Desc");
    });
}

fn handle_report_export(kind: &str, rw: &mut ReportWindowState, settings: &AppSettings) {
    let Some(ref tbl) = rw.table else {
        return;
    };
    let scope = rw.active_scope();
    let enc = settings.export_encoding;

    let bytes_res: Result<Vec<u8>, ReportError> = match kind {
        "delimited" => tbl.export_delimited(&DelimitedExportOptions {
            scope,
            include_header: rw.include_header,
            use_descriptions_in_header: rw.use_descriptions_in_header,
            separator_input: rw.delimited_separator_input.clone(),
            quote_mode: rw.delimited_quote_mode,
            encoding: enc,
        }),
        "fixed" => tbl.export_fixed_width(&FixedWidthExportOptions {
            scope,
            include_header: rw.include_header,
            use_descriptions_in_header: rw.use_descriptions_in_header,
            columns: rw.fixed_specs.clone(),
            encoding: enc,
        }),
        "csv" => tbl.export_csv(
            &scope,
            rw.include_header,
            rw.use_descriptions_in_header,
            enc,
        ),
        "json" => tbl.export_json(&scope, enc),
        _ => return,
    };

    match bytes_res {
        Ok(bytes) => {
            if let Some(path) = rfd::FileDialog::new().save_file() {
                match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        rw.status_or_error = Some(format!("Exported report to {}", path.display()));
                    }
                    Err(e) => rw.status_or_error = Some(format!("Export write error: {e}")),
                }
            }
        }
        Err(e) => {
            rw.status_or_error = Some(e.to_string());
        }
    }
}
