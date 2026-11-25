//! Top-level `eframe::App` shell for `pipe_wrench` (§3..§5, G-5, NFR-6, NFR-7).

use std::sync::atomic::Ordering;

use egui::{Context, Key, KeyboardShortcut, Modifiers};
use hl7_core::{format_memory_bytes, ExportEncoding, MemoryBudget, RecoveryMode, SortDirection};

use crate::config::{AppSettings, SaveTerminatorSetting};
use crate::state::{
    FilterBuilderOp, FilterCombineMode, FindReplaceScope, FindReplaceTab, PendingUnsavedAction,
    SessionState,
};
use crate::strings;
use crate::ui::detail_pane::DetailPaneAction;
use crate::ui::icons::{
    icon_button, icon_labeled_button, icon_menu_button, icon_menu_button_tinted,
    icon_submenu_button, icon_toggle_button, icon_toggle_labeled_button, paint_icon, IconKind,
    ICON_SIZE_INLINE,
};
use crate::ui::net_panels::TcpIpDockTab;
use crate::ui::record_list::RecordListAction;
use crate::ui::theme::{
    add_action_button_job, apply_unified_control_metrics, styled_singleline_text_edit,
    toggle_pill_button, ThemePalette, CONTROL_CORNER_RADIUS, CONTROL_HEIGHT,
};
use crate::ui::{
    compare_modal, detail_pane, dialogs, net_panels, record_list, reports, scan_window, theme,
};

/// Configuration for automated `--screenshot <PATH>` capture mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotConfig {
    /// Destination PNG file path.
    pub output_path: std::path::PathBuf,
    /// `true` for Dark Modern theme (default), `false` for Light Modern theme.
    pub dark_mode: bool,
}

#[derive(Debug, Clone)]
struct ScreenshotCaptureState {
    output_path: std::path::PathBuf,
    frames_rendered: u32,
    requested: bool,
}

/// Main desktop application struct implementing [`eframe::App`].
pub struct PipeWrenchApp {
    /// Persisted user settings and display options.
    pub settings: AppSettings,
    /// Active session state.
    pub state: SessionState,
    /// Allow immediate close after user confirms discard/save on exit (FS-8).
    allow_exit: bool,
    /// Active automated screenshot capture state (`--screenshot <PATH>`).
    screenshot_capture: Option<ScreenshotCaptureState>,
    /// Cached live system memory / process RSS snapshot for the bottom status bar.
    cached_memory_budget: MemoryBudget,
    /// Last timestamp when `cached_memory_budget` was refreshed from the OS.
    last_memory_refresh: std::time::Instant,
    /// Lazily cached procedural app icon texture for the About modal header.
    about_icon_texture: Option<egui::TextureHandle>,
}

impl Default for PipeWrenchApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PipeWrenchApp {
    /// Construct the application, loading persisted TOML settings and any user dictionary.
    pub fn new() -> Self {
        let settings = AppSettings::load_default();
        let mut state = SessionState::new();
        if let Some(ref user_dict) = settings.user_dictionary_path {
            let _ = state.dictionary.merge_file(user_dict);
        }
        Self {
            settings,
            state,
            allow_exit: false,
            screenshot_capture: None,
            cached_memory_budget: MemoryBudget::from_system(),
            last_memory_refresh: std::time::Instant::now(),
            about_icon_texture: None,
        }
    }

    /// Construct a GUI application pre-loaded with curated synthetic HL7 v2.5.1 sample messages
    /// for automated `--screenshot <PATH>` capture (strictly synthetic data, zero PHI).
    pub fn new_for_screenshot(cfg: ScreenshotConfig) -> Self {
        let settings = AppSettings {
            dark_mode: Some(cfg.dark_mode),
            ui_zoom: 1.0,
            record_pane_width: 350.0,
            ..AppSettings::default()
        };
        let mut state = SessionState::new();
        let synthetic_samples = concat!(
            "MSH|^~\\&|EHR_APP|SYNTH_HOSP|LAB_SYS|REGIONAL_LAB|20261123101500||ADT^A01|SYNTH0001|P|2.5.1\r",
            "EVN|A01|20261123101500|||CLERK01^ADMIN^ALICE\r",
            "PID|1||MRN10042^^^SYNTH_HOSP^MR~ALT9001^^^CLINIC^PI||EXAMPLE^JORDAN^M||19840719|F|||420 OAK AVE^^SPRINGFIELD^IL^62704||555-0101\r",
            "PV1|1|I|MED^302^B^SYNTH_HOSP||||1042^SAMPLE^MORGAN^^^DR|||MED||||1|||1042^SAMPLE^MORGAN^^^DR|INP|V20261123\r",
            "ROL|1|AD|AT|1042^SAMPLE^MORGAN^^^DR|20261123101500||||||420 OAK AVE^^SPRINGFIELD^IL^62704|555-0199|MED^302^B\r",
            "GT1|1||EXAMPLE^JORDAN^M||420 OAK AVE^^SPRINGFIELD^IL^62704|555-0101||19840719|F||SEL||||||ACME^CORP|100 COMMERCE WAY^^SPRINGFIELD^IL^62701\r",
            "MSH|^~\\&|LAB_SYS|REGIONAL_LAB|EHR_APP|SYNTH_HOSP|20261123101842||ORU^R01|SYNTH0002|P|2.5.1\r",
            "PID|1||MRN10042^^^SYNTH_HOSP^MR||EXAMPLE^JORDAN^M||19840719|F\r",
            "OBR|1|ORD5001|FIL5001|2345-7^Glucose [Mass/volume] in Serum^LN|||20261123101200\r",
            "OBX|1|NM|2345-7^Glucose^LN||104|mg/dL|70-99|H|||F\r",
            "NTE|1|L|Fasting specimen\\.br\\Verified by auto-analyzer \\F\\ QC passed\r",
            "MSH|^~\\&|EHR_APP|SYNTH_HOSP|RAD_RIS|SYNTH_HOSP|20261123102210||ORM^O01|SYNTH0003|P|2.5.1\r",
            "PID|1||MRN20088^^^SYNTH_HOSP^MR||TESTPATIENT^ALEX||19720308|M\r",
            "ORC|NW|ORD6002|||||1^^^20261123102200^^R\r",
            "OBR|1|ORD6002||71046^Chest X-Ray 2 Views^CPT4\r",
            "MSH|^~\\&|SCHED_SYS|SYNTH_HOSP|EHR_APP|SYNTH_HOSP|20261123103000||SIU^S12|SYNTH0004|P|2.5.1\r",
            "SCH|APT9001|FIL9001|||||CHECKUP^Follow-up Visit||30|MIN\r",
            "PID|1||MRN30115^^^SYNTH_HOSP^MR||DEMO^CASEY||19911130|U\r",
            "MSH|^~\\&|EHR_APP|SYNTH_HOSP|BILLING|SYNTH_HOSP|20261123104500||ADT^A08|SYNTH0005|P|2.5.1\r",
            "EVN|A08|20261123104500\r",
            "PID|1||MRN10042^^^SYNTH_HOSP^MR||EXAMPLE^JORDAN^M||19840719|F\r",
        );
        state.paste_from_text(synthetic_samples, false, &settings);
        state.detail_view_mode = crate::state::DetailViewMode::Split;
        state.detail_raw_split_height = 210.0;
        state.filter_bar_open = true;
        state.select_record(0, false, &settings);
        state.expanded_segments.clear();
        state.expanded_segments.insert(0); // MSH
        state.expanded_segments.insert(2); // PID
        state.expanded_segments.insert(4); // ROL
        state.undo_stack.mark_saved();

        Self {
            settings,
            state,
            allow_exit: false,
            screenshot_capture: Some(ScreenshotCaptureState {
                output_path: cfg.output_path,
                frames_rendered: 0,
                requested: false,
            }),
            cached_memory_budget: MemoryBudget::from_system(),
            last_memory_refresh: std::time::Instant::now(),
            about_icon_texture: None,
        }
    }

    /// Construct the window title including dirty marker `*` and `"(sorted)"` indicator (`SORT-1`, `FS-8`).
    pub fn window_title(&self) -> String {
        let dirty = if self.state.undo_stack.is_dirty() {
            "*"
        } else {
            ""
        };
        let sorted = if self.state.is_sorted {
            " (sorted)"
        } else {
            ""
        };
        format!(
            "{}{} — {}{}",
            dirty,
            strings::APP_TITLE,
            self.state.source.display_label(),
            sorted
        )
    }

    fn build_open_file_dialog(&self) -> rfd::FileDialog {
        let mut dialog = rfd::FileDialog::new();
        if let Some(active) = self
            .settings
            .file_filter_patterns
            .get(self.settings.active_filter_index)
        {
            let ext = active.trim_start_matches("*.");
            if ext != "*" && !ext.is_empty() {
                dialog = dialog.add_filter(active, &[ext]);
            }
        }
        dialog = dialog.add_filter("HL7 & Archives", &["hl7", "txt", "dat", "gz", "zip"]);
        dialog.add_filter("All Files (*.*)", &["*"])
    }

    fn prompt_open_files(&mut self, ctx: &Context) {
        if let Some(paths) = self.build_open_file_dialog().pick_files() {
            if self.state.undo_stack.is_dirty() {
                self.state.pending_unsaved_action = Some(PendingUnsavedAction::OpenFiles(paths));
            } else {
                self.state.request_open_files_with_guardrail(
                    paths,
                    RecoveryMode::Strict,
                    None,
                    false,
                    &self.settings,
                    ctx.clone(),
                    None,
                );
            }
        }
    }

    fn prompt_append_files(&mut self, ctx: &Context) {
        if let Some(paths) = self.build_open_file_dialog().pick_files() {
            self.state.request_open_files_with_guardrail(
                paths,
                RecoveryMode::Strict,
                None,
                true,
                &self.settings,
                ctx.clone(),
                None,
            );
        }
    }

    fn prompt_save_file(&mut self, selected_only: bool) -> bool {
        let dialog = rfd::FileDialog::new()
            .add_filter("HL7 Files (*.hl7, *.txt, *.dat)", &["hl7", "txt", "dat"])
            .add_filter("All Files (*.*)", &["*"]);
        if let Some(path) = dialog.save_file() {
            if let Err(e) = self.state.save_to_path(path, selected_only, &self.settings) {
                let msg = format!("Save error: {e}");
                self.state.status_message = msg.clone();
                self.state.modal_alert = Some(msg);
                return false;
            }
            return true;
        }
        false
    }

    fn prompt_export_filtered(&mut self) -> bool {
        let dialog = rfd::FileDialog::new()
            .set_title("Export Filtered Messages")
            .set_file_name("filtered.hl7")
            .add_filter("HL7 Files (*.hl7, *.txt, *.dat)", &["hl7", "txt", "dat"])
            .add_filter("All Files (*.*)", &["*"]);
        if let Some(path) = dialog.save_file() {
            if let Err(e) = self.state.export_filtered_to_path(path, &self.settings) {
                let msg = format!("Export filtered error: {e}");
                self.state.status_message = msg.clone();
                self.state.modal_alert = Some(msg);
                return false;
            }
            return true;
        }
        false
    }

    fn execute_save_now(&mut self) -> bool {
        // FS-1, FS-2: Save Now overwrites single source file, or falls back to Save As
        // when the source is clipboard, TCP/IP, or multi-file.
        if let Some(path) = self.state.source.single_file_path().cloned() {
            if let Err(e) = self.state.save_to_path(path, false, &self.settings) {
                let msg = format!("Save error: {e}");
                self.state.status_message = msg.clone();
                self.state.modal_alert = Some(msg);
                return false;
            }
            true
        } else {
            self.prompt_save_file(false)
        }
    }

    fn handle_global_shortcuts(&mut self, ctx: &Context) {
        let open_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let append_open_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
        let save_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
        let undo_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        let redo_y_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
        let redo_z_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        let replace_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::H);
        let edit_field_sc = KeyboardShortcut::new(Modifiers::NONE, Key::F2);
        let save_as_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
        let report_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
        let jump_rec_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::G);
        let prefs_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);
        let scan_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::D);
        let help_sc = KeyboardShortcut::new(Modifiers::NONE, Key::F1);
        let find_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
        let filter_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
        let search_next_sc = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
        let search_prev_sc = KeyboardShortcut::new(Modifiers::SHIFT, Key::F3);
        let zoom_in_eq_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Equals);
        let zoom_in_plus_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus);
        let zoom_out_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Minus);
        let zoom_reset_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num0);

        if ctx.input_mut(|i| i.consume_shortcut(&append_open_sc)) {
            self.prompt_append_files(ctx);
        } else if ctx.input_mut(|i| i.consume_shortcut(&open_sc)) {
            self.prompt_open_files(ctx);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&zoom_in_eq_sc))
            || ctx.input_mut(|i| i.consume_shortcut(&zoom_in_plus_sc))
        {
            self.settings.zoom_in();
            self.settings.save_default();
        } else if ctx.input_mut(|i| i.consume_shortcut(&zoom_out_sc)) {
            self.settings.zoom_out();
            self.settings.save_default();
        } else if ctx.input_mut(|i| i.consume_shortcut(&zoom_reset_sc)) {
            self.settings.reset_zoom();
            self.settings.save_default();
        }

        // Ctrl + Mouse Scroll Wheel global zoom (D-28)
        let ctrl_scroll_y = ctx.input(|i| {
            if i.modifiers.command || i.modifiers.ctrl {
                i.raw_scroll_delta.y
            } else {
                0.0
            }
        });
        if ctrl_scroll_y > 0.0 {
            self.settings.zoom_in();
            self.settings.save_default();
        } else if ctrl_scroll_y < 0.0 {
            self.settings.zoom_out();
            self.settings.save_default();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&save_as_sc)) && !self.state.doc.is_empty() {
            let _ = self.prompt_save_file(false);
        } else if ctx.input_mut(|i| i.consume_shortcut(&save_sc)) && !self.state.doc.is_empty() {
            let _ = self.execute_save_now();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&redo_z_sc))
            || ctx.input_mut(|i| i.consume_shortcut(&redo_y_sc))
        {
            let _ = self.state.redo(&self.settings);
        } else if ctx.input_mut(|i| i.consume_shortcut(&undo_sc)) {
            let _ = self.state.undo(&self.settings);
        }
        let has_messages = self.state.has_messages();
        if ctx.input_mut(|i| i.consume_shortcut(&replace_sc)) && has_messages {
            self.state.open_replace_panel();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&edit_field_sc))
            && self.state.selected_field_row.is_some()
        {
            let _ = self.state.start_inline_field_edit(&self.settings);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&report_sc)) && has_messages {
            self.state.open_report_from_selected_field();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&jump_rec_sc)) && has_messages {
            self.state.begin_record_jump();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&prefs_sc)) {
            self.state.preferences_open = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&scan_sc)) {
            self.state.scan_window.open = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&help_sc)) {
            self.state.shortcuts_help_open = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&search_prev_sc)) && has_messages {
            if !self.state.search_bar_open && self.state.search_query.trim().is_empty() {
                self.state.open_find_panel();
            } else {
                let _ = self.state.jump_search(false, &self.settings);
            }
        } else if ctx.input_mut(|i| i.consume_shortcut(&search_next_sc)) && has_messages {
            if !self.state.search_bar_open && self.state.search_query.trim().is_empty() {
                self.state.open_find_panel();
            } else {
                let _ = self.state.jump_search(true, &self.settings);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&filter_sc)) && has_messages {
            self.state.filter_bar_open = !self.state.filter_bar_open;
        } else if ctx.input_mut(|i| i.consume_shortcut(&find_sc)) && has_messages {
            self.state.open_find_panel();
        }
        if self.state.about_open && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.state.about_open = false;
        } else if self.state.search_bar_open && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.state.close_find_replace_panel();
        }

        // Global Ctrl+C copy & Ctrl+V paste of raw HL7 record(s) when focus is NOT inside a TextEdit (D-29).
        if !ctx.wants_keyboard_input() {
            let copy_triggered = ctx.input(|i| {
                i.events.iter().any(|ev| matches!(ev, egui::Event::Copy))
                    || ((i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(Key::C))
            });
            if copy_triggered && !self.state.selected_records.is_empty() {
                let clip_text = self
                    .state
                    .format_selected_records_for_clipboard(&self.settings);
                ctx.copy_text(clip_text);
                self.state.status_message = format!(
                    "Copied {} message(s) to clipboard.",
                    self.state.selected_records.len()
                );
            }
            if let Some(pasted) = read_paste_event_text(ctx) {
                let _ = self.state.paste_records_undoable(&pasted, &self.settings);
            }
        }
    }

    fn show_menu_bar(&mut self, ui: &mut egui::Ui, ctx: &Context) {
        let palette = ThemePalette::for_visuals(ui.visuals());
        let has_messages = self.state.has_messages();
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if icon_menu_button(ui, IconKind::Open, "Open File(s)...", Some("Ctrl+O"), true)
                    .clicked()
                {
                    ui.close_menu();
                    self.prompt_open_files(ctx);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Open,
                    "Append File(s)...",
                    Some("Ctrl+Shift+O"),
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.prompt_append_files(ctx);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Scan,
                    "Directory Scan (Headless Count)...",
                    Some("Ctrl+Shift+D"),
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.scan_window.open = true;
                }
                ui.separator();
                if icon_menu_button(ui, IconKind::Save, "Save", Some("Ctrl+S"), has_messages)
                    .clicked()
                {
                    ui.close_menu();
                    let _ = self.execute_save_now();
                }
                if icon_menu_button(
                    ui,
                    IconKind::Save,
                    "Save As...",
                    Some("Ctrl+Shift+S"),
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.prompt_save_file(false);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Save,
                    "Save Selected Messages...",
                    None,
                    !self.state.selected_records.is_empty(),
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.prompt_save_file(true);
                }
                if let Some(ref visible) = self.state.filtered_record_indices {
                    if icon_menu_button(
                        ui,
                        IconKind::Save,
                        "Export Filtered...",
                        None,
                        !visible.is_empty(),
                    )
                    .clicked()
                    {
                        ui.close_menu();
                        let _ = self.prompt_export_filtered();
                    }
                }
                if icon_menu_button(
                    ui,
                    IconKind::Save,
                    "Export Synthetic (Scrub PHI)...",
                    None,
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.scrub_phi_dialog = Some(crate::state::ScrubPhiDialog::new(
                        crate::state::ScrubPhiMode::ExportSynthetic,
                    ));
                }
                ui.separator();
                icon_submenu_button(
                    ui,
                    IconKind::Settings,
                    "SAVE-TERM Policy",
                    true,
                    None,
                    |ui| {
                        let mut changed = false;
                        for (label, val) in [
                            ("Preserve source (default)", SaveTerminatorSetting::Preserve),
                            ("CR (0x0D)", SaveTerminatorSetting::Cr),
                            ("CRLF (0x0D 0x0A)", SaveTerminatorSetting::CrLf),
                            ("LF (0x0A)", SaveTerminatorSetting::Lf),
                        ] {
                            if ui
                                .radio_value(&mut self.settings.save_terminator, val, label)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                        if changed {
                            self.settings.save_default();
                        }
                    },
                );
                if ui
                    .checkbox(
                        &mut self.settings.disk_envelope_on_save,
                        "Disk Envelope Characters (MLLP)",
                    )
                    .changed()
                {
                    self.settings.save_default();
                }
                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Settings,
                    "Preferences...",
                    Some("Ctrl+,"),
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.preferences_open = true;
                }
            });

            ui.menu_button("Edit", |ui| {
                let undo_label = match self.state.undo_stack.undo_description() {
                    Some(d) => format!("Undo {d}"),
                    None => "Undo".to_string(),
                };
                if icon_menu_button(
                    ui,
                    IconKind::Undo,
                    &undo_label,
                    Some("Ctrl+Z"),
                    self.state.undo_stack.can_undo(),
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.state.undo(&self.settings);
                }

                let redo_label = match self.state.undo_stack.redo_description() {
                    Some(d) => format!("Redo {d}"),
                    None => "Redo".to_string(),
                };
                if icon_menu_button(
                    ui,
                    IconKind::Redo,
                    &redo_label,
                    Some("Ctrl+Y"),
                    self.state.undo_stack.can_redo(),
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.state.redo(&self.settings);
                }

                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Edit,
                    "Edit Field...",
                    Some("F2"),
                    self.state.selected_field_row.is_some(),
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.state.open_edit_field_dialog(&self.settings);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Edit,
                    "Edit Message...",
                    None,
                    self.state.selected_record.is_some(),
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_edit_record_dialog();
                }
                if icon_menu_button(
                    ui,
                    IconKind::Edit,
                    "Assign Value to Field...",
                    None,
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_assign_field_dialog();
                }
                if icon_menu_button(ui, IconKind::Find, "Find...", Some("Ctrl+F"), has_messages)
                    .clicked()
                {
                    ui.close_menu();
                    self.state.open_find_panel();
                }
                if icon_menu_button(
                    ui,
                    IconKind::Replace,
                    "Find & Replace...",
                    Some("Ctrl+H"),
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_replace_panel();
                }

                ui.separator();
                let can_del_seg = self.state.selected_segment.is_some();
                if icon_menu_button_tinted(
                    ui,
                    IconKind::Delete,
                    "Delete Segment",
                    None,
                    can_del_seg,
                    Some(palette.accent_red),
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.state.request_delete_selected_segment(&self.settings);
                }
                let can_del_msgs_menu = has_messages && !self.state.selected_records.is_empty();
                icon_submenu_button(
                    ui,
                    IconKind::Delete,
                    "Delete Message(s)",
                    can_del_msgs_menu,
                    Some(palette.accent_red),
                    |ui| {
                        let can_del_msgs = !self.state.selected_records.is_empty();
                        if icon_menu_button_tinted(
                            ui,
                            IconKind::Delete,
                            "Delete Selected Message(s)",
                            None,
                            can_del_msgs,
                            Some(palette.accent_red),
                        )
                        .clicked()
                        {
                            ui.close_menu();
                            let indices: Vec<usize> =
                                self.state.selected_records.iter().copied().collect();
                            let _ = self.state.request_delete_records(indices, &self.settings);
                        }
                        if let Some(cur) = self.state.selected_record {
                            if icon_menu_button_tinted(
                                ui,
                                IconKind::Delete,
                                "Delete This Message Through First",
                                None,
                                true,
                                Some(palette.accent_red),
                            )
                            .clicked()
                            {
                                ui.close_menu();
                                let indices: Vec<usize> = (0..=cur).collect();
                                let _ = self.state.request_delete_records(indices, &self.settings);
                            }
                            if icon_menu_button_tinted(
                                ui,
                                IconKind::Delete,
                                "Delete This Message Through Last",
                                None,
                                true,
                                Some(palette.accent_red),
                            )
                            .clicked()
                            {
                                ui.close_menu();
                                let indices: Vec<usize> = (cur..self.state.doc.len()).collect();
                                let _ = self.state.request_delete_records(indices, &self.settings);
                            }
                        }
                    },
                );

                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Delimiters,
                    "Change Message Delimiters...",
                    None,
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_change_delimiters_dialog();
                }
                if icon_menu_button(
                    ui,
                    IconKind::Replace,
                    "Scrub PHI in Loaded Messages...",
                    None,
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.scrub_phi_dialog = Some(crate::state::ScrubPhiDialog::new(
                        crate::state::ScrubPhiMode::InPlaceLoaded,
                    ));
                }
                if ui
                    .checkbox(
                        &mut self.settings.prompt_before_deleting,
                        "Prompt Before Deleting",
                    )
                    .changed()
                {
                    self.settings.save_default();
                }

                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Paste,
                    "Paste Message(s)",
                    Some("Ctrl+V"),
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self.state.paste_from_clipboard_result(
                        read_clipboard_text(ctx),
                        false,
                        &self.settings,
                    );
                }
                if icon_menu_button(ui, IconKind::Paste, "Paste (Replace List)", None, true)
                    .clicked()
                {
                    ui.close_menu();
                    if self.state.undo_stack.is_dirty() {
                        self.state.pending_unsaved_action =
                            Some(PendingUnsavedAction::PasteReplace);
                    } else {
                        let _ = self.state.paste_from_clipboard_result(
                            read_clipboard_text(ctx),
                            true,
                            &self.settings,
                        );
                    }
                }
                if icon_menu_button(ui, IconKind::Paste, "Paste Append", None, true).clicked() {
                    ui.close_menu();
                    let _ = self.state.paste_from_clipboard_result(
                        read_clipboard_text(ctx),
                        false,
                        &self.settings,
                    );
                }
            });

            ui.menu_button("View", |ui| {
                let mut rebuild = false;
                if ui
                    .checkbox(
                        &mut self.settings.display_empty_fields,
                        "Display Empty Fields",
                    )
                    .changed()
                {
                    rebuild = true;
                }
                if ui
                    .checkbox(
                        &mut self.settings.display_compound_fields,
                        "Display Compound Fields",
                    )
                    .changed()
                {
                    rebuild = true;
                }
                if ui
                    .checkbox(
                        &mut self.settings.display_field_descriptions,
                        "Display Field Descriptions",
                    )
                    .changed()
                {
                    rebuild = true;
                }
                if ui
                    .checkbox(
                        &mut self.settings.display_segment_descriptions,
                        "Display Segment Descriptions",
                    )
                    .changed()
                {
                    rebuild = true;
                }
                ui.separator();
                let view_palette = crate::ui::theme::ThemePalette::for_visuals(ui.visuals());
                if crate::ui::icons::icon_menu_checkbox(
                    ui,
                    IconKind::DecodeEscapes,
                    &mut self.settings.decode_escape_sequences,
                    "Decode Escape Sequences (Read-Only)",
                    Some(view_palette.accent),
                )
                .changed()
                {
                    rebuild = true;
                }
                if ui
                    .checkbox(&mut self.settings.show_raw_pane, "Show Raw Pane")
                    .changed()
                {
                    self.settings.save_default();
                }
                if ui
                    .checkbox(
                        &mut self.settings.show_control_chars,
                        "Show Control Characters (¶)",
                    )
                    .changed()
                {
                    self.settings.save_default();
                }
                ui.separator();
                let selected_count = self.state.selected_records.len();
                let can_compare =
                    crate::ui::record_list::can_compare_selected_records(selected_count);
                let compare_menu_tip = if can_compare {
                    "Compare Selected Message(s) (1–5 selected)".to_string()
                } else {
                    format!(
                        "Select 1 to {} messages to compare (currently {selected_count} selected)",
                        crate::state::MAX_COMPARE_MESSAGES
                    )
                };
                if ui
                    .add_enabled_ui(can_compare, |ui| {
                        icon_menu_button(
                            ui,
                            IconKind::Compare,
                            "Compare Messages...",
                            None,
                            can_compare,
                        )
                    })
                    .inner
                    .on_hover_text(&compare_menu_tip)
                    .on_disabled_hover_text(&compare_menu_tip)
                    .clicked()
                {
                    ui.close_menu();
                    self.state.open_compare_selected_messages();
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of("compare_hl7_messages_window"),
                        egui::ViewportCommand::Focus,
                    );
                }
                ui.separator();
                if icon_menu_button(ui, IconKind::ZoomIn, "Zoom In", Some("Ctrl+="), true).clicked()
                {
                    ui.close_menu();
                    self.settings.zoom_in();
                    self.settings.save_default();
                }
                if icon_menu_button(ui, IconKind::ZoomOut, "Zoom Out", Some("Ctrl+-"), true)
                    .clicked()
                {
                    ui.close_menu();
                    self.settings.zoom_out();
                    self.settings.save_default();
                }
                if icon_menu_button(
                    ui,
                    IconKind::ZoomReset,
                    "Reset Zoom (100%)",
                    Some("Ctrl+0"),
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.settings.reset_zoom();
                    self.settings.save_default();
                }
                ui.separator();
                icon_submenu_button(ui, IconKind::Settings, "Theme", true, None, |ui| {
                    for (label, mode) in [
                        ("Follow System", None),
                        ("Dark Mode", Some(true)),
                        ("Light Mode", Some(false)),
                    ] {
                        if ui
                            .radio_value(&mut self.settings.dark_mode, mode, label)
                            .changed()
                        {
                            apply_theme(ctx, self.settings.dark_mode);
                            self.settings.save_default();
                        }
                    }
                });

                if rebuild {
                    self.settings.save_default();
                    self.state.rebuild_selected_record_cache(&self.settings);
                }
            });

            ui.menu_button("Reports & Sort", |ui| {
                let can_sort = self.state.doc.len() > 1;
                if icon_menu_button(
                    ui,
                    IconKind::Sort,
                    "Sort Messages by MSH-7 (Ascending)",
                    None,
                    can_sort,
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self
                        .state
                        .sort_records_by_msh7(SortDirection::Ascending, &self.settings);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Sort,
                    "Sort Messages by MSH-7 (Descending)",
                    None,
                    can_sort,
                )
                .clicked()
                {
                    ui.close_menu();
                    let _ = self
                        .state
                        .sort_records_by_msh7(SortDirection::Descending, &self.settings);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Sort,
                    "Sort Messages by Field...",
                    None,
                    can_sort,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_sort_records_dialog();
                }
                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Report,
                    "Build Report from Selected Field...",
                    Some("Ctrl+R"),
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.open_report_from_selected_field();
                }
                if icon_menu_button(
                    ui,
                    IconKind::Report,
                    "Open Report Builder...",
                    None,
                    has_messages,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.report_window.open_with_field(
                        None,
                        &self.state.doc.records,
                        &self.state.dictionary,
                    );
                }
                ui.separator();
                icon_submenu_button(
                    ui,
                    IconKind::Settings,
                    "Report Export Encoding",
                    true,
                    None,
                    |ui| {
                        let mut changed = false;
                        for (label, enc) in [
                            ("UTF-8 (default)", ExportEncoding::Utf8),
                            ("UTF-8 with BOM", ExportEncoding::Utf8Bom),
                            ("Windows-1252", ExportEncoding::Windows1252),
                        ] {
                            if ui
                                .radio_value(&mut self.settings.export_encoding, enc, label)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                        if changed {
                            self.settings.save_default();
                        }
                    },
                );
            });

            ui.menu_button("Network", |ui| {
                if icon_menu_button(
                    ui,
                    IconKind::Network,
                    "Toggle Docked TCP/IP Panel",
                    None,
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.network.toggle_dock();
                }
                ui.separator();
                if icon_menu_button(
                    ui,
                    IconKind::Network,
                    "Send HL7 Messages (Client Tab)",
                    None,
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.network.open_dock_tab(TcpIpDockTab::ClientSender);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Network,
                    "Receive HL7 Messages (Listener Tab)",
                    None,
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state
                        .network
                        .open_dock_tab(TcpIpDockTab::ServerListener);
                }
                if icon_menu_button(ui, IconKind::Network, "Peer Connections Tab", None, true)
                    .clicked()
                {
                    ui.close_menu();
                    self.state.network.open_dock_tab(TcpIpDockTab::Connections);
                }
                if icon_menu_button(
                    ui,
                    IconKind::Report,
                    "Received ACK / NAK Log Tab",
                    None,
                    true,
                )
                .clicked()
                {
                    ui.close_menu();
                    self.state.network.open_dock_tab(TcpIpDockTab::AckLog);
                }
            });

            ui.menu_button("Help", |ui| {
                if icon_menu_button(ui, IconKind::Help, "Keyboard Shortcuts", Some("F1"), true)
                    .clicked()
                {
                    ui.close_menu();
                    self.state.shortcuts_help_open = true;
                }
                ui.separator();
                if icon_menu_button(ui, IconKind::Help, "About pipe_wrench", None, true).clicked() {
                    ui.close_menu();
                    self.state.about_open = true;
                }
            });
        });
    }

    fn show_modals(&mut self, ctx: &Context) {
        // Phase 1: Large File Memory Guardrail modal
        if let Some(dlg) = self.state.memory_guardrail_dialog.clone() {
            let palette = ThemePalette::for_visuals(&ctx.style().visuals);
            let mut load_subset = false;
            let mut load_all = false;
            let mut cancel_load = false;

            egui::Window::new("Large File Memory Guardrail")
                .id(egui::Id::new("large_file_memory_guardrail_modal"))
                .collapsible(false)
                .resizable(false)
                .min_width(470.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(490.0);
                    let amber_bg = if palette.is_dark {
                        egui::Color32::from_rgb(0x3B, 0x2B, 0x10)
                    } else {
                        egui::Color32::from_rgb(0xFE, 0xF3, 0xC7)
                    };
                    let amber_border = if palette.is_dark {
                        egui::Color32::from_rgb(0xD9, 0x77, 0x06)
                    } else {
                        egui::Color32::from_rgb(0xB4, 0x53, 0x09)
                    };
                    let amber_fg = if palette.is_dark {
                        egui::Color32::from_rgb(0xFD, 0xE6, 0x8A)
                    } else {
                        egui::Color32::from_rgb(0x78, 0x35, 0x0F)
                    };

                    egui::Frame::NONE
                        .fill(amber_bg)
                        .stroke(egui::Stroke::new(1.0_f32, amber_border))
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "The selected file(s) are projected to exceed this system's safe in-memory load budget.",
                                )
                                .strong()
                                .color(amber_fg),
                            );
                        });

                    ui.add_space(8.0);
                    egui::Grid::new("memory_guardrail_stats_grid")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            ui.weak("Total File Size on Disk:");
                            ui.label(
                                egui::RichText::new(format_memory_bytes(dlg.total_disk_bytes))
                                    .monospace()
                                    .strong(),
                            );
                            ui.end_row();

                            ui.weak("Estimated Heap Footprint:");
                            ui.label(
                                egui::RichText::new(format_memory_bytes(dlg.estimated_heap_bytes))
                                    .monospace()
                                    .strong()
                                    .color(amber_border),
                            );
                            ui.end_row();

                            ui.weak("Live Available System RAM:");
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} (Safe Budget: {})",
                                    format_memory_bytes(dlg.available_ram_bytes),
                                    format_memory_bytes(dlg.safe_budget_bytes),
                                ))
                                .monospace(),
                            );
                            ui.end_row();
                        });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let primary_label = format!(
                            "Load Safe Subset (First {} Messages)",
                            dlg.safe_message_limit
                        );
                        let primary_btn = egui::Button::new(
                            egui::RichText::new(primary_label)
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(37, 99, 235))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(29, 78, 216),
                        ))
                        .corner_radius(egui::CornerRadius::same(6))
                        .min_size(egui::vec2(210.0, 28.0));
                        if ui.add(primary_btn).clicked() {
                            load_subset = true;
                        }
                        if ui
                            .add(
                                egui::Button::new("Load All Anyway")
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .min_size(egui::vec2(115.0, 28.0)),
                            )
                            .clicked()
                        {
                            load_all = true;
                        }
                        if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .min_size(egui::vec2(72.0, 28.0)),
                            )
                            .clicked()
                        {
                            cancel_load = true;
                        }
                    });
                });

            if load_subset {
                self.state
                    .confirm_memory_guardrail_safe_subset(&self.settings, ctx.clone());
            } else if load_all {
                self.state
                    .confirm_memory_guardrail_load_all(&self.settings, ctx.clone());
            } else if cancel_load {
                self.state.cancel_memory_guardrail();
            }
        }

        // FO-11: Non-HL7 recovery confirmation prompt.
        if let Some((paths, append)) = self.state.pending_non_hl7_paths.clone() {
            egui::Window::new("Non-HL7 Content Detected")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(strings::PROMPT_NON_HL7_RECOVERY);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            self.state.pending_non_hl7_paths = None;
                            self.state.start_background_load_with_mode(
                                paths,
                                RecoveryMode::RecoverNonHl7,
                                None,
                                append,
                                &self.settings,
                                ctx.clone(),
                            );
                        }
                        if ui.button("No").clicked() {
                            self.state.pending_non_hl7_paths = None;
                            self.state.status_message = "Load canceled.".to_string();
                        }
                    });
                });
        }

        // FO-4, D-5: Multi-entry .zip picker dialog.
        if let Some((zip_path, entries, mut flags, append)) = self.state.pending_zip_picker.clone()
        {
            let mut close_picker = false;
            let mut load_chosen: Option<Vec<String>> = None;
            egui::Window::new("Select Archive Entries to Load")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "'{}' contains {} entries:",
                        zip_path.display(),
                        entries.len()
                    ));
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for (name, flag) in entries.iter().zip(flags.iter_mut()) {
                                ui.checkbox(flag, name);
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Load Selected").clicked() {
                            let chosen: Vec<String> = entries
                                .iter()
                                .zip(flags.iter())
                                .filter(|&(_, &f)| f)
                                .map(|(n, _)| n.clone())
                                .collect();
                            load_chosen = Some(chosen);
                            close_picker = true;
                        }
                        if ui.button("First Entry Only").clicked() {
                            load_chosen = Some(entries.iter().take(1).cloned().collect());
                            close_picker = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_picker = true;
                        }
                    });
                });
            if close_picker {
                self.state.pending_zip_picker = None;
                if let Some(chosen) = load_chosen {
                    if !chosen.is_empty() {
                        self.state.start_background_load_with_mode(
                            vec![zip_path],
                            RecoveryMode::Strict,
                            Some(chosen),
                            append,
                            &self.settings,
                            ctx.clone(),
                        );
                    }
                }
            } else {
                self.state.pending_zip_picker = Some((zip_path, entries, flags, append));
            }
        }

        // FS-8: Unsaved changes confirmation modal.
        if let Some(pending) = self.state.pending_unsaved_action.clone() {
            egui::Window::new("Unsaved Changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(strings::PROMPT_UNSAVED_CHANGES);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() && self.execute_save_now() {
                            self.state.pending_unsaved_action = None;
                            self.complete_pending_unsaved_action(pending.clone(), ctx);
                        }
                        if ui.button("Discard").clicked() {
                            self.state.pending_unsaved_action = None;
                            self.state.undo_stack.mark_saved();
                            self.complete_pending_unsaved_action(pending, ctx);
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.pending_unsaved_action = None;
                        }
                    });
                });
        }

        // FO-12: Zero HL7 records found -> offer external editor / OS default viewer.
        if let Some(paths) = self.state.pending_no_hl7_paths.clone() {
            let mut close = false;
            let mut open_ext = false;
            egui::Window::new("No HL7 Messages Found")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(strings::MSG_NO_HL7_RECORDS_FOUND);
                    ui.label("Would you like to open the file in the configured external editor / OS viewer?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Open in External Editor / OS Viewer").clicked() {
                            open_ext = true;
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if open_ext {
                for p in &paths {
                    open_in_external_editor_or_os(p, self.settings.external_editor_path.as_deref());
                }
            }
            if close {
                self.state.pending_no_hl7_paths = None;
            }
        }

        // FO-1, FO-2, §5.4, FO-12, D-12: Preferences Window.
        if self.state.preferences_open {
            let mut open = self.state.preferences_open;
            let mut remove_idx = None;
            egui::Window::new("Preferences")
                .open(&mut open)
                .resizable(true)
                .default_width(520.0)
                .show(ctx, |ui| {
                    ui.heading("File Open / Save Filter Patterns (FO-1, FO-2)");
                    ui.horizontal(|ui| {
                        ui.label("New pattern (e.g. *.msg):");
                        ui.text_edit_singleline(&mut self.state.new_filter_pattern_input);
                        if ui
                            .button(add_action_button_job(ui, "Add Pattern", true))
                            .clicked()
                        {
                            let pat = self.state.new_filter_pattern_input.clone();
                            match self.settings.add_filter_pattern(pat) {
                                Ok(()) => {
                                    self.state.new_filter_pattern_input.clear();
                                    self.state.preferences_error = None;
                                    self.settings.save_default();
                                }
                                Err(e) => self.state.preferences_error = Some(e),
                            }
                        }
                    });
                    if let Some(ref err) = self.state.preferences_error {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    let patterns = self.settings.file_filter_patterns.clone();
                    for (idx, pat) in patterns.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .radio_value(&mut self.settings.active_filter_index, idx, pat)
                                .changed()
                            {
                                self.settings.save_default();
                            }
                            if patterns.len() > 1
                                && icon_labeled_button(
                                    ui,
                                    IconKind::Delete,
                                    "Remove",
                                    true,
                                    Some(ThemePalette::for_visuals(ui.visuals()).accent_red),
                                )
                                .clicked()
                            {
                                remove_idx = Some(idx);
                            }
                        });
                    }
                    ui.separator();
                    ui.heading("Message List Windowing / Paging (§5.1, NFR-1)");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Max messages per active page:");
                        for (label, val) in [
                            ("10,000 (Default)", 10_000usize),
                            ("25,000", 25_000usize),
                            ("50,000", 50_000usize),
                            ("Unlimited", 0usize),
                        ] {
                            if ui
                                .selectable_label(self.settings.max_records_per_page == val, label)
                                .clicked()
                            {
                                self.settings.max_records_per_page = val;
                                self.state.load_all_pages_override = false;
                                self.state.sync_record_page_to_selected(&self.settings);
                                self.settings.save_default();
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Custom page size (0 = Unlimited):");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.settings.max_records_per_page)
                                    .range(0..=1_000_000)
                                    .speed(100),
                            )
                            .changed()
                        {
                            self.state.load_all_pages_override = false;
                            self.state.sync_record_page_to_selected(&self.settings);
                            self.settings.save_default();
                        }
                    });

                    ui.separator();
                    ui.heading("Undo Memory Warning Limit (EDIT-1)");
                    let mut mib = (self.settings.undo_warning_bytes / (1024 * 1024)).max(1);
                    ui.horizontal(|ui| {
                        ui.label("Soft warning threshold (MiB):");
                        if ui
                            .add(egui::DragValue::new(&mut mib).range(1..=4096))
                            .changed()
                        {
                            self.settings.undo_warning_bytes = mib * 1024 * 1024;
                            self.settings.save_default();
                        }
                    });
                });
            if let Some(idx) = remove_idx {
                self.settings.remove_filter_pattern(idx);
                self.settings.save_default();
            }
            self.state.preferences_open = open;
        }

        // NFR-6: Keyboard Shortcuts Reference Window.
        if self.state.shortcuts_help_open {
            let mut open = self.state.shortcuts_help_open;
            egui::Window::new("Keyboard Shortcuts Reference")
                .open(&mut open)
                .resizable(false)
                .show(ctx, |ui| {
                    for (key, desc) in [
                        ("Ctrl+O", "Open File(s)"),
                        ("Ctrl+Shift+O", "Append File(s)"),
                        ("Ctrl+S", "Save"),
                        ("Ctrl+Shift+S", "Save As..."),
                        ("Ctrl+C", "Copy Selected Message(s) in Messages List"),
                        (
                            "Ctrl+V",
                            "Paste HL7 Message(s) from Clipboard (outside text inputs)",
                        ),
                        ("Ctrl+Z", "Undo"),
                        ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
                        ("Ctrl+F", "Open Find Panel"),
                        ("Ctrl+H", "Open Find & Replace Panel"),
                        (
                            "Ctrl+Shift+F",
                            "Toggle Message Filter Bar (Shift+Enter stacks rule)",
                        ),
                        ("F3 / Shift+F3", "Find Next / Find Previous Match"),
                        ("Escape", "Close Find & Replace Panel / Cancel Inline Edit"),
                        ("F2 / Double-click", "Edit Selected Field Inline"),
                        (
                            "Ctrl+Click / Shift+Click",
                            "Multi-Select Rows (Messages List / Segment Accordion)",
                        ),
                        ("Ctrl+R", "Build Report from Selected Field(s)"),
                        ("Ctrl+G", "Jump to Message Number (Msg # / Total)"),
                        (
                            "Ctrl+= / Ctrl+- / Ctrl+0",
                            "Zoom In / Zoom Out / Reset Zoom (100%)",
                        ),
                        ("Ctrl+Scroll Wheel", "Zoom In / Out"),
                        ("Ctrl+,", "Preferences"),
                        ("F1", "Keyboard Shortcuts Help"),
                        ("Arrow / Page / Home / End", "Navigate Messages List"),
                    ] {
                        ui.horizontal(|ui| {
                            ui.strong(format!("{key:<26}"));
                            ui.label(desc);
                        });
                    }
                });
            self.state.shortcuts_help_open = open;
        }

        // About pipe_wrench modal dialog.
        if self.state.about_open {
            let palette = ThemePalette::for_visuals(&ctx.style().visuals);
            let icon_tex = self
                .about_icon_texture
                .get_or_insert_with(|| {
                    let rgba = crate::icon::render_icon_rgba(64);
                    let image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &rgba);
                    ctx.load_texture(
                        "pipe_wrench_about_icon",
                        image,
                        egui::TextureOptions::LINEAR,
                    )
                })
                .clone();
            let trademark_color = if palette.is_dark {
                egui::Color32::from_rgb(0xB0, 0xB7, 0xC3)
            } else {
                egui::Color32::from_rgb(0x4B, 0x55, 0x63)
            };
            let version_pill_bg = palette.accent.gamma_multiply(0.16);
            let version_pill_border = palette.accent.gamma_multiply(0.55);
            let window_frame =
                egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(18));

            let mut open = self.state.about_open;
            let mut close_clicked = false;
            egui::Window::new("About pipe_wrench")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .frame(window_frame)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    apply_unified_control_metrics(ui);
                    ui.set_width(444.0);

                    // Header row: 40×40 icon + title + version badge
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.add(
                            egui::Image::from_texture(&icon_tex)
                                .fit_to_exact_size(egui::vec2(40.0, 40.0)),
                        );
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui.label(
                                    egui::RichText::new("pipe_wrench")
                                        .size(19.0)
                                        .strong()
                                        .color(palette.primary_text),
                                );
                                egui::Frame::NONE
                                    .fill(version_pill_bg)
                                    .stroke(egui::Stroke::new(1.0_f32, version_pill_border))
                                    .corner_radius(egui::CornerRadius::same(4))
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(concat!(
                                                "v",
                                                env!("CARGO_PKG_VERSION")
                                            ))
                                            .size(13.0)
                                            .monospace()
                                            .strong()
                                            .color(palette.accent),
                                        );
                                    });
                            });
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "Desktop and CLI workbench for HL7® v2 interface troubleshooting and conversion.",
                                    )
                                    .size(14.0)
                                    .color(palette.primary_text),
                                )
                                .wrap(),
                            );
                        });
                    });

                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            egui::RichText::new("Repository:")
                                .size(13.5)
                                .strong()
                                .color(palette.primary_text),
                        );
                        ui.hyperlink_to(
                            egui::RichText::new("https://github.com/brett-a-willis/pipe_wrench")
                                .size(13.5),
                            "https://github.com/brett-a-willis/pipe_wrench",
                        );
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "Copyright (c) 2026 Brett Willis — Licensed under the MIT License. Provided \"AS IS\", without warranty of any kind.",
                            )
                            .size(13.5)
                            .color(palette.primary_text),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "HL7® and Health Level Seven® are registered trademarks of Health Level Seven International. pipe_wrench is an independent utility and is not affiliated with, endorsed, or sponsored by Health Level Seven International.",
                            )
                            .size(12.5)
                            .line_height(Some(18.0))
                            .color(trademark_color),
                        )
                        .wrap(),
                    );

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), crate::ui::theme::CONTROL_HEIGHT),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.style_mut().text_styles.insert(
                                egui::TextStyle::Button,
                                egui::FontId::proportional(13.5),
                            );
                            if icon_labeled_button(ui, IconKind::Close, "Close", true, None)
                                .on_hover_text("Close About dialog (Esc)")
                                .clicked()
                            {
                                close_clicked = true;
                            }
                        },
                    );
                });
            self.state.about_open = open && !close_clicked;
        }

        // Deterministic PHI De-Identification / Export Synthetic modal.
        if let Some(mut dlg) = self.state.scrub_phi_dialog.clone() {
            let mut close_dialog = false;
            let mut run_action = false;
            let mut remove_custom_idx: Option<usize> = None;
            let mut clear_custom_all = false;
            let mut add_custom_rule: Option<hl7_core::CustomPhiRule> = None;
            let detected_z_paths = self.state.detected_custom_z_paths_in_selected();
            let custom_match_count = hl7_core::count_custom_phi_matches(
                &self.state.doc.records,
                &self.settings.custom_phi_rules,
            );
            let title = match dlg.mode {
                crate::state::ScrubPhiMode::ExportSynthetic => "Export Synthetic HL7 (Scrub PHI)",
                crate::state::ScrubPhiMode::InPlaceLoaded => "Scrub PHI in Loaded Messages",
            };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .default_width(560.0)
                .max_width(560.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let palette = ThemePalette::for_visuals(ui.visuals());
                    let is_dark = palette.is_dark;
                    match dlg.mode {
                        crate::state::ScrubPhiMode::ExportSynthetic => {
                            ui.label(
                                "Deterministically de-identifies PHI (names, MRNs/SSNs/visit IDs, DOBs, addresses, phones, and free-text references in OBX-5/NTE-3/TXA/Z*) into a new file without modifying the open session.",
                            );
                        }
                        crate::state::ScrubPhiMode::InPlaceLoaded => {
                            ui.label(
                                "Deterministically de-identifies PHI across all loaded messages in place as a single undoable operation (Ctrl+Z restores the original messages).",
                            );
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Seed (optional):");
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.seed_input)
                                .hint_text("Blank = random per-run seed")
                                .desired_width(270.0),
                        );
                        if ui
                            .button("Randomize")
                            .on_hover_text("Generate a fresh random seed string")
                            .clicked()
                        {
                            dlg.seed_input =
                                hl7_core::PhiScrubber::with_random_seed().seed().to_string();
                        }
                    });
                    ui.weak(
                        "Using the same seed across batches guarantees the same patient MRN, name, and encounter map to identical synthetic values.",
                    );
                    ui.add_space(8.0);

                    // Interactive "Additional Custom PHI Fields" Pill Grid section
                    egui::Frame::NONE
                        .fill(palette.side_panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, palette.divider))
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.strong("Additional Custom PHI Fields");
                                if !self.settings.custom_phi_rules.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "({custom_match_count} matching value(s) in loaded messages)"
                                        ))
                                        .small()
                                        .color(palette.accent),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if !self.settings.custom_phi_rules.is_empty()
                                            && ui
                                                .small_button("Clear Custom")
                                                .on_hover_text("Remove all custom PHI field rules")
                                                .clicked()
                                        {
                                            clear_custom_all = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);

                            // Quick-Add Bar: monospace path input + Scrub Mode dropdown + `+ Add` + optional Z* quick-pick
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                let has_err = dlg.custom_path_error.is_some();
                                let mut submit_path = false;
                                let stroke_col = if has_err {
                                    palette.accent_red
                                } else {
                                    palette.divider
                                };
                                egui::Frame::NONE
                                    .fill(palette.base_canvas)
                                    .stroke(egui::Stroke::new(1.0_f32, stroke_col))
                                    .corner_radius(egui::CornerRadius::same(4))
                                    .inner_margin(egui::Margin::symmetric(6, 3))
                                    .show(ui, |ui| {
                                        let path_resp = ui.add(
                                            egui::TextEdit::singleline(&mut dlg.custom_path_input)
                                                .id_salt("scrub_custom_phi_path_input")
                                                .code_editor()
                                                .frame(false)
                                                .hint_text("e.g. ZPI-4, PID-18.1, OBX-5")
                                                .desired_width(195.0),
                                        );
                                        if path_resp.changed() {
                                            dlg.custom_path_error = None;
                                        }
                                        if (path_resp.lost_focus() || path_resp.has_focus())
                                            && ui.input(|i| i.key_pressed(Key::Enter))
                                        {
                                            submit_path = true;
                                        }
                                    });

                                egui::ComboBox::from_id_salt("scrub_custom_phi_mode_combo")
                                    .selected_text(dlg.custom_mode.label())
                                    .width(88.0)
                                    .show_ui(ui, |ui| {
                                        for m in hl7_core::CustomPhiMode::ALL {
                                            ui.selectable_value(&mut dlg.custom_mode, m, m.label());
                                        }
                                    });

                                let can_add_custom = !dlg.custom_path_input.trim().is_empty();
                                if icon_labeled_button(
                                    ui,
                                    IconKind::Add,
                                    "Add",
                                    can_add_custom,
                                    Some(palette.accent_green),
                                )
                                .on_hover_text("Add custom HL7 path to scrub list (Enter)")
                                .clicked()
                                    || (submit_path && can_add_custom)
                                {
                                    add_custom_rule = Some(hl7_core::CustomPhiRule {
                                        path: dlg.custom_path_input.trim().to_string(),
                                        mode: dlg.custom_mode,
                                    });
                                }

                                if !detected_z_paths.is_empty() {
                                    egui::ComboBox::from_id_salt("scrub_z_quick_pick_combo")
                                        .selected_text("Z* Pick")
                                        .width(78.0)
                                        .show_ui(ui, |ui| {
                                            for zp in &detected_z_paths {
                                                if ui
                                                    .selectable_label(
                                                        false,
                                                        egui::RichText::new(zp).monospace(),
                                                    )
                                                    .clicked()
                                                {
                                                    add_custom_rule =
                                                        Some(hl7_core::CustomPhiRule {
                                                            path: zp.clone(),
                                                            mode: dlg.custom_mode,
                                                        });
                                                }
                                            }
                                        });
                                }
                            });

                            if let Some(ref err) = dlg.custom_path_error {
                                ui.label(
                                    egui::RichText::new(err)
                                        .small()
                                        .color(palette.accent_red),
                                );
                            }

                            ui.add_space(6.0);

                            // Bounded scrollable grid-aligned pill container
                            if self.settings.custom_phi_rules.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "No custom paths added. Add site-specific Z* fields, OBX-5 notes, or right-click any field in the tree.",
                                    )
                                    .small()
                                    .italics()
                                    .color(palette.muted_text),
                                );
                            } else {
                                egui::ScrollArea::vertical()
                                    .id_salt("custom_phi_pills_scroll")
                                    .max_height(96.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
                                            for (idx, rule) in
                                                self.settings.custom_phi_rules.iter().enumerate()
                                            {
                                                let dict_desc = rule
                                                    .path
                                                    .parse::<hl7_core::FieldAddress>()
                                                    .ok()
                                                    .and_then(|a| {
                                                        self.state
                                                            .dictionary
                                                            .field_description(&a)
                                                    })
                                                    .unwrap_or("[Custom Segment]");
                                                let pill_tip = format!(
                                                    "{} — {} (Mode: {})",
                                                    rule.path,
                                                    dict_desc,
                                                    rule.mode.label()
                                                );
                                                let pill_frame = egui::Frame::NONE
                                                    .fill(palette.elevated_bg)
                                                    .stroke(egui::Stroke::new(
                                                        1.0_f32,
                                                        palette.divider,
                                                    ))
                                                    .corner_radius(egui::CornerRadius::same(12))
                                                    .inner_margin(egui::Margin::symmetric(8, 3))
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.spacing_mut().item_spacing.x = 5.0;
                                                            ui.label(
                                                                egui::RichText::new(&rule.path)
                                                                    .monospace()
                                                                    .strong()
                                                                    .small()
                                                                    .color(palette.primary_text),
                                                            );
                                                            let mode_color = match rule.mode {
                                                                hl7_core::CustomPhiMode::Synthetic => {
                                                                    palette.accent
                                                                }
                                                                hl7_core::CustomPhiMode::Redact => {
                                                                    palette.accent_red
                                                                }
                                                                hl7_core::CustomPhiMode::Mask => {
                                                                    egui::Color32::from_rgb(
                                                                        0xD9, 0x77, 0x06,
                                                                    )
                                                                }
                                                            };
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    rule.mode.short_tag(),
                                                                )
                                                                .monospace()
                                                                .small()
                                                                .color(mode_color),
                                                            );
                                                            let (x_rect, x_resp) =
                                                                ui.allocate_exact_size(
                                                                    egui::vec2(14.0, 14.0),
                                                                    egui::Sense::click(),
                                                                );
                                                            if ui.is_rect_visible(x_rect) {
                                                                let fg = if x_resp.hovered() {
                                                                    palette.accent_red
                                                                } else {
                                                                    palette.muted_text
                                                                };
                                                                paint_icon(
                                                                    ui.painter(),
                                                                    IconKind::Close,
                                                                    x_rect.center(),
                                                                    fg,
                                                                    palette.elevated_bg,
                                                                    9.0,
                                                                );
                                                            }
                                                            if x_resp
                                                                .on_hover_text(format!(
                                                                    "Remove {}",
                                                                    rule.path
                                                                ))
                                                                .clicked()
                                                            {
                                                                remove_custom_idx = Some(idx);
                                                            }
                                                        });
                                                    });
                                                pill_frame.response.on_hover_text(pill_tip);
                                            }
                                        });
                                    });
                            }
                        });

                    ui.add_space(10.0);

                    // Mandatory best-effort PHI de-identification warning callout
                    let (warn_bg, warn_border, warn_title_fg, warn_body_fg) = if is_dark {
                        (
                            egui::Color32::from_rgb(0x33, 0x26, 0x0C),
                            egui::Color32::from_rgb(0xD2, 0x99, 0x22),
                            egui::Color32::from_rgb(0xF2, 0xCC, 0x60),
                            egui::Color32::from_rgb(0xE6, 0xDC, 0xC6),
                        )
                    } else {
                        (
                            egui::Color32::from_rgb(0xFF, 0xF8, 0xC5),
                            egui::Color32::from_rgb(0xBF, 0x87, 0x00),
                            egui::Color32::from_rgb(0x7D, 0x4E, 0x00),
                            egui::Color32::from_rgb(0x3B, 0x2E, 0x00),
                        )
                    };
                    egui::Frame::NONE
                        .fill(warn_bg)
                        .stroke(egui::Stroke::new(1.25_f32, warn_border))
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(hl7_core::PHI_SCRUB_DISCLAIMER_HEADING)
                                    .strong()
                                    .color(warn_title_fg),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(hl7_core::PHI_SCRUB_DISCLAIMER_BODY)
                                    .color(warn_body_fg),
                            );
                        });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let action_label = match dlg.mode {
                            crate::state::ScrubPhiMode::ExportSynthetic => "Export Synthetic...",
                            crate::state::ScrubPhiMode::InPlaceLoaded => "Scrub Loaded Messages",
                        };
                        let primary_btn = egui::Button::new(
                            egui::RichText::new(action_label)
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(37, 99, 235))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(29, 78, 216),
                        ))
                        .corner_radius(egui::CornerRadius::same(6))
                        .min_size(egui::vec2(150.0, 28.0));
                        if ui.add(primary_btn).clicked() {
                            run_action = true;
                        }
                        ui.add_space(4.0);
                        let cancel_btn = egui::Button::new("Cancel")
                            .corner_radius(egui::CornerRadius::same(6))
                            .min_size(egui::vec2(72.0, 28.0));
                        if ui.add(cancel_btn).clicked() {
                            close_dialog = true;
                        }
                    });
                });

            if let Some(rule) = add_custom_rule {
                match self.settings.add_custom_phi_rule(rule) {
                    Ok(()) => {
                        dlg.custom_path_input.clear();
                        dlg.custom_path_error = None;
                        self.settings.save_default();
                    }
                    Err(err) => {
                        dlg.custom_path_error = Some(err);
                    }
                }
            } else if let Some(idx) = remove_custom_idx {
                self.settings.remove_custom_phi_rule(idx);
                self.settings.save_default();
            } else if clear_custom_all {
                self.settings.clear_custom_phi_rules();
                self.settings.save_default();
            }

            if run_action {
                let seed_opt =
                    (!dlg.seed_input.trim().is_empty()).then_some(dlg.seed_input.as_str());
                match dlg.mode {
                    crate::state::ScrubPhiMode::ExportSynthetic => {
                        let file_dialog = rfd::FileDialog::new()
                            .set_title("Export Synthetic HL7 (PHI Scrubbed)")
                            .set_file_name("synthetic.hl7")
                            .add_filter("HL7 Files (*.hl7, *.txt, *.dat)", &["hl7", "txt", "dat"])
                            .add_filter("All Files (*.*)", &["*"]);
                        if let Some(path) = file_dialog.save_file() {
                            if let Err(e) =
                                self.state
                                    .export_synthetic_to_path(path, seed_opt, &self.settings)
                            {
                                let msg = format!("Export synthetic error: {e}");
                                self.state.status_message = msg.clone();
                                self.state.modal_alert = Some(msg);
                            }
                            close_dialog = true;
                        }
                    }
                    crate::state::ScrubPhiMode::InPlaceLoaded => {
                        if let Err(e) = self
                            .state
                            .scrub_loaded_records_undoable(seed_opt, &self.settings)
                        {
                            let msg = format!("Scrub PHI error: {e}");
                            self.state.status_message = msg.clone();
                            self.state.modal_alert = Some(msg);
                        }
                        close_dialog = true;
                    }
                }
            }

            if close_dialog {
                self.state.scrub_phi_dialog = None;
            } else {
                self.state.scrub_phi_dialog = Some(dlg);
            }
        }
    }

    fn show_icon_toolbar(&mut self, ui: &mut egui::Ui, ctx: &Context) {
        let palette = ThemePalette::for_visuals(ui.visuals());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            if icon_labeled_button(ui, IconKind::Open, "Open", true, None)
                .on_hover_text("Open HL7 file(s) (Ctrl+O)")
                .clicked()
            {
                self.prompt_open_files(ctx);
            }

            if icon_labeled_button(ui, IconKind::Save, "Save", !self.state.doc.is_empty(), None)
                .on_hover_text("Save (Ctrl+S)")
                .clicked()
            {
                let _ = self.execute_save_now();
            }

            let selected_count = self.state.selected_records.len();
            let can_compare = crate::ui::record_list::can_compare_selected_records(selected_count);
            let compare_tip = if can_compare {
                "Compare Selected Message(s) (1–5 selected)".to_string()
            } else {
                format!(
                    "Select 1 to {} messages to compare (currently {selected_count} selected)",
                    crate::state::MAX_COMPARE_MESSAGES
                )
            };
            if ui
                .add_enabled_ui(can_compare, |ui| {
                    icon_labeled_button(ui, IconKind::Compare, "Compare", can_compare, None)
                })
                .inner
                .on_hover_text(&compare_tip)
                .on_disabled_hover_text(&compare_tip)
                .clicked()
            {
                self.state.open_compare_selected_messages();
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("compare_hl7_messages_window"),
                    egui::ViewportCommand::Focus,
                );
            }

            ui.separator();

            let undo_tip = self.state.undo_stack.undo_description().map_or_else(
                || "Undo (Ctrl+Z)".to_string(),
                |d| format!("Undo {d} (Ctrl+Z)"),
            );
            if icon_labeled_button(
                ui,
                IconKind::Undo,
                "Undo",
                self.state.undo_stack.can_undo(),
                None,
            )
            .on_hover_text(undo_tip)
            .clicked()
            {
                let _ = self.state.undo(&self.settings);
            }

            let redo_tip = self.state.undo_stack.redo_description().map_or_else(
                || "Redo (Ctrl+Y)".to_string(),
                |d| format!("Redo {d} (Ctrl+Y)"),
            );
            if icon_labeled_button(
                ui,
                IconKind::Redo,
                "Redo",
                self.state.undo_stack.can_redo(),
                None,
            )
            .on_hover_text(redo_tip)
            .clicked()
            {
                let _ = self.state.redo(&self.settings);
            }

            ui.separator();

            let has_messages = self.state.has_messages();
            let filter_active = has_messages
                && (self.state.filter_bar_open || self.state.filtered_record_indices.is_some());
            if icon_toggle_labeled_button(
                ui,
                IconKind::Filter,
                "Filter",
                filter_active,
                has_messages,
                palette.accent,
            )
            .on_hover_text(
                "Toggle non-destructive Message Filter bar (Ctrl+Shift+F, e.g. MSH-9.1=ADT)",
            )
            .clicked()
            {
                self.state.filter_bar_open = !self.state.filter_bar_open;
            }

            if icon_toggle_labeled_button(
                ui,
                IconKind::Find,
                "Find",
                has_messages && self.state.search_bar_open,
                has_messages,
                palette.accent,
            )
            .on_hover_text("Open Find (Ctrl+F) / Find & Replace (Ctrl+H)")
            .clicked()
            {
                if self.state.search_bar_open {
                    self.state.close_find_replace_panel();
                } else {
                    self.state.open_find_panel();
                }
            }

            ui.separator();

            if icon_toggle_labeled_button(
                ui,
                IconKind::Network,
                "TCP/IP",
                self.state.network.docked_open,
                true,
                palette.accent,
            )
            .on_hover_text("Toggle docked MLLP / TLS 1.2/1.3 TCP/IP panel")
            .clicked()
            {
                self.state.network.toggle_dock();
            }
        });
    }

    fn show_filter_bar(&mut self, ui: &mut egui::Ui) {
        let palette = ThemePalette::for_visuals(ui.visuals());
        let mut export_filtered_clicked = false;
        let mut keep_only_clicked = false;
        let mut remove_rule_idx: Option<usize> = None;
        let mut new_combine_mode: Option<FilterCombineMode> = None;

        ui.scope(|ui| {
            apply_unified_control_metrics(ui);

            ui.horizontal_wrapped(|ui| {
                ui.strong("Filter:");
                let is_wildcard_target =
                    self.state.filter_query.trim() == crate::state::DEFAULT_FILTER_PATH;

                let resp = ui
                    .add(
                        styled_singleline_text_edit(
                            &mut self.state.filter_query,
                            190.0,
                            Some("* or HL7 path (e.g. PID-3(*).1, MSH-9.1)"),
                        )
                        .id_salt("filter_bar_query_input"),
                    )
                    .on_hover_text(
                        "Target HL7 path: use * to search the entire message, or type a path such as PID-3(*).1, MSH-9.1, PV1-2, or OBX-5",
                    );
                if self.state.focus_filter_input {
                    resp.request_focus();
                    if let Some(mut text_state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                        let end =
                            egui::text::CCursor::new(self.state.filter_query.chars().count());
                        text_state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(end)));
                        text_state.store(ui.ctx(), resp.id);
                    }
                    self.state.focus_filter_input = false;
                }
                if !self.state.filter_query.is_empty()
                    && !is_wildcard_target
                    && icon_button(
                        ui,
                        IconKind::Close,
                        true,
                        None,
                        egui::vec2(CONTROL_HEIGHT, CONTROL_HEIGHT),
                        "Reset target path to * (Entire Message)",
                    )
                    .clicked()
                {
                    self.state.filter_query = crate::state::DEFAULT_FILTER_PATH.to_string();
                    self.state.filter_builder_value.clear();
                    if self.state.active_filters.is_empty() {
                        self.state.clear_filter(&self.settings);
                    } else {
                        self.state.apply_filter(&self.settings);
                    }
                }

                egui::ComboBox::from_id_salt("filter_builder_op_combo")
                    .selected_text(self.state.filter_builder_op.label())
                    .width(150.0)
                    .height(260.0)
                    .show_ui(ui, |ui| {
                        for op in FilterBuilderOp::ALL {
                            ui.selectable_value(&mut self.state.filter_builder_op, op, op.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Comparison operator for matching the target HL7 path or entire message",
                    );

                let mut value_enter = false;
                if self.state.filter_builder_op.requires_value() {
                    let value_hint = if is_wildcard_target {
                        "Search entire message..."
                    } else {
                        "Value"
                    };
                    let val_resp = ui
                        .add(
                            styled_singleline_text_edit(
                                &mut self.state.filter_builder_value,
                                160.0,
                                Some(value_hint),
                            )
                            .id_salt("filter_builder_value_input"),
                        )
                        .on_hover_text(
                            "Text, numeric value, or regex pattern to match against the target path",
                        );
                    value_enter =
                        val_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }

                if icon_toggle_button(
                    ui,
                    IconKind::Help,
                    self.state.filter_help_open,
                    true,
                    palette.accent,
                    egui::vec2(CONTROL_HEIGHT, CONTROL_HEIGHT),
                    "Filter syntax: Path + Operator + Value, or inline SEG-F[(R|*)][.C[.S]] with =, !=, ~=, =~, contains, exists, >, >=, <, <=, && / ||. Click for examples.",
                )
                .clicked()
                {
                    self.state.filter_help_open = !self.state.filter_help_open;
                }
                if toggle_pill_button(ui, "Aa", self.state.filter_case_sensitive, palette.accent)
                    .on_hover_text("Case-Sensitive Filter")
                    .clicked()
                {
                    self.state.filter_case_sensitive = !self.state.filter_case_sensitive;
                    if self.state.filtered_record_indices.is_some() {
                        self.state.apply_filter(&self.settings);
                    }
                }
                if toggle_pill_button(ui, "!v", self.state.filter_invert_match, palette.accent)
                    .on_hover_text("Invert Match (-v): keep messages that do NOT match")
                    .clicked()
                {
                    self.state.filter_invert_match = !self.state.filter_invert_match;
                    if self.state.filtered_record_indices.is_some() {
                        self.state.apply_filter(&self.settings);
                    }
                }

                let (enter_key, shift_held) =
                    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift));
                let enter_pressed = (resp.lost_focus() && enter_key) || value_enter;
                let can_add = self.state.resolve_unified_filter_input().is_some();
                if icon_labeled_button(ui, IconKind::Add, "Add", can_add, Some(palette.accent_green))
                    .on_hover_text("Add rule to active filters (Shift+Enter)")
                    .clicked()
                    || (enter_pressed && shift_held)
                {
                    self.state.add_filter_from_input(&self.settings);
                }
                if icon_labeled_button(ui, IconKind::Check, "Apply", true, Some(palette.accent))
                    .on_hover_text("Apply filter (Enter)")
                    .clicked()
                    || (enter_pressed && !shift_held)
                {
                    self.state.apply_filter(&self.settings);
                }
                if icon_labeled_button(ui, IconKind::Close, "Clear", true, None)
                    .on_hover_text("Clear all active filters")
                    .clicked()
                {
                    self.state.clear_filter(&self.settings);
                }
                if let Some(ref visible) = self.state.filtered_record_indices {
                    let match_count = visible.len();
                    let total_count = self.state.doc.len();
                    ui.colored_label(
                        palette.accent,
                        format!("Showing {match_count} of {total_count} messages"),
                    )
                    .on_hover_text(
                        "Filtering is non-destructive; all messages remain loaded in memory until pruned.",
                    );
                    if icon_labeled_button(
                        ui,
                        IconKind::Save,
                        "Export Filtered...",
                        match_count > 0,
                        None,
                    )
                    .on_hover_text(
                        "Save only the matching filtered messages to a new file without modifying this session",
                    )
                    .clicked()
                    {
                        export_filtered_clicked = true;
                    }
                    let can_prune = match_count < total_count;
                    if icon_labeled_button(
                        ui,
                        IconKind::Delete,
                        "Keep Only Filtered",
                        can_prune,
                        Some(palette.accent_red),
                    )
                    .on_hover_text(
                        "Remove unmatched messages from memory to keep only the filtered subset",
                    )
                    .clicked()
                    {
                        keep_only_clicked = true;
                    }
                }
                if let Some(ref err) = self.state.filter_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
            });

            if !self.state.active_filters.is_empty() {
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.weak("Match:");
                    if toggle_pill_button(
                        ui,
                        "AND",
                        self.state.filter_combine_mode == FilterCombineMode::And,
                        palette.accent,
                    )
                    .on_hover_text("Match messages satisfying ALL active filters")
                    .clicked()
                    {
                        new_combine_mode = Some(FilterCombineMode::And);
                    }
                    if toggle_pill_button(
                        ui,
                        "OR",
                        self.state.filter_combine_mode == FilterCombineMode::Or,
                        palette.accent,
                    )
                    .on_hover_text("Match messages satisfying ANY active filter")
                    .clicked()
                    {
                        new_combine_mode = Some(FilterCombineMode::Or);
                    }
                    ui.separator();
                    for (idx, rule) in self.state.active_filters.iter().enumerate() {
                        let mut flags = String::new();
                        if rule.case_sensitive {
                            flags.push_str(" Aa");
                        }
                        if rule.use_regex {
                            flags.push_str(" .*");
                        }
                        if rule.invert_match {
                            flags.push_str(" !v");
                        }
                        egui::Frame::NONE
                            .fill(palette.elevated_bg)
                            .stroke(egui::Stroke::new(1.0_f32, palette.accent))
                            .corner_radius(egui::CornerRadius::same(CONTROL_CORNER_RADIUS))
                            .inner_margin(egui::Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.set_min_height(CONTROL_HEIGHT - 8.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(
                                        egui::RichText::new(format!("{}{}", rule.query, flags))
                                            .monospace()
                                            .small(),
                                    );
                                    if icon_button(
                                        ui,
                                        IconKind::Close,
                                        true,
                                        None,
                                        egui::vec2(20.0, 20.0),
                                        "Remove this filter rule",
                                    )
                                    .clicked()
                                    {
                                        remove_rule_idx = Some(idx);
                                    }
                                });
                            });
                    }
                });
            }

            if self.state.filter_help_open {
                ui.add_space(2.0);
                egui::Frame::NONE
                    .fill(palette.elevated_bg)
                    .stroke(egui::Stroke::new(1.0_f32, palette.divider))
                    .corner_radius(egui::CornerRadius::same(CONTROL_CORNER_RADIUS))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("Filter Syntax:");
                            ui.weak(
                                "SEG-F[(R|*)][.C[.S]]  |  Wildcard repetitions: PID-3(*).1  |  Operators: =  !=  ~=  =~  contains  exists  >  >=  <  <=",
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("Examples (click to insert):");
                            for ex in [
                                "MSH-9.1=ADT",
                                "PID-3(*).1=123",
                                "PID-3(*).1 ~= 23",
                                "PID-5.1 =~ ^SM.*",
                                "MSH-7 >= 20260101",
                                "OBX-5 > 200",
                                "PID-3(*).1 exists",
                                "ROL-4 exists",
                            ] {
                                if ui
                                    .add(
                                        egui::Button::new(egui::RichText::new(ex).monospace())
                                            .min_size(egui::vec2(0.0, CONTROL_HEIGHT)),
                                    )
                                    .on_hover_text(format!("Apply example filter: {ex}"))
                                    .clicked()
                                {
                                    self.state.filter_query = ex.to_string();
                                    self.state.filter_builder_value.clear();
                                    self.state.apply_filter(&self.settings);
                                }
                            }
                        });
                    });
            }
        });

        if let Some(mode) = new_combine_mode {
            self.state.set_filter_combine_mode(mode, &self.settings);
        }
        if let Some(idx) = remove_rule_idx {
            self.state.remove_filter_rule(idx, &self.settings);
        }
        if export_filtered_clicked {
            self.prompt_export_filtered();
        }
        if keep_only_clicked {
            if let Err(e) = self.state.request_keep_only_filtered(&self.settings) {
                self.state.modal_alert = Some(e.to_string());
            }
        }
    }

    /// Render the compact VS Code-style floating Find & Replace overlay anchored to the
    /// top-right of the main window (`Ctrl+F` collapsed, `Ctrl+H` expanded).
    fn show_floating_find_replace_widget(&mut self, ctx: &Context) {
        let palette = ThemePalette::for_visuals(&ctx.style().visuals);
        let replace_expanded = self.state.find_replace_tab == FindReplaceTab::Replace;

        // Tight, grid-aligned VS Code geometry:
        // Left chevron (20) + gap (4) + Input box (240) + gap (4) + Right actions (178) = 446 px
        const CHEV_W: f32 = 20.0;
        const INPUT_W: f32 = 240.0;
        const INPUT_H: f32 = 26.0;
        const ROW_GAP: f32 = 4.0;
        const COUNTER_W: f32 = 72.0;
        const BTN_SIZE: f32 = 24.0;
        const BTN_GAP: f32 = 2.0;
        const INLINE_TOGGLE: f32 = 20.0;
        // Right-side total: 72 (counter) + 4 + 4*24 (btns) + 3*2 (gaps) = 178 px
        // Row 2 right-side: 24 (Replace) + 2 + 24 (Replace All) + 4 + 124 (Scope) = 178 px
        const SCOPE_COMBO_W: f32 = 124.0;
        const INNER_W: f32 =
            CHEV_W + 4.0 + INPUT_W + 4.0 + COUNTER_W + 4.0 + 4.0 * BTN_SIZE + 3.0 * BTN_GAP;

        let mut clicked_breakdown_path: Option<String> = None;

        egui::Area::new(egui::Id::new("vscode_floating_find_replace"))
            .order(egui::Order::Foreground)
            .interactable(true)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 28.0))
            .show(ctx, |ui| {
                let frame_bg = palette.elevated_bg;
                let input_bg = if palette.is_dark {
                    egui::Color32::from_rgb(0x16, 0x1B, 0x22)
                } else {
                    egui::Color32::WHITE
                };
                let pill_active_bg = if palette.is_dark {
                    egui::Color32::from_rgb(0x1F, 0x4E, 0x79)
                } else {
                    egui::Color32::from_rgb(0xDB, 0xEA, 0xFE)
                };
                let pill_active_fg = if palette.is_dark {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(0x1D, 0x4E, 0xD8)
                };
                let no_results_red = if palette.is_dark {
                    egui::Color32::from_rgb(0xF8, 0x71, 0x71)
                } else {
                    egui::Color32::from_rgb(0xC5, 0x30, 0x30)
                };

                egui::Frame::NONE
                    .fill(frame_bg)
                    .stroke(egui::Stroke::new(1.0_f32, palette.divider))
                    .corner_radius(egui::CornerRadius::same(6))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 12,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(if palette.is_dark { 110 } else { 38 }),
                    })
                    .inner_margin(egui::Margin::symmetric(6, 5))
                    .show(ui, |ui| {
                        ui.set_width(INNER_W);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, ROW_GAP);

                            // Left full-height vertical chevron toggle (`>` collapsed, `v` expanded),
                            // dead-center both horizontally and vertically across Find (+ Replace) rows.
                            let chev_h = if replace_expanded {
                                INPUT_H * 2.0 + ROW_GAP
                            } else {
                                INPUT_H
                            };
                            let (chev_rect, chev_resp) = ui.allocate_exact_size(
                                egui::vec2(CHEV_W, chev_h),
                                egui::Sense::click(),
                            );
                            let chev_resp = chev_resp.on_hover_text("Toggle Replace (Ctrl+H)");
                            if ui.is_rect_visible(chev_rect) {
                                if chev_resp.hovered() {
                                    ui.painter().rect_filled(
                                        chev_rect,
                                        egui::CornerRadius::same(3),
                                        ui.visuals().widgets.hovered.bg_fill,
                                    );
                                }
                                let fg = if chev_resp.hovered() {
                                    palette.primary_text
                                } else {
                                    palette.muted_text
                                };
                                paint_icon(
                                    ui.painter(),
                                    if replace_expanded {
                                        IconKind::ChevronDown
                                    } else {
                                        IconKind::ChevronRight
                                    },
                                    chev_rect.center(),
                                    fg,
                                    frame_bg,
                                    12.0,
                                );
                            }
                            if chev_resp.clicked() {
                                self.state.find_replace_tab = if replace_expanded {
                                    FindReplaceTab::Find
                                } else {
                                    FindReplaceTab::Replace
                                };
                            }

                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(4.0, ROW_GAP);

                                // Helper for uniform 24×24 square action icon buttons
                                let square_icon_btn =
                                    |ui: &mut egui::Ui,
                                     icon: IconKind,
                                     active: bool,
                                     tip: &str|
                                     -> egui::Response {
                                        let (fill, fg, stroke_col) = if active {
                                            (pill_active_bg, pill_active_fg, palette.accent)
                                        } else {
                                            (frame_bg, palette.primary_text, palette.divider)
                                        };
                                        let resp = ui
                                            .add(
                                                egui::Button::new("")
                                                    .small()
                                                    .fill(fill)
                                                    .stroke(egui::Stroke::new(1.0_f32, stroke_col))
                                                    .corner_radius(egui::CornerRadius::same(4))
                                                    .min_size(egui::vec2(BTN_SIZE, BTN_SIZE)),
                                            )
                                            .on_hover_text(tip);
                                        if ui.is_rect_visible(resp.rect) {
                                            paint_icon(
                                                ui.painter(),
                                                icon,
                                                resp.rect.center(),
                                                fg,
                                                fill,
                                                ICON_SIZE_INLINE,
                                            );
                                        }
                                        resp
                                    };

                                // Row 1: Find input (240×26) with 20×20 inline Aa / ab / .* toggles + 72px counter + 4×(24×24) buttons
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    let hint_color = ui.visuals().weak_text_color();

                                    let mut find_changed = false;
                                    let mut find_enter = false;
                                    let mut find_shift_enter = false;
                                    let mut find_id_to_refocus: Option<egui::Id> = None;

                                    egui::Frame::NONE
                                        .fill(input_bg)
                                        .stroke(egui::Stroke::new(1.0_f32, palette.divider))
                                        .corner_radius(egui::CornerRadius::same(4))
                                        .inner_margin(egui::Margin::symmetric(5, 2))
                                        .show(ui, |ui| {
                                            ui.set_width(INPUT_W - 10.0);
                                            ui.set_height(INPUT_H - 4.0);
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 2.0;
                                                let text_w = (INPUT_W
                                                    - 10.0
                                                    - 3.0 * INLINE_TOGGLE
                                                    - 3.0 * 2.0)
                                                    .max(80.0);
                                                let find_resp = ui.add_sized(
                                                    egui::vec2(text_w, INPUT_H - 6.0),
                                                    egui::TextEdit::singleline(
                                                        &mut self.state.search_query,
                                                    )
                                                    .id_salt("floating_find_query_input")
                                                    .frame(false)
                                                    .hint_text(
                                                        egui::RichText::new("Find")
                                                            .color(hint_color),
                                                    ),
                                                );
                                                if self.state.focus_search_input {
                                                    find_resp.request_focus();
                                                    self.state.focus_search_input = false;
                                                }
                                                if find_resp.changed() {
                                                    find_changed = true;
                                                }
                                                let enter_key = ui.input(|i| {
                                                    i.key_pressed(egui::Key::Enter)
                                                });
                                                if enter_key
                                                    && (find_resp.lost_focus()
                                                        || find_resp.has_focus())
                                                {
                                                    find_enter = true;
                                                    find_shift_enter =
                                                        ui.input(|i| i.modifiers.shift);
                                                    find_id_to_refocus = Some(find_resp.id);
                                                }

                                                // Uniform 20×20 inline toggle buttons (`Aa`, `ab` w/ underline, `.*`)
                                                let inline_toggle =
                                                    |ui: &mut egui::Ui,
                                                     active: bool,
                                                     label: &str,
                                                     underline: bool,
                                                     tip: &str|
                                                     -> bool {
                                                        let (fill, fg, stroke) = if active {
                                                            (
                                                                pill_active_bg,
                                                                pill_active_fg,
                                                                egui::Stroke::new(
                                                                    1.0_f32,
                                                                    palette.accent,
                                                                ),
                                                            )
                                                        } else {
                                                            (
                                                                egui::Color32::TRANSPARENT,
                                                                palette.muted_text,
                                                                egui::Stroke::NONE,
                                                            )
                                                        };
                                                        let (rect, resp) = ui.allocate_exact_size(
                                                            egui::vec2(INLINE_TOGGLE, INLINE_TOGGLE),
                                                            egui::Sense::click(),
                                                        );
                                                        let resp = resp.on_hover_text(tip);
                                                        if ui.is_rect_visible(rect) {
                                                            let bg = if !active && resp.hovered() {
                                                                ui.visuals().widgets.hovered.bg_fill
                                                            } else {
                                                                fill
                                                            };
                                                            ui.painter().rect_filled(
                                                                rect,
                                                                egui::CornerRadius::same(3),
                                                                bg,
                                                            );
                                                            if active {
                                                                ui.painter().rect_stroke(
                                                                    rect,
                                                                    egui::CornerRadius::same(3),
                                                                    stroke,
                                                                    egui::StrokeKind::Inside,
                                                                );
                                                            }
                                                            let font = egui::FontId::monospace(10.5);
                                                            let galley = ui.painter().layout_no_wrap(
                                                                label.to_string(),
                                                                font,
                                                                fg,
                                                            );
                                                            let dy = if underline { -1.5 } else { 0.0 };
                                                            let pos = egui::pos2(
                                                                rect.center().x
                                                                    - galley.size().x * 0.5,
                                                                rect.center().y
                                                                    - galley.size().y * 0.5
                                                                    + dy,
                                                            );
                                                            ui.painter().galley(pos, galley, fg);
                                                            if underline {
                                                                let uy = (rect.bottom() - 3.0).round();
                                                                ui.painter().line_segment(
                                                                    [
                                                                        egui::pos2(
                                                                            rect.left() + 4.0,
                                                                            uy,
                                                                        ),
                                                                        egui::pos2(
                                                                            rect.right() - 4.0,
                                                                            uy,
                                                                        ),
                                                                    ],
                                                                    egui::Stroke::new(1.2_f32, fg),
                                                                );
                                                            }
                                                        }
                                                        resp.clicked()
                                                    };

                                                if inline_toggle(
                                                    ui,
                                                    self.state.search_case_sensitive,
                                                    "Aa",
                                                    false,
                                                    "Match Case",
                                                ) {
                                                    self.state.search_case_sensitive =
                                                        !self.state.search_case_sensitive;
                                                    find_changed = true;
                                                }
                                                let whole_tip = if self
                                                    .state
                                                    .search_field_input
                                                    .trim()
                                                    .is_empty()
                                                {
                                                    "Match Whole Word"
                                                } else {
                                                    "Match Whole Value"
                                                };
                                                if inline_toggle(
                                                    ui,
                                                    self.state.search_match_whole,
                                                    "ab",
                                                    true,
                                                    whole_tip,
                                                ) {
                                                    self.state.search_match_whole =
                                                        !self.state.search_match_whole;
                                                    find_changed = true;
                                                }
                                                if inline_toggle(
                                                    ui,
                                                    self.state.search_use_regex,
                                                    ".*",
                                                    false,
                                                    "Use Regular Expression (supports $1, $2)",
                                                ) {
                                                    self.state.search_use_regex =
                                                        !self.state.search_use_regex;
                                                    find_changed = true;
                                                }
                                            });
                                        });

                                    if find_changed {
                                        self.state.last_searched_query.clear();
                                        if self.state.search_query.trim().is_empty()
                                            && self.state.search_field_input.trim().is_empty()
                                        {
                                            self.state.search_matches.clear();
                                            self.state.search_field_breakdown.clear();
                                            self.state.search_match_cursor = None;
                                            self.state.search_status.clear();
                                        } else if let Err(err) = self.state.count_find_matches() {
                                            self.state.search_status = err;
                                        }
                                    }

                                    // Also allow Enter / Shift+Enter when widget is open & no text edit has focus
                                    if !find_enter
                                        && !ctx.wants_keyboard_input()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        find_enter = true;
                                        find_shift_enter = ui.input(|i| i.modifiers.shift);
                                    }

                                    // Right-aligned fixed-width match counter (`72.0 px`) so buttons never jump
                                    let has_query = !self.state.search_query.trim().is_empty()
                                        || !self.state.search_field_input.trim().is_empty();
                                    let total_matches = self.state.search_matches.len();
                                    let (counter_text, counter_color) = if !has_query {
                                        (String::new(), palette.muted_text)
                                    } else if total_matches == 0 {
                                        ("No results".to_string(), no_results_red)
                                    } else if let Some(cur) = self.state.search_match_cursor {
                                        (
                                            format!("{} of {total_matches}", cur + 1),
                                            palette.muted_text,
                                        )
                                    } else {
                                        (format!("{total_matches} found"), palette.muted_text)
                                    };
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(COUNTER_W, BTN_SIZE),
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(counter_text)
                                                        .small()
                                                        .color(counter_color),
                                                )
                                                .truncate(),
                                            );
                                        },
                                    );

                                    // Uniform 24×24 square icon buttons with tight 2px spacing
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = BTN_GAP;

                                        let prev_resp = square_icon_btn(
                                            ui,
                                            IconKind::MoveUp,
                                            false,
                                            "Previous Match (Shift+F3 / Shift+Enter)",
                                        );
                                        if prev_resp.clicked() || (find_enter && find_shift_enter) {
                                            let _ = self.state.jump_search(false, &self.settings);
                                            if let Some(id) = find_id_to_refocus {
                                                ctx.memory_mut(|m| m.request_focus(id));
                                            }
                                        }

                                        let next_resp = square_icon_btn(
                                            ui,
                                            IconKind::MoveDown,
                                            false,
                                            "Next Match (F3 / Enter)",
                                        );
                                        if next_resp.clicked() || (find_enter && !find_shift_enter)
                                        {
                                            let _ = self.state.jump_search(true, &self.settings);
                                            if let Some(id) = find_id_to_refocus {
                                                ctx.memory_mut(|m| m.request_focus(id));
                                            }
                                        }

                                        let path_drawer_active = self.state.search_breakdown_open
                                            || !self.state.search_field_input.trim().is_empty();
                                        let drawer_resp = square_icon_btn(
                                            ui,
                                            IconKind::MenuLines,
                                            path_drawer_active,
                                            "Toggle HL7 Target Path (e.g. PID-3.4) & Hit Breakdown",
                                        );
                                        if drawer_resp.clicked() {
                                            self.state.search_breakdown_open =
                                                !self.state.search_breakdown_open;
                                        }

                                        let close_resp = square_icon_btn(
                                            ui,
                                            IconKind::Close,
                                            false,
                                            "Close (Esc)",
                                        );
                                        if close_resp.clicked() {
                                            self.state.close_find_replace_panel();
                                        }
                                    });
                                });

                                // Row 2 (Replace — identical 240×26 input box + aligned 24×24 Replace / Replace All + Scope)
                                if replace_expanded {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        let hint_color = ui.visuals().weak_text_color();
                                        let mut rep_enter = false;
                                        let mut rep_ctrl_enter = false;
                                        let mut rep_id_to_refocus: Option<egui::Id> = None;

                                        egui::Frame::NONE
                                            .fill(input_bg)
                                            .stroke(egui::Stroke::new(1.0_f32, palette.divider))
                                            .corner_radius(egui::CornerRadius::same(4))
                                            .inner_margin(egui::Margin::symmetric(5, 2))
                                            .show(ui, |ui| {
                                                ui.set_width(INPUT_W - 10.0);
                                                ui.set_height(INPUT_H - 4.0);
                                                let rep_resp = ui.add_sized(
                                                    egui::vec2(INPUT_W - 10.0, INPUT_H - 6.0),
                                                    egui::TextEdit::singleline(
                                                        &mut self.state.replace_with_input,
                                                    )
                                                    .id_salt("floating_replace_input")
                                                    .frame(false)
                                                    .hint_text(
                                                        egui::RichText::new("Replace ($1, $2)")
                                                            .color(hint_color),
                                                    ),
                                                );
                                                let enter_key = ui.input(|i| {
                                                    i.key_pressed(egui::Key::Enter)
                                                });
                                                if enter_key
                                                    && (rep_resp.lost_focus()
                                                        || rep_resp.has_focus())
                                                {
                                                    let mods = ui.input(|i| i.modifiers);
                                                    if mods.ctrl || mods.command {
                                                        rep_ctrl_enter = true;
                                                    } else {
                                                        rep_enter = true;
                                                    }
                                                    rep_id_to_refocus = Some(rep_resp.id);
                                                }
                                            });

                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = BTN_GAP;

                                            if square_icon_btn(
                                                ui,
                                                IconKind::ReplaceOne,
                                                false,
                                                "Replace (Enter)",
                                            )
                                            .clicked()
                                                || rep_enter
                                            {
                                                if let Err(err) = self
                                                    .state
                                                    .replace_current_and_advance(&self.settings)
                                                {
                                                    self.state.replace_status_banner = err;
                                                }
                                                if let Some(id) = rep_id_to_refocus {
                                                    ctx.memory_mut(|m| m.request_focus(id));
                                                }
                                            }
                                            if square_icon_btn(
                                                ui,
                                                IconKind::ReplaceAll,
                                                false,
                                                "Replace All (Ctrl+Alt+Enter)",
                                            )
                                            .clicked()
                                                || rep_ctrl_enter
                                            {
                                                if let Err(err) =
                                                    self.state.replace_all_in_scope(&self.settings)
                                                {
                                                    self.state.replace_status_banner = err;
                                                }
                                                if let Some(id) = rep_id_to_refocus {
                                                    ctx.memory_mut(|m| m.request_focus(id));
                                                }
                                            }
                                        });

                                        let scope_label = match self.state.find_replace_scope {
                                            FindReplaceScope::CurrentRecord => "Selected",
                                            FindReplaceScope::FilteredRecords => "Filtered",
                                            FindReplaceScope::AllRecords => "All",
                                        };
                                        egui::ComboBox::from_id_salt("floating_replace_scope_combo")
                                            .selected_text(scope_label)
                                            .width(SCOPE_COMBO_W)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut self.state.find_replace_scope,
                                                    FindReplaceScope::AllRecords,
                                                    "All",
                                                );
                                                ui.selectable_value(
                                                    &mut self.state.find_replace_scope,
                                                    FindReplaceScope::FilteredRecords,
                                                    "Filtered",
                                                );
                                                ui.selectable_value(
                                                    &mut self.state.find_replace_scope,
                                                    FindReplaceScope::CurrentRecord,
                                                    "Selected",
                                                );
                                            });
                                    });

                                    if !self.state.replace_status_banner.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&self.state.replace_status_banner)
                                                .small()
                                                .color(palette.accent),
                                        );
                                    }
                                }

                                // Expandable HL7 Target Path & Hit Breakdown drawer
                                if self.state.search_breakdown_open {
                                    ui.separator();
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        ui.small("Target Path:");
                                        let hint_color = ui.visuals().weak_text_color();
                                        let field_resp = ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.state.search_field_input,
                                            )
                                            .id_salt("floating_target_path_input")
                                            .hint_text(
                                                egui::RichText::new("e.g. PID-3.4")
                                                    .color(hint_color),
                                            )
                                            .desired_width(115.0),
                                        );
                                        if field_resp.changed() {
                                            self.state.last_searched_query.clear();
                                            let _ = self.state.count_find_matches();
                                        }
                                        if !self.state.search_field_input.is_empty() {
                                            let trimmed = self.state.search_field_input.trim();
                                            match trimmed.parse::<hl7_core::FieldAddress>() {
                                                Ok(_) => {
                                                    ui.colored_label(
                                                        egui::Color32::from_rgb(74, 222, 128),
                                                        "OK",
                                                    )
                                                    .on_hover_text(format!(
                                                        "Targeting exact HL7 path '{trimmed}'"
                                                    ));
                                                }
                                                Err(e) => {
                                                    ui.colored_label(
                                                        egui::Color32::LIGHT_RED,
                                                        "!",
                                                    )
                                                    .on_hover_text(format!(
                                                        "Invalid HL7 field path: {e}"
                                                    ));
                                                }
                                            }
                                            if ui
                                                .small_button("Clear")
                                                .on_hover_text("Clear Target Field Path")
                                                .clicked()
                                            {
                                                self.state.search_field_input.clear();
                                                self.state.last_searched_query.clear();
                                                let _ = self.state.count_find_matches();
                                            }
                                        }
                                        for chip_path in ["PID-3.4", "PID-5.1", "OBX-5", "MSH-4"] {
                                            if ui
                                                .small_button(
                                                    egui::RichText::new(chip_path)
                                                        .monospace()
                                                        .small(),
                                                )
                                                .clicked()
                                            {
                                                self.state.search_field_input =
                                                    chip_path.to_string();
                                                self.state.last_searched_query.clear();
                                                let _ = self.state.count_find_matches();
                                            }
                                        }
                                    });

                                    if !self.state.search_field_breakdown.is_empty() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.small("Hits by field:");
                                            for item in &self.state.search_field_breakdown {
                                                let chip =
                                                    format!("{}: {}", item.field_path, item.count);
                                                if ui
                                                    .small_button(
                                                        egui::RichText::new(chip)
                                                            .monospace()
                                                            .small(),
                                                    )
                                                    .on_hover_text(format!(
                                                        "Restrict Find/Replace to {}",
                                                        item.field_path
                                                    ))
                                                    .clicked()
                                                {
                                                    clicked_breakdown_path =
                                                        Some(item.field_path.clone());
                                                }
                                            }
                                        });
                                    }
                                }
                            });
                        });
                    });
            });

        if let Some(path) = clicked_breakdown_path {
            let _ = self
                .state
                .restrict_search_to_field_path(&path, &self.settings);
        }
    }

    fn complete_pending_unsaved_action(&mut self, action: PendingUnsavedAction, ctx: &Context) {
        match action {
            PendingUnsavedAction::OpenFiles(paths) => {
                self.state.request_open_files_with_guardrail(
                    paths,
                    RecoveryMode::Strict,
                    None,
                    false,
                    &self.settings,
                    ctx.clone(),
                    None,
                );
            }
            PendingUnsavedAction::PasteReplace => {
                let _ = self.state.paste_from_clipboard_result(
                    read_clipboard_text(ctx),
                    true,
                    &self.settings,
                );
            }
            PendingUnsavedAction::ExitApp => {
                self.allow_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Render one full application UI frame into `ctx` (also used by headless UI smoke tests).
    pub fn render_frame(&mut self, ctx: &Context) {
        ctx.set_zoom_factor(
            self.settings
                .ui_zoom
                .clamp(crate::config::MIN_UI_ZOOM, crate::config::MAX_UI_ZOOM),
        );
        apply_theme(ctx, self.settings.dark_mode);
        if self.last_memory_refresh.elapsed() >= std::time::Duration::from_secs(2) {
            self.cached_memory_budget = MemoryBudget::from_system();
            self.last_memory_refresh = std::time::Instant::now();
        }
        self.state.poll_background_load(&self.settings);
        self.state.poll_network(&self.settings);
        self.handle_global_shortcuts(ctx);

        // OS Drag-and-Drop of one or more HL7 files (D-28).
        let dropped_paths: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped_paths.is_empty() {
            if self.state.undo_stack.is_dirty() {
                self.state.pending_unsaved_action =
                    Some(PendingUnsavedAction::OpenFiles(dropped_paths));
            } else {
                self.state.request_open_files_with_guardrail(
                    dropped_paths,
                    RecoveryMode::Strict,
                    None,
                    false,
                    &self.settings,
                    ctx.clone(),
                    None,
                );
            }
        }

        // Update native window title (FO-3, FS-8).
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));

        // FS-8: Intercept window close when document has unsaved changes.
        if ctx.input(|i| i.viewport().close_requested())
            && self.state.undo_stack.is_dirty()
            && !self.allow_exit
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.state.pending_unsaved_action = Some(PendingUnsavedAction::ExitApp);
        }

        // Top menu bar + icon toolbar + filter/search bars + optional loading progress bar (FO-13, D-26).
        egui::TopBottomPanel::top("top_menu_panel").show(ctx, |ui| {
            self.show_menu_bar(ui, ctx);
            ui.separator();
            self.show_icon_toolbar(ui, ctx);

            if self.state.filter_bar_open {
                ui.separator();
                self.show_filter_bar(ui);
            }

            if let Some(ref job) = self.state.active_load {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spinner();
                    if let Some(prog) = job.progress {
                        let frac = if prog.total_files > 0 {
                            prog.files_done as f32 / prog.total_files as f32
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .text(format!(
                                    "Files: {}/{} ({} KiB)",
                                    prog.files_done,
                                    prog.total_files,
                                    prog.bytes_read / 1024
                                ))
                                .desired_width(240.0),
                        );
                    } else {
                        ui.label("Loading...");
                    }
                    if ui.button("Cancel").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
        });

        // Bottom status bar with clickable zoom percentage badge (D-28) and live RAM readout (Phase 1).
        egui::TopBottomPanel::bottom("bottom_status_bar").show(ctx, |ui| {
            let status_palette = ThemePalette::for_visuals(ui.visuals());
            ui.horizontal(|ui| {
                ui.label(&self.state.status_message);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let zoom_pct = format!("{}%", self.settings.zoom_percent());
                    if ui
                        .small_button(egui::RichText::new(zoom_pct).monospace())
                        .on_hover_text("Click to reset UI zoom to 100% (Ctrl+0)")
                        .clicked()
                    {
                        self.settings.reset_zoom();
                        self.settings.save_default();
                    }
                    ui.separator();
                    let mem = self.cached_memory_budget;
                    let mem_label = format!(
                        "RAM: {} / {} budget",
                        format_memory_bytes(mem.process_rss_bytes),
                        format_memory_bytes(mem.safe_load_budget_bytes),
                    );
                    let mem_color = if mem.is_rss_high() {
                        if status_palette.is_dark {
                            egui::Color32::from_rgb(0xF5, 0x9E, 0x0B)
                        } else {
                            egui::Color32::from_rgb(0xB4, 0x53, 0x09)
                        }
                    } else {
                        status_palette.muted_text
                    };
                    ui.label(
                        egui::RichText::new(mem_label)
                            .monospace()
                            .small()
                            .color(mem_color),
                    )
                    .on_hover_text(format!(
                        "Process RSS: {}\nSafe Load Budget (30% of available RAM): {}\nAvailable System RAM: {}",
                        format_memory_bytes(mem.process_rss_bytes),
                        format_memory_bytes(mem.safe_load_budget_bytes),
                        format_memory_bytes(mem.available_ram_bytes),
                    ));
                    ui.separator();
                    ui.weak(self.state.source.display_label());
                });
            });
        });

        // Integrated Docked TCP/IP Section with draggable splitter (replacing floating windows, D-26).
        if self.state.network.docked_open {
            egui::TopBottomPanel::bottom("docked_tcpip_panel")
                .resizable(true)
                .default_height(260.0)
                .height_range(140.0..=550.0)
                .show(ctx, |ui| {
                    net_panels::show_docked_tcpip_panel(
                        ui,
                        ctx,
                        &mut self.state.network,
                        &self.state.doc.records,
                        &self.state.selected_records,
                    );
                });
        }

        // Left pane: Virtualized Record list with draggable, persisted splitter (§5.1).
        let mut rec_action = RecordListAction::None;
        let rec_resp = egui::SidePanel::left("left_record_list_pane")
            .resizable(true)
            .default_width(self.settings.record_pane_width)
            .width_range(220.0..=900.0)
            .show(ctx, |ui| {
                rec_action = record_list::show_record_list(ui, &mut self.state, &self.settings);
            });
        let rw = rec_resp.response.rect.width();
        if (rw - self.settings.record_pane_width).abs() > 1.0 {
            self.settings.record_pane_width = rw;
        }
        match rec_action {
            RecordListAction::None => {}
            RecordListAction::SaveSelectedRecords => {
                let _ = self.prompt_save_file(true);
            }
            RecordListAction::PasteFromClipboard => {
                let _ = self.state.paste_from_clipboard_result(
                    read_clipboard_text(ctx),
                    false,
                    &self.settings,
                );
            }
            RecordListAction::CompareMessages(a, b) => {
                self.state.open_compare_pair(a, b);
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("compare_hl7_messages_window"),
                    egui::ViewportCommand::Focus,
                );
            }
            RecordListAction::CompareSelected => {
                self.state.open_compare_selected_messages();
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("compare_hl7_messages_window"),
                    egui::ViewportCommand::Focus,
                );
            }
        }

        // Central Unified Record Detail Area (Dual Dark/Light Raw Editor + Expandable Segment Accordion, D-26).
        let mut detail_action = DetailPaneAction::None;
        egui::CentralPanel::default().show(ctx, |ui| {
            detail_action =
                detail_pane::show_record_detail_area(ui, &mut self.state, &mut self.settings);
        });
        if self.state.search_bar_open {
            self.show_floating_find_replace_widget(ctx);
        }
        match detail_action {
            DetailPaneAction::None => {}
            DetailPaneAction::OpenFiles => self.prompt_open_files(ctx),
            DetailPaneAction::PasteFromClipboard => {
                let _ = self.state.paste_from_clipboard_result(
                    read_clipboard_text(ctx),
                    false,
                    &self.settings,
                );
            }
            DetailPaneAction::StartListener => {
                self.state
                    .network
                    .open_dock_tab(TcpIpDockTab::ServerListener);
            }
        }

        self.show_modals(ctx);
        compare_modal::show_unique_values_modal(ctx, &mut self.state, &self.settings);
        compare_modal::show_compare_messages_modal(ctx, &mut self.state, read_clipboard_text);
        dialogs::show_editing_dialogs(ctx, &mut self.state, &self.settings);
        reports::show_reports_and_sort_windows(ctx, &mut self.state, &mut self.settings);
        net_panels::show_network_modals(ctx, &mut self.state.network);
        scan_window::show_directory_scan_window(ctx, &mut self.state, &self.settings);

        // Automated `--screenshot <PATH>` capture lifecycle
        if let Some(ref mut cap) = self.screenshot_capture {
            if cap.requested {
                let captured = ctx.input(|i| {
                    i.events.iter().find_map(|ev| match ev {
                        egui::Event::Screenshot { image, .. } => Some(std::sync::Arc::clone(image)),
                        _ => None,
                    })
                });
                if let Some(image) = captured {
                    if let Err(e) = save_color_image_png(&cap.output_path, &image) {
                        tracing::error!("Failed to save screenshot: {e}");
                    }
                    self.allow_exit = true;
                    self.screenshot_capture = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint();
                }
            } else {
                cap.frames_rendered = cap.frames_rendered.saturating_add(1);
                if cap.frames_rendered >= 3 {
                    cap.requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
                ctx.request_repaint();
            }
        }
    }
}

fn save_color_image_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir_all '{}': {e}", parent.display()))?;
        }
    }
    let file =
        std::fs::File::create(path).map_err(|e| format!("create '{}': {e}", path.display()))?;
    let w = std::io::BufWriter::new(file);
    let width =
        u32::try_from(image.width()).map_err(|_| "screenshot width overflow".to_string())?;
    let height =
        u32::try_from(image.height()).map_err(|_| "screenshot height overflow".to_string())?;
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png write_header: {e}"))?;
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for px in &image.pixels {
        rgba.extend_from_slice(&px.to_array());
    }
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("png write_image_data: {e}"))?;
    Ok(())
}

impl eframe::App for PipeWrenchApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.render_frame(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.save_default();
    }
}

fn apply_theme(ctx: &Context, dark_mode: Option<bool>) {
    theme::apply_design_system(ctx, dark_mode);
}

fn read_paste_event_text(ctx: &Context) -> Option<String> {
    ctx.input(|i| {
        i.events.iter().find_map(|ev| match ev {
            egui::Event::Paste(s) => Some(s.clone()),
            _ => None,
        })
    })
}

fn read_clipboard_text(ctx: &Context) -> Result<String, &'static str> {
    if let Some(ev_text) = read_paste_event_text(ctx) {
        return Ok(ev_text);
    }
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(s) if !s.is_empty() => Ok(s),
        _ => Err("ContentNotAvailable"),
    }
}

fn open_in_external_editor_or_os(
    path: &std::path::Path,
    external_editor: Option<&std::path::Path>,
) {
    if let Some(editor) = external_editor {
        let _ = std::process::Command::new(editor).arg(path).spawn();
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_filter_bar_with_no_messages_loaded_does_not_panic() {
        let mut app = PipeWrenchApp::new();
        assert!(app.state.doc.is_empty());
        app.state.filter_bar_open = true;

        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };

        for _ in 0..3 {
            let _ = ctx.run(raw_input.clone(), |ctx| {
                apply_theme(ctx, app.settings.dark_mode);
                egui::TopBottomPanel::top("top_menu_panel").show(ctx, |ui| {
                    app.show_menu_bar(ui, ctx);
                    ui.separator();
                    app.show_icon_toolbar(ui, ctx);
                    if app.state.filter_bar_open {
                        ui.separator();
                        app.show_filter_bar(ui);
                    }
                });
                egui::SidePanel::left("left_record_list_pane").show(ctx, |ui| {
                    let _ = record_list::show_record_list(ui, &mut app.state, &app.settings);
                });
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ =
                        detail_pane::show_record_detail_area(ui, &mut app.state, &mut app.settings);
                });
            });
        }

        // Also verify rendering with stacked filter chips and help popover open on empty doc
        app.state
            .active_filters
            .push(crate::state::ActiveFilterRule {
                query: "PID-5.1=DOE".to_string(),
                case_sensitive: false,
                use_regex: false,
                invert_match: false,
            });
        app.state.filter_help_open = true;
        let _ = ctx.run(raw_input, |ctx| {
            apply_theme(ctx, app.settings.dark_mode);
            egui::TopBottomPanel::top("top_menu_panel").show(ctx, |ui| {
                app.show_filter_bar(ui);
            });
        });
    }

    #[test]
    fn about_modal_renders_and_closes_on_escape() {
        let mut app = PipeWrenchApp::new();
        app.state.about_open = true;
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            app.render_frame(ctx);
        });
        assert!(app.state.about_open);

        let esc_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            events: vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(esc_input, |ctx| {
            app.render_frame(ctx);
        });
        assert!(!app.state.about_open);
    }
}
