//! GUI Directory Scan window (`SCAN-1..SCAN-13`, `SCAN-15`, `SORT-3`, `G-5`).
//!
//! Provides a non-blocking background directory scanner with progress & cancellation,
//! a sortable per-file results table, rollup and group-by views, CSV/JSON/clipboard export,
//! and "Open Selected File(s) in Main Window" handoff (`SCAN-15`).

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use egui::{Color32, Context, RichText, Sense, Ui, Window};
use egui_extras::{Column, TableBuilder};
use hl7_core::{
    scan_directory, DirectoryScanConfig, DirectoryScanReport, FieldAddress, FileScanResult,
    FileScanStatus, GroupByKey, RecoveryMode, ScanProgress, SortDirection,
};

use crate::config::AppSettings;
use crate::state::SessionState;

/// Sortable columns in the GUI Directory Scan table (`SCAN-13`, `SORT-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanSortColumn {
    /// Relative display path.
    #[default]
    Path,
    /// Size in bytes.
    Size,
    /// Count of `MSH` messages.
    Msh,
    /// Count of `FHS` records.
    Fhs,
    /// Count of `BHS` records.
    Bhs,
    /// Count of `BTS` records.
    Bts,
    /// Count of `FTS` records.
    Fts,
    /// File status label.
    Status,
}

/// Active tab inside the Directory Scan window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanViewTab {
    /// Per-file table (`SCAN-4`, `SCAN-13`).
    #[default]
    Files,
    /// Grand total, extension rollups, and subdirectory rollups (`SCAN-6`).
    Rollups,
    /// Combinable group-by breakdown (`SCAN-7`, `SCAN-8`).
    GroupBy,
}

enum ScanWorkerMessage {
    Progress(ScanProgress),
    Completed(Result<DirectoryScanReport, String>),
}

struct ActiveScanJob {
    rx: Receiver<ScanWorkerMessage>,
    cancel: Arc<AtomicBool>,
    progress: Option<ScanProgress>,
}

/// State for the GUI Directory Scan window (`SCAN-1..SCAN-15`).
pub struct DirectoryScanWindowState {
    /// Whether the scan window is open.
    pub open: bool,
    /// Directory path text input.
    pub dir_input: String,
    /// Comma-separated extension patterns (default `"*.txt, *.dat, *.hl7"`).
    pub ext_input: String,
    /// Recurse into subdirectories (default `true`).
    pub recursive: bool,
    /// Comma-separated exclude globs.
    pub exclude_input: String,
    /// Group by `MSH-9` message type (`SCAN-7`).
    pub group_by_type: bool,
    /// Group by `MSH-3/4` sending app/facility (`SCAN-7`).
    pub group_by_sending: bool,
    /// Group by `MSH-5/6` receiving app/facility (`SCAN-7`).
    pub group_by_receiving: bool,
    /// Group by `MSH-7` message date (`SCAN-7`).
    pub group_by_date: bool,
    /// Optional custom field address for group-by (`SCAN-8`).
    pub custom_group_by_address: String,
    /// Check for duplicate `MSH-10` control IDs (`SCAN-12`).
    pub check_duplicate_control_ids: bool,
    /// Active view tab.
    pub active_tab: ScanViewTab,
    /// Current sort column (`SORT-3`).
    pub sort_column: ScanSortColumn,
    /// Current sort direction (`SORT-3`).
    pub sort_direction: SortDirection,
    /// Selected row indices in the file results table (`SCAN-15`).
    pub selected_rows: BTreeSet<usize>,
    /// Most recent completed scan report.
    pub report: Option<DirectoryScanReport>,
    /// Status or error message.
    pub status_or_error: Option<String>,
    active_job: Option<ActiveScanJob>,
}

impl Default for DirectoryScanWindowState {
    fn default() -> Self {
        Self {
            open: false,
            dir_input: ".".to_string(),
            ext_input: "*.txt, *.dat, *.hl7".to_string(),
            recursive: true,
            exclude_input: String::new(),
            group_by_type: true,
            group_by_sending: false,
            group_by_receiving: false,
            group_by_date: false,
            custom_group_by_address: String::new(),
            check_duplicate_control_ids: false,
            active_tab: ScanViewTab::Files,
            sort_column: ScanSortColumn::Path,
            sort_direction: SortDirection::Ascending,
            selected_rows: BTreeSet::new(),
            report: None,
            status_or_error: None,
            active_job: None,
        }
    }
}

impl DirectoryScanWindowState {
    /// Build a [`DirectoryScanConfig`] from current UI inputs.
    pub fn build_config(&self, settings: &AppSettings) -> Result<DirectoryScanConfig, String> {
        let mut group_by = Vec::new();
        if self.group_by_type {
            group_by.push(GroupByKey::MessageType);
        }
        if self.group_by_sending {
            group_by.push(GroupByKey::SendingAppFacility);
        }
        if self.group_by_receiving {
            group_by.push(GroupByKey::ReceivingAppFacility);
        }
        if self.group_by_date {
            group_by.push(GroupByKey::MessageDate);
        }
        let custom_trimmed = self.custom_group_by_address.trim();
        if !custom_trimmed.is_empty() {
            let addr: FieldAddress = custom_trimmed
                .parse()
                .map_err(|e: hl7_core::AddressError| e.to_string())?;
            group_by.push(GroupByKey::CustomAddress(addr));
        }

        let extension_patterns: Vec<String> = self
            .ext_input
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        let exclude_globs: Vec<String> = self
            .exclude_input
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        Ok(DirectoryScanConfig {
            root_dir: PathBuf::from(self.dir_input.trim()),
            extension_patterns,
            recursive: self.recursive,
            exclude_globs,
            envelope: settings.mllp_envelope(),
            group_by,
            check_duplicate_control_ids: self.check_duplicate_control_ids,
            ..DirectoryScanConfig::default()
        })
    }

    /// Launch a background scan worker thread (`G-5`, `SCAN-11`).
    pub fn start_scan(&mut self, settings: &AppSettings, ctx: Option<Context>) {
        let cfg = match self.build_config(settings) {
            Ok(c) => c,
            Err(e) => {
                self.status_or_error = Some(e);
                return;
            }
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let tx_prog = tx.clone();
        let ctx_prog = ctx.clone();

        std::thread::spawn(move || {
            let res = scan_directory(&cfg, &cancel_worker, move |p| {
                let _ = tx_prog.send(ScanWorkerMessage::Progress(p));
                if let Some(ref c) = ctx_prog {
                    c.request_repaint();
                }
            })
            .map_err(|e| e.to_string());

            let _ = tx.send(ScanWorkerMessage::Completed(res));
            if let Some(ref c) = ctx {
                c.request_repaint();
            }
        });

        self.active_job = Some(ActiveScanJob {
            rx,
            cancel,
            progress: None,
        });
        self.status_or_error = Some("Scanning directory...".to_string());
    }

    /// Poll background scan progress and completion without blocking (`G-5`).
    pub fn poll(&mut self) {
        let mut completed_res = None;
        if let Some(ref mut job) = self.active_job {
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    ScanWorkerMessage::Progress(p) => job.progress = Some(p),
                    ScanWorkerMessage::Completed(res) => {
                        completed_res = Some(res);
                        break;
                    }
                }
            }
        }
        if let Some(res) = completed_res {
            self.active_job = None;
            match res {
                Ok(report) => {
                    let msg = format!(
                        "Scanned {} file(s): {} MSH message(s) ({} KiB).",
                        report.grand_total.files,
                        report.grand_total.msh_count,
                        report.grand_total.size_bytes / 1024
                    );
                    self.selected_rows.clear();
                    self.report = Some(report);
                    self.status_or_error = Some(msg);
                }
                Err(err) => {
                    self.status_or_error = Some(format!("Scan failed: {err}"));
                }
            }
        }
    }

    /// Toggle sort on `col` (`SORT-3`, `SCAN-13`).
    pub fn sort_by_column(&mut self, col: ScanSortColumn) {
        if self.sort_column == col {
            self.sort_direction = self.sort_direction.toggle();
        } else {
            self.sort_column = col;
            self.sort_direction = SortDirection::Ascending;
        }
        if let Some(ref mut report) = self.report {
            let dir = self.sort_direction;
            report.files.sort_by(|a, b| {
                let ord = compare_scan_rows(a, b, col);
                match dir {
                    SortDirection::Ascending => ord,
                    SortDirection::Descending => ord.reverse(),
                }
            });
        }
    }

    /// Collect the deduplicated filesystem paths of the currently selected rows (`SCAN-15`).
    pub fn selected_filesystem_paths(&self) -> Vec<PathBuf> {
        let Some(ref report) = self.report else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for &idx in &self.selected_rows {
            if let Some(file_res) = report.files.get(idx) {
                if !paths.contains(&file_res.filesystem_path) {
                    paths.push(file_res.filesystem_path.clone());
                }
            }
        }
        paths
    }
}

fn compare_scan_rows(a: &FileScanResult, b: &FileScanResult, col: ScanSortColumn) -> Ordering {
    match col {
        ScanSortColumn::Path => a.display_path.cmp(&b.display_path),
        ScanSortColumn::Size => a.size_bytes.cmp(&b.size_bytes),
        ScanSortColumn::Msh => a.msh_count.cmp(&b.msh_count),
        ScanSortColumn::Fhs => a.fhs_count.cmp(&b.fhs_count),
        ScanSortColumn::Bhs => a.bhs_count.cmp(&b.bhs_count),
        ScanSortColumn::Bts => a.bts_count.cmp(&b.bts_count),
        ScanSortColumn::Fts => a.fts_count.cmp(&b.fts_count),
        ScanSortColumn::Status => a.status.display_label().cmp(&b.status.display_label()),
    }
}

/// Render the GUI Directory Scan window (`SCAN-13`, `SCAN-15`).
pub fn show_directory_scan_window(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    state.scan_window.poll();
    if !state.scan_window.open {
        return;
    }

    let mut open = state.scan_window.open;
    let mut start_clicked = false;
    let mut open_selected_paths: Option<Vec<PathBuf>> = None;
    let mut sort_clicked: Option<ScanSortColumn> = None;

    Window::new("Directory Scan — Headless Message Counter")
        .open(&mut open)
        .resizable(true)
        .default_width(920.0)
        .default_height(600.0)
        .show(ctx, |ui| {
            // Directory & pattern inputs (SCAN-1)
            ui.horizontal(|ui| {
                ui.label("Directory:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.scan_window.dir_input)
                        .desired_width(300.0),
                );
                if ui.button("Browse...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        state.scan_window.dir_input = folder.display().to_string();
                    }
                }
                ui.checkbox(&mut state.scan_window.recursive, "Recursive");
            });

            ui.horizontal(|ui| {
                ui.label("Extensions:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.scan_window.ext_input)
                        .desired_width(180.0),
                );
                ui.label("Exclude Globs:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.scan_window.exclude_input)
                        .desired_width(180.0),
                );
                ui.checkbox(
                    &mut state.scan_window.check_duplicate_control_ids,
                    "Check Duplicate MSH-10",
                );
            });

            // Group-by options (SCAN-7, SCAN-8)
            ui.horizontal_wrapped(|ui| {
                ui.label("Group By:");
                ui.checkbox(&mut state.scan_window.group_by_type, "MSH-9 (Type)");
                ui.checkbox(&mut state.scan_window.group_by_sending, "MSH-3/4 (Sending)");
                ui.checkbox(
                    &mut state.scan_window.group_by_receiving,
                    "MSH-5/6 (Receiving)",
                );
                ui.checkbox(&mut state.scan_window.group_by_date, "MSH-7 (Date)");
                ui.label("Custom Field (SCAN-8):");
                ui.add(
                    egui::TextEdit::singleline(&mut state.scan_window.custom_group_by_address)
                        .desired_width(90.0),
                );
                if !state.scan_window.custom_group_by_address.trim().is_empty() {
                    ui.colored_label(
                        Color32::from_rgb(230, 160, 40),
                        "(Cost warning: scans beyond MSH)",
                    );
                }
            });

            ui.separator();

            // Controls & Progress bar (SCAN-11, SCAN-13, SCAN-15)
            ui.horizontal(|ui| {
                let is_running = state.scan_window.active_job.is_some();
                if ui
                    .add_enabled(!is_running, egui::Button::new("Start Scan"))
                    .clicked()
                {
                    start_clicked = true;
                }
                if let Some(ref job) = state.scan_window.active_job {
                    if ui.button("Cancel").clicked() {
                        job.cancel.store(true, AtomicOrdering::Relaxed);
                    }
                    if let Some(p) = job.progress {
                        let frac = if p.files_total > 0 {
                            p.files_done as f32 / p.files_total as f32
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .text(format!(
                                    "{}/{} files ({} / {} KiB)",
                                    p.files_done,
                                    p.files_total,
                                    p.bytes_done / 1024,
                                    p.bytes_total / 1024
                                ))
                                .desired_width(220.0),
                        );
                    }
                }

                ui.separator();
                let has_report = state.scan_window.report.is_some();
                if ui
                    .add_enabled(
                        !state.scan_window.selected_rows.is_empty(),
                        egui::Button::new("Open Selected in Main Window"),
                    )
                    .clicked()
                {
                    open_selected_paths = Some(state.scan_window.selected_filesystem_paths());
                }
                if ui
                    .add_enabled(has_report, egui::Button::new("Export CSV..."))
                    .clicked()
                {
                    if let Some(ref rep) = state.scan_window.report {
                        if let Ok(bytes) = rep.export_csv() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("scan_report.csv")
                                .save_file()
                            {
                                let _ = std::fs::write(path, bytes);
                            }
                        }
                    }
                }
                if ui
                    .add_enabled(has_report, egui::Button::new("Export JSON..."))
                    .clicked()
                {
                    if let Some(ref rep) = state.scan_window.report {
                        if let Ok(bytes) = rep.export_json() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("scan_report.json")
                                .save_file()
                            {
                                let _ = std::fs::write(path, bytes);
                            }
                        }
                    }
                }
                if ui
                    .add_enabled(has_report, egui::Button::new("Copy to Clipboard"))
                    .clicked()
                {
                    if let Some(ref rep) = state.scan_window.report {
                        ctx.copy_text(rep.format_table());
                        state.scan_window.status_or_error =
                            Some("Copied scan summary table to clipboard.".to_string());
                    }
                }
            });

            if let Some(ref msg) = state.scan_window.status_or_error {
                ui.colored_label(Color32::LIGHT_YELLOW, msg);
            }

            ui.separator();

            // View tabs
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut state.scan_window.active_tab,
                    ScanViewTab::Files,
                    "Files Table",
                );
                ui.selectable_value(
                    &mut state.scan_window.active_tab,
                    ScanViewTab::Rollups,
                    "Rollups (Grand Total / Ext / Subdir)",
                );
                ui.selectable_value(
                    &mut state.scan_window.active_tab,
                    ScanViewTab::GroupBy,
                    "Group-By Summary",
                );
            });
            ui.separator();

            match state.scan_window.active_tab {
                ScanViewTab::Files => {
                    show_scan_files_table(ui, &mut state.scan_window, &mut sort_clicked);
                }
                ScanViewTab::Rollups => {
                    show_scan_rollups(ui, &state.scan_window);
                }
                ScanViewTab::GroupBy => {
                    show_scan_group_by(ui, &state.scan_window);
                }
            }
        });

    state.scan_window.open = open;

    if start_clicked {
        state.scan_window.start_scan(settings, Some(ctx.clone()));
    }
    if let Some(col) = sort_clicked {
        state.scan_window.sort_by_column(col);
    }
    if let Some(paths) = open_selected_paths {
        if !paths.is_empty() {
            state.start_background_load(
                paths,
                RecoveryMode::RecoverNonHl7,
                None,
                settings,
                ctx.clone(),
            );
        }
    }
}

fn show_scan_files_table(
    ui: &mut Ui,
    sw: &mut DirectoryScanWindowState,
    sort_clicked: &mut Option<ScanSortColumn>,
) {
    let Some(ref report) = sw.report else {
        ui.weak("Run a directory scan to view results.");
        return;
    };

    let mut clicked_row: Option<(usize, bool)> = None;

    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(260.0).at_least(120.0)) // Path
        .column(Column::initial(80.0)) // Bytes
        .column(Column::initial(64.0)) // MSH
        .column(Column::initial(52.0)) // FHS
        .column(Column::initial(52.0)) // BHS
        .column(Column::initial(52.0)) // BTS
        .column(Column::initial(52.0)) // FTS
        .column(Column::remainder().at_least(140.0)) // Status
        .header(22.0, |mut h| {
            for (label, col) in [
                ("Path", ScanSortColumn::Path),
                ("Bytes", ScanSortColumn::Size),
                ("MSH", ScanSortColumn::Msh),
                ("FHS", ScanSortColumn::Fhs),
                ("BHS", ScanSortColumn::Bhs),
                ("BTS", ScanSortColumn::Bts),
                ("FTS", ScanSortColumn::Fts),
                ("Status", ScanSortColumn::Status),
            ] {
                h.col(|ui| {
                    let marker = if sw.sort_column == col {
                        match sw.sort_direction {
                            SortDirection::Ascending => " ▲",
                            SortDirection::Descending => " ▼",
                        }
                    } else {
                        ""
                    };
                    if ui
                        .button(RichText::new(format!("{label}{marker}")).strong())
                        .clicked()
                    {
                        *sort_clicked = Some(col);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(20.0, report.files.len(), |mut row| {
                let idx = row.index();
                let f = &report.files[idx];
                row.set_selected(sw.selected_rows.contains(&idx));

                let mut check_click = |resp: egui::Response, ui: &Ui| {
                    if resp.clicked() {
                        let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                        clicked_row = Some((idx, ctrl));
                    }
                };

                row.col(|ui| {
                    let r = ui.add(
                        egui::Label::new(RichText::new(&f.display_path).monospace())
                            .sense(Sense::click()),
                    );
                    check_click(r, ui);
                });
                row.col(|ui| {
                    let r = ui.add(
                        egui::Label::new(RichText::new(format!("{}", f.size_bytes)).monospace())
                            .sense(Sense::click()),
                    );
                    check_click(r, ui);
                });
                for val in [
                    f.msh_count,
                    f.fhs_count,
                    f.bhs_count,
                    f.bts_count,
                    f.fts_count,
                ] {
                    row.col(|ui| {
                        let r = ui.add(
                            egui::Label::new(RichText::new(format!("{val}")).monospace())
                                .sense(Sense::click()),
                        );
                        check_click(r, ui);
                    });
                }
                row.col(|ui| {
                    let mut label = f.status.display_label();
                    if !f.batch_mismatches.is_empty() {
                        let m = &f.batch_mismatches[0];
                        label = format!(
                            "{label} (BTS-1={} vs actual={})",
                            m.declared_bts1, m.actual_count
                        );
                    }
                    let color = match f.status {
                        FileScanStatus::Ok => Color32::LIGHT_GREEN,
                        FileScanStatus::BatchCountMismatch | FileScanStatus::Unreadable(_) => {
                            Color32::LIGHT_RED
                        }
                        FileScanStatus::ContainsNonHl7Data => Color32::from_rgb(230, 170, 50),
                        FileScanStatus::NoHl7Found | FileScanStatus::Skipped(_) => Color32::GRAY,
                    };
                    let r = ui.add(
                        egui::Label::new(RichText::new(label).color(color)).sense(Sense::click()),
                    );
                    check_click(r, ui);
                });
            });
        });

    if let Some((idx, toggle)) = clicked_row {
        if toggle {
            if !sw.selected_rows.remove(&idx) {
                sw.selected_rows.insert(idx);
            }
        } else {
            sw.selected_rows.clear();
            sw.selected_rows.insert(idx);
        }
    }
}

fn show_scan_rollups(ui: &mut Ui, sw: &DirectoryScanWindowState) {
    let Some(ref report) = sw.report else {
        ui.weak("Run a directory scan to view rollups.");
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Grand Total");
        ui.monospace(format!(
            "Files: {} | Bytes: {} | MSH: {} | FHS: {} | BHS: {} | BTS: {} | FTS: {} | Batch Mismatch Files: {}",
            report.grand_total.files,
            report.grand_total.size_bytes,
            report.grand_total.msh_count,
            report.grand_total.fhs_count,
            report.grand_total.bhs_count,
            report.grand_total.bts_count,
            report.grand_total.fts_count,
            report.grand_total.batch_mismatch_files,
        ));
        if sw.check_duplicate_control_ids {
            let approx = if report.duplicate_control_ids_approximate {
                " (approximate)"
            } else {
                ""
            };
            ui.monospace(format!(
                "Duplicate MSH-10 Control IDs across scan: {}{approx}",
                report.duplicate_control_ids_total
            ));
        }

        ui.separator();
        ui.heading("Rollup by Extension");
        for (ext, r) in &report.by_extension {
            ui.monospace(format!(
                "{ext:<12} -> Files: {:>5} | Bytes: {:>10} | MSH: {:>8} | FHS: {:>4} | BHS: {:>4} | BTS: {:>4} | FTS: {:>4}",
                r.files, r.size_bytes, r.msh_count, r.fhs_count, r.bhs_count, r.bts_count, r.fts_count
            ));
        }

        ui.separator();
        ui.heading("Rollup by Subdirectory");
        for (sub, r) in &report.by_subdirectory {
            ui.monospace(format!(
                "{sub:<24} -> Files: {:>5} | Bytes: {:>10} | MSH: {:>8}",
                r.files, r.size_bytes, r.msh_count
            ));
        }
    });
}

fn show_scan_group_by(ui: &mut Ui, sw: &DirectoryScanWindowState) {
    let Some(ref report) = sw.report else {
        ui.weak("Run a directory scan to view group-by results.");
        return;
    };
    if report.group_by_rows.is_empty() {
        ui.weak("No group-by rows (enable at least one Group-By key before scanning).");
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.strong(format!(
            "Keys: {}  (-> Message Count)",
            report.group_by_headers.join(" | ")
        ));
        ui.separator();
        for row in &report.group_by_rows {
            ui.monospace(format!(
                "{:<48} -> {}",
                row.key_values.join(" | "),
                row.message_count
            ));
        }
    });
}
