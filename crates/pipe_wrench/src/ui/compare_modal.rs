//! WinMerge-style side-by-side HL7 Message Compare modal and Unique Values / Field Distribution modal.

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, Context, CornerRadius, FontId, Frame, Key, Margin, RichText, ScrollArea, Stroke,
};
use egui_extras::{Column, TableBuilder};
use hl7_core::{CharDiffKind, CharDiffSpan, DiffChangeKind};

use crate::config::AppSettings;
use crate::state::{normalize_hl7_for_multiline, SessionState, UniqueValuesSortColumn};
use crate::ui::icons::{icon_labeled_button, paint_icon, IconKind};
use crate::ui::theme::ThemePalette;

/// Render the **Unique Values / Field Distribution** modal when open.
pub fn show_unique_values_modal(ctx: &Context, state: &mut SessionState, settings: &AppSettings) {
    let Some(modal_snapshot) = state.unique_values_modal.clone() else {
        return;
    };

    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        state.unique_values_modal = None;
        return;
    }

    let palette = ThemePalette::for_visuals(&ctx.style().visuals);
    let mut open = true;
    let mut close_clicked = false;
    let mut clicked_sort: Option<UniqueValuesSortColumn> = None;
    let mut clicked_filter_val: Option<(String, bool)> = None;
    let mut copy_clicked = false;

    let title = match &modal_snapshot.description {
        Some(desc) => format!("Unique Values — {} ({desc})", modal_snapshot.path),
        None => format!("Unique Values — {}", modal_snapshot.path),
    };

    let max_window_h = (ctx.screen_rect().height() - 96.0).max(260.0);
    let default_window_h = 420.0_f32.min(max_window_h);

    egui::Window::new(title)
        .id(egui::Id::new("unique_values_distribution_modal"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, default_window_h])
        .min_size([400.0, 260.0])
        .max_height(max_window_h)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} unique value(s) across {} loaded message(s). Click any row to filter.",
                        modal_snapshot.rows.len(),
                        modal_snapshot.total_messages
                    ))
                    .small()
                    .color(palette.muted_text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_labeled_button(ui, IconKind::Copy, "Copy Values Table", true, None)
                        .clicked()
                    {
                        copy_clicked = true;
                    }
                });
            });
            ui.separator();

            let val_hdr = if modal_snapshot.sort_col == UniqueValuesSortColumn::Value {
                if modal_snapshot.sort_desc {
                    "Value (desc)"
                } else {
                    "Value (asc)"
                }
            } else {
                "Value"
            };
            let cnt_hdr = if modal_snapshot.sort_col == UniqueValuesSortColumn::Count {
                if modal_snapshot.sort_desc {
                    "Count (desc)"
                } else {
                    "Count (asc)"
                }
            } else {
                "Count"
            };

            let max_table_h = (max_window_h - 110.0).max(120.0);
            let avail_h = (ui.available_height() - 40.0).clamp(120.0, max_table_h);
            TableBuilder::new(ui)
                .id_salt("unique_values_table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .max_scroll_height(avail_h)
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(200.0))
                .column(Column::initial(85.0).at_least(65.0))
                .column(Column::initial(115.0).at_least(85.0))
                .header(22.0, |mut header| {
                    header.col(|ui| {
                        if ui.button(RichText::new(val_hdr).strong()).clicked() {
                            clicked_sort = Some(UniqueValuesSortColumn::Value);
                        }
                    });
                    header.col(|ui| {
                        if ui.button(RichText::new(cnt_hdr).strong()).clicked() {
                            clicked_sort = Some(UniqueValuesSortColumn::Count);
                        }
                    });
                    header.col(|ui| {
                        ui.strong("% of Messages");
                    });
                })
                .body(|body| {
                    body.rows(22.0, modal_snapshot.rows.len(), |mut row| {
                        let item = &modal_snapshot.rows[row.index()];
                        row.col(|ui| {
                            if item.is_empty_missing {
                                ui.label(
                                    RichText::new("(Empty / Missing)")
                                        .italics()
                                        .color(palette.muted_text),
                                );
                            } else {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&item.value)
                                            .monospace()
                                            .color(palette.primary_text),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(format!("{}", item.count)).monospace());
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(format!("{:.1}%", item.percent))
                                    .monospace()
                                    .color(palette.muted_text),
                            );
                        });
                        if row
                            .response()
                            .on_hover_text("Click to filter Messages list by this value")
                            .clicked()
                        {
                            clicked_filter_val = Some((item.value.clone(), item.is_empty_missing));
                        }
                    });
                });

            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close (Esc)").clicked() {
                    close_clicked = true;
                }
            });
        });

    if !open || close_clicked {
        state.unique_values_modal = None;
        return;
    }
    if let Some(col) = clicked_sort {
        state.sort_unique_values(col);
    }
    if copy_clicked {
        let table_text = state.format_unique_values_table();
        ctx.copy_text(table_text);
        state.status_message = format!(
            "Copied Unique Values table for {} to clipboard.",
            modal_snapshot.path
        );
    }
    if let Some((val, is_empty)) = clicked_filter_val {
        let path = modal_snapshot.path.clone();
        if is_empty {
            state.open_filter_for_path(&path, "", settings);
        } else {
            state.open_filter_for_path(&path, &val, settings);
        }
        state.unique_values_modal = None;
    }
}

/// Render the WinMerge-style side-by-side **Compare HL7 Messages** view in a standalone native
/// OS window via `egui`'s multi-viewport API (`show_viewport_immediate`, with embedded fallback
/// for headless test contexts).
pub fn show_compare_messages_modal(
    ctx: &Context,
    state: &mut SessionState,
    read_clipboard: impl Fn(&Context) -> Result<String, &'static str>,
) {
    if state.compare_modal.is_none() {
        return;
    }

    let vp_id = egui::ViewportId::from_hash_of("compare_hl7_messages_window");
    let builder = egui::ViewportBuilder::default()
        .with_title("Compare HL7 Messages — pipe_wrench")
        .with_position([24.0, 24.0])
        .with_inner_size([1180.0, 720.0])
        .with_min_inner_size([720.0, 400.0]);

    ctx.show_viewport_immediate(vp_id, builder, |vp_ctx, class| {
        if vp_ctx.input(|i| i.viewport().close_requested()) {
            state.compare_modal = None;
            return;
        }
        if vp_ctx.input(|i| i.key_pressed(Key::Escape)) {
            state.compare_modal = None;
            if class != egui::ViewportClass::Embedded {
                vp_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }

        let wants_kb = vp_ctx.wants_keyboard_input();
        let had_paste_event =
            vp_ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
        let is_baseline_setup_now = state
            .compare_modal
            .as_ref()
            .is_some_and(|m| m.editing_baseline || m.left_raw.trim().is_empty());

        if !is_baseline_setup_now {
            let (nav_next, nav_prev) = vp_ctx.input(|i| {
                let f7 = i.key_pressed(Key::F7);
                let next =
                    (f7 && !i.modifiers.shift) || (!wants_kb && i.key_pressed(Key::ArrowDown));
                let prev = (f7 && i.modifiers.shift) || (!wants_kb && i.key_pressed(Key::ArrowUp));
                (next, prev)
            });
            if nav_next {
                state.compare_next_diff();
            } else if nav_prev {
                state.compare_prev_diff();
            }
        }

        let Some(modal_snap) = state.compare_modal.clone() else {
            return;
        };

        let is_baseline_setup =
            modal_snap.editing_baseline || modal_snap.left_raw.trim().is_empty();

        // Pre-collect other loaded messages for the Baseline quick-picker dropdown.
        let loaded_choices: Vec<(usize, String)> = if is_baseline_setup {
            state
                .summaries
                .iter()
                .enumerate()
                .filter(|(idx, _)| Some(*idx) != modal_snap.right_record_index)
                .take(500)
                .map(|(idx, sum)| {
                    let label = if sum.control_id.is_empty() {
                        format!("#{} {}", idx + 1, sum.message_type)
                    } else {
                        format!("#{} {} • {}", idx + 1, sum.message_type, sum.control_id)
                    };
                    (idx, label)
                })
                .collect()
        } else {
            Vec::new()
        };

        let palette = ThemePalette::for_visuals(&vp_ctx.style().visuals);
        let mut open = true;
        let mut do_prev = false;
        let mut do_next = false;
        let mut do_swap = false;
        let mut do_paste_baseline = false;
        let mut do_edit_baseline = false;
        let mut do_cancel_edit_baseline = false;
        let mut do_apply_baseline = false;
        let mut selected_loaded_baseline: Option<usize> = None;
        let mut promote_target_idx: Option<usize> = None;
        let mut remove_target_idx: Option<usize> = None;
        let mut baseline_draft = modal_snap.baseline_draft.clone();
        let mut baseline_draft_changed = false;
        let mut do_copy_summary = false;
        let mut new_opts = modal_snap.options;
        let mut clicked_diff_ordinal: Option<usize> = None;

        let is_multi_target = modal_snap.is_multi_target();
        let total_diffs = modal_snap.active_total_diff_rows();
        let active_cursor = modal_snap.active_diff_cursor;
        let scroll_to_active = modal_snap.scroll_to_active_diff;
        let reset_scroll = modal_snap.reset_scroll_to_top_left;

        let render_contents = |ui: &mut egui::Ui| {
            if is_baseline_setup {
                let avail_w = ui.available_width();
                let col_gap = 12.0_f32;
                let col_w = ((avail_w - col_gap) * 0.5).max(240.0);
                let pane_h = (ui.available_height() - 6.0).max(280.0);

                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = col_gap;

                    // Left Pane — "Provide Baseline Message"
                    Frame::NONE
                        .fill(palette.side_panel_bg)
                        .stroke(Stroke::new(1.0_f32, palette.divider))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width((col_w - 26.0).max(210.0));
                            ui.set_min_height((pane_h - 26.0).max(240.0));

                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new("Provide Baseline Message")
                                        .strong()
                                        .size(15.0)
                                        .color(palette.primary_text),
                                );
                                ui.label(
                                    RichText::new(
                                        "Paste a raw HL7 message from your clipboard, type/paste directly below, or pick another loaded message.",
                                    )
                                    .small()
                                    .color(palette.muted_text),
                                );
                                ui.add_space(8.0);

                                // Action Row: Primary "Paste from Clipboard" + "Or pick another loaded message..." dropdown
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    if primary_icon_labeled_button(
                                        ui,
                                        IconKind::Paste,
                                        "Paste from Clipboard",
                                        true,
                                        palette.is_dark,
                                    )
                                    .on_hover_text(
                                        "Read an HL7 message from the clipboard and compare immediately",
                                    )
                                    .clicked()
                                    {
                                        do_paste_baseline = true;
                                    }

                                    let picker_enabled = !loaded_choices.is_empty();
                                    ui.add_enabled_ui(picker_enabled, |ui| {
                                        egui::ComboBox::from_id_salt("compare_pick_loaded_baseline")
                                            .width(235.0)
                                            .selected_text(if picker_enabled {
                                                "Or pick another loaded message..."
                                            } else {
                                                "No other loaded messages"
                                            })
                                            .show_ui(ui, |ui| {
                                                for (doc_idx, label) in &loaded_choices {
                                                    if ui.selectable_label(false, label).clicked() {
                                                        selected_loaded_baseline = Some(*doc_idx);
                                                    }
                                                }
                                            });
                                    });
                                });

                                ui.add_space(8.0);

                                // Large auto-focused monospace TextEdit::multiline
                                let editor_h = (pane_h - 165.0).max(150.0);
                                Frame::NONE
                                    .fill(palette.base_canvas)
                                    .stroke(Stroke::new(1.0_f32, palette.divider))
                                    .corner_radius(CornerRadius::same(4))
                                    .inner_margin(Margin::same(6))
                                    .show(ui, |ui| {
                                        ScrollArea::vertical()
                                            .id_salt("compare_baseline_input_scroll")
                                            .max_height(editor_h)
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                                let edit_resp = ui.add_sized(
                                                    egui::vec2(ui.available_width(), editor_h - 4.0),
                                                    egui::TextEdit::multiline(&mut baseline_draft)
                                                        .code_editor()
                                                        .frame(false)
                                                        .hint_text(
                                                            "Paste raw HL7 baseline message here (Ctrl+V)...",
                                                        )
                                                        .desired_width(f32::INFINITY),
                                                );
                                                if modal_snap.focus_baseline_input {
                                                    edit_resp.request_focus();
                                                }
                                                if edit_resp.changed() {
                                                    baseline_draft_changed = true;
                                                }
                                            });
                                    });

                                ui.add_space(8.0);

                                // Bottom action bar: Apply / Compare (+ Cancel if editing existing baseline)
                                ui.horizontal(|ui| {
                                    let can_apply = !baseline_draft.trim().is_empty();
                                    if icon_labeled_button(
                                        ui,
                                        IconKind::Compare,
                                        "Apply / Compare",
                                        can_apply,
                                        Some(palette.accent),
                                    )
                                    .on_hover_text(
                                        "Compare the baseline text above against the current message",
                                    )
                                    .clicked()
                                    {
                                        do_apply_baseline = true;
                                    }

                                    if !modal_snap.left_raw.trim().is_empty()
                                        && ui.button("Cancel").clicked()
                                    {
                                        do_cancel_edit_baseline = true;
                                    }
                                });
                            });
                        });

                    // Right Pane — "Current Message (Loaded)"
                    Frame::NONE
                        .fill(palette.side_panel_bg)
                        .stroke(Stroke::new(1.0_f32, palette.divider))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width((col_w - 26.0).max(210.0));
                            ui.set_min_height((pane_h - 26.0).max(240.0));

                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new("Current Message (Loaded)")
                                        .strong()
                                        .size(15.0)
                                        .color(palette.primary_text),
                                );
                                ui.add_space(4.0);
                                Frame::NONE
                                    .fill(palette.elevated_bg)
                                    .stroke(Stroke::new(1.0_f32, palette.divider))
                                    .corner_radius(CornerRadius::same(4))
                                    .inner_margin(Margin::symmetric(8, 4))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(&modal_snap.right_label)
                                                .monospace()
                                                .strong()
                                                .color(palette.accent),
                                        );
                                    });
                                ui.add_space(8.0);

                                let preview_h = (pane_h - 110.0).max(150.0);
                                let preview_text =
                                    normalize_hl7_for_multiline(&modal_snap.right_raw);
                                Frame::NONE
                                    .fill(palette.base_canvas)
                                    .stroke(Stroke::new(1.0_f32, palette.divider))
                                    .corner_radius(CornerRadius::same(4))
                                    .inner_margin(Margin::same(8))
                                    .show(ui, |ui| {
                                        ScrollArea::both()
                                            .id_salt("compare_right_preview_scroll")
                                            .max_height(preview_h)
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                                if preview_text.trim().is_empty() {
                                                    ui.weak("No current message loaded.");
                                                } else {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(preview_text)
                                                                .monospace()
                                                                .color(palette.primary_text),
                                                        )
                                                        .selectable(true),
                                                    );
                                                }
                                            });
                                    });
                            });
                        });
                });
                return;
            }

            // Top Navigation & Action Bar (Active Diff View)
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;

                // Prev Diff & Next Diff with vector-painted up/down chevrons
                let has_diffs = total_diffs > 0;
                if icon_labeled_button(ui, IconKind::MoveUp, "Prev Diff", has_diffs, None)
                    .on_hover_text("Previous Difference (Shift+F7 / Up)")
                    .clicked()
                {
                    do_prev = true;
                }
                if icon_labeled_button(ui, IconKind::MoveDown, "Next Diff", has_diffs, None)
                    .on_hover_text("Next Difference (F7 / Down)")
                    .clicked()
                {
                    do_next = true;
                }

                let counter_label = if total_diffs == 0 {
                    "0 diffs (Identical)".to_string()
                } else if let Some(cur) = active_cursor {
                    format!("Diff {} of {total_diffs}", cur + 1)
                } else {
                    format!("{total_diffs} diff(s)")
                };
                Frame::NONE
                    .fill(palette.elevated_bg)
                    .stroke(Stroke::new(1.0_f32, palette.divider))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(counter_label).monospace().strong().color(
                            if total_diffs > 0 {
                                palette.accent
                            } else {
                                palette.accent_green
                            },
                        ));
                    });

                ui.separator();

                ui.checkbox(&mut new_opts.differences_only, "Differences Only");
                ui.checkbox(
                    &mut new_opts.ignore_volatile_ids_and_timestamps,
                    "Ignore Timestamps & Control IDs",
                )
                .on_hover_text("Ignore volatile MSH-7, MSH-10, and EVN-2 differences");

                ui.separator();

                if !is_multi_target
                    && icon_labeled_button(ui, IconKind::Replace, "Swap Left / Right", true, None)
                        .clicked()
                {
                    do_swap = true;
                }
                if icon_labeled_button(ui, IconKind::Paste, "Paste Baseline", true, None)
                    .on_hover_text(
                        "Paste an HL7 message from clipboard into the Left (Baseline) pane",
                    )
                    .clicked()
                {
                    do_paste_baseline = true;
                }
                if icon_labeled_button(ui, IconKind::Edit, "Edit Baseline", true, None)
                    .on_hover_text("Edit, paste, or pick a different Left (Baseline) HL7 message")
                    .clicked()
                {
                    do_edit_baseline = true;
                }
                if icon_labeled_button(ui, IconKind::Copy, "Copy Diff Summary", true, None)
                    .clicked()
                {
                    do_copy_summary = true;
                }
            });

            ui.separator();

            // Theme-aware WinMerge diff colors + subtle alternating zebra striping for unchanged rows
            let colors = DiffColors::for_theme(palette.is_dark);
            let zebra_stripe_bg = ui.visuals().faint_bg_color;
            let mono_font = FontId::monospace(egui::TextStyle::Monospace.resolve(ui.style()).size);
            let total_w = ui.available_width();
            let gutter_w = 155.0_f32;

            if is_multi_target {
                let target_count = modal_snap.targets.len();
                let num_msg_cols = 1 + target_count;
                let col_w = ((total_w - gutter_w - 28.0) / num_msg_cols as f32).max(240.0);
                let cell_pad_x = 6.0_f32;
                let inner_text_w = (col_w - cell_pad_x * 2.0).max(160.0);
                let grid_w =
                    col_w * num_msg_cols as f32 + gutter_w + (num_msg_cols as f32 + 2.0) * 4.0;
                let scroll_h = (ui.available_height() - 6.0).max(180.0);

                let multi_scroll_salt = "winmerge_multi_compare_scroll";
                let multi_scroll_id = ui.make_persistent_id(multi_scroll_salt);
                let prev_scroll_x = if reset_scroll {
                    0.0
                } else {
                    egui::scroll_area::State::load(ui.ctx(), multi_scroll_id)
                        .map_or(0.0, |s| s.offset.x)
                };

                let mut multi_scroll = ScrollArea::both()
                    .id_salt(multi_scroll_salt)
                    .max_height(scroll_h)
                    .auto_shrink([false, false]);
                if reset_scroll {
                    multi_scroll = multi_scroll.scroll_offset(egui::vec2(0.0, 0.0));
                }

                let scroll_out = multi_scroll.show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;

                        // Multi-column header row: Baseline (with BASELINE badge) | Gutter | Target Columns
                        Frame::NONE
                            .fill(palette.side_panel_bg)
                            .stroke(Stroke::new(1.0_f32, palette.divider))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(4, 5))
                            .show(ui, |ui| {
                                ui.set_min_width(grid_w);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;

                                    // Baseline column header (strict width + ellipsis truncation)
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(col_w, 24.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.set_max_width(col_w);
                                            Frame::NONE
                                                .fill(palette.accent.gamma_multiply(0.22))
                                                .stroke(Stroke::new(1.0_f32, palette.accent))
                                                .corner_radius(CornerRadius::same(3))
                                                .inner_margin(Margin::symmetric(5, 1))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new("BASELINE")
                                                            .small()
                                                            .strong()
                                                            .color(palette.accent),
                                                    );
                                                });
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&modal_snap.left_label).strong(),
                                                )
                                                .wrap_mode(egui::TextWrapMode::Truncate),
                                            )
                                            .on_hover_text(&modal_snap.left_label);
                                        },
                                    );

                                    // Gutter header
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(gutter_w, 24.0),
                                        egui::Layout::centered_and_justified(
                                            egui::Direction::LeftToRight,
                                        ),
                                        |ui| {
                                            ui.set_max_width(gutter_w);
                                            ui.weak("HL7 Path / Δ");
                                        },
                                    );

                                    // Target column headers with Make Baseline + × buttons (strict width + truncated label)
                                    for (t_idx, target) in modal_snap.targets.iter().enumerate() {
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(col_w, 24.0),
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.set_max_width(col_w);
                                                ui.spacing_mut().item_spacing.x = 4.0;
                                                if ui
                                                    .small_button("×")
                                                    .on_hover_text(
                                                        "Remove this message column from comparison",
                                                    )
                                                    .clicked()
                                                {
                                                    remove_target_idx = Some(t_idx);
                                                }
                                                if ui
                                                    .small_button("Make Baseline")
                                                    .on_hover_text(
                                                        "Promote this message to Baseline and re-diff all columns against it",
                                                    )
                                                    .clicked()
                                                {
                                                    promote_target_idx = Some(t_idx);
                                                }
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&target.label).strong(),
                                                    )
                                                    .wrap_mode(egui::TextWrapMode::Truncate),
                                                )
                                                .on_hover_text(&target.label);
                                            },
                                        );
                                    }
                                });
                            });

                        ui.add_space(4.0);

                        if modal_snap.multi_diff.blocks.is_empty() {
                            ui.centered_and_justified(|ui| {
                                ui.colored_label(
                                    palette.accent_green,
                                    "No differences found with current options.",
                                );
                            });
                            return;
                        }

                        let mut diff_ordinal = 0usize;
                        for block in &modal_snap.multi_diff.blocks {
                            let seg_title = if block.occurrence > 1 {
                                format!("{}({})", block.segment_tag, block.occurrence)
                            } else {
                                block.segment_tag.clone()
                            };
                            let seg_desc = block
                                .description
                                .as_deref()
                                .map(|d| format!(" — {d}"))
                                .unwrap_or_default();
                            let seg_badge = match block.kind {
                                DiffChangeKind::Unchanged => "",
                                DiffChangeKind::Modified => " [Modified]",
                                DiffChangeKind::Added => " [Added]",
                                DiffChangeKind::Removed => " [Removed]",
                            };
                            let seg_color = match block.kind {
                                DiffChangeKind::Modified => colors.mod_accent,
                                DiffChangeKind::Added => colors.add_accent,
                                DiffChangeKind::Removed => colors.del_accent,
                                DiffChangeKind::Unchanged => palette.primary_text,
                            };
                            Frame::NONE
                                .fill(palette.elevated_bg)
                                .stroke(Stroke::new(1.0_f32, palette.divider))
                                .corner_radius(CornerRadius::same(4))
                                .inner_margin(Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.set_min_width(grid_w);
                                    ui.label(
                                        RichText::new(format!("{seg_title}{seg_desc}{seg_badge}"))
                                            .monospace()
                                            .strong()
                                            .color(seg_color),
                                    );
                                });

                            for (row_idx, row) in block.rows.iter().enumerate() {
                                let this_diff_ord = if row.is_navigable_diff {
                                    let o = diff_ordinal;
                                    diff_ordinal += 1;
                                    Some(o)
                                } else {
                                    None
                                };
                                let is_active_diff =
                                    this_diff_ord.is_some() && this_diff_ord == active_cursor;

                                let unchanged_bg = if row_idx % 2 == 1 {
                                    zebra_stripe_bg
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let row_stroke = if is_active_diff {
                                    Stroke::new(1.5_f32, palette.accent)
                                } else {
                                    Stroke::NONE
                                };
                                let (marker, marker_fg) = if row.has_any_diff {
                                    ("!=", colors.mod_accent)
                                } else {
                                    ("==", palette.muted_text)
                                };

                                // Pre-build wrapped LayoutJobs & measure tallest cell height so all columns
                                // in this row share the exact same height and zero text overflows borders.
                                let base_job = row.baseline_text.as_deref().map(|btxt| {
                                    build_plain_wrapped_layout_job(
                                        btxt,
                                        mono_font.clone(),
                                        palette.primary_text,
                                        inner_text_w,
                                    )
                                });
                                let mut max_text_h = 16.0_f32;
                                if let Some(ref job) = base_job {
                                    let galley = ui.painter().layout_job(job.clone());
                                    max_text_h = max_text_h.max(galley.size().y);
                                }
                                let target_jobs: Vec<Option<LayoutJob>> = row
                                    .cells
                                    .iter()
                                    .map(|cell| {
                                        if cell.text.is_some() {
                                            let job = build_char_diff_layout_job(
                                                &cell.target_spans,
                                                mono_font.clone(),
                                                palette.primary_text,
                                                &colors,
                                                inner_text_w,
                                            );
                                            let galley = ui.painter().layout_job(job.clone());
                                            max_text_h = max_text_h.max(galley.size().y);
                                            Some(job)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let row_h = (max_text_h + 6.0).max(22.0);

                                let row_frame = Frame::NONE
                                    .stroke(row_stroke)
                                    .corner_radius(CornerRadius::same(3))
                                    .inner_margin(Margin::symmetric(2, 1))
                                    .show(ui, |ui| {
                                        ui.set_min_width(grid_w);
                                        ui.horizontal_top(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;

                                            // Baseline cell (fixed col_w × row_h, clipped & wrapped inside border)
                                            let base_bg = if row.baseline_text.is_none() {
                                                colors.blank_row_bg
                                            } else {
                                                unchanged_bg
                                            };
                                            render_clipped_diff_cell(
                                                ui,
                                                col_w,
                                                row_h,
                                                cell_pad_x,
                                                base_bg,
                                                |ui| {
                                                    if let Some(job) = base_job {
                                                        ui.add(
                                                            egui::Label::new(job)
                                                                .wrap()
                                                                .selectable(true),
                                                        );
                                                    } else if block.kind == DiffChangeKind::Added {
                                                        ui.label(
                                                            RichText::new("— (absent)")
                                                                .monospace()
                                                                .small()
                                                                .color(palette.muted_text),
                                                        );
                                                    } else {
                                                        ui.label(
                                                            RichText::new(" ")
                                                                .monospace()
                                                                .color(palette.muted_text),
                                                        );
                                                    }
                                                },
                                            );

                                            // Center gutter: HL7 path + change marker (fixed gutter_w × row_h)
                                            render_clipped_diff_cell(
                                                ui,
                                                gutter_w,
                                                row_h,
                                                5.0,
                                                palette.side_panel_bg,
                                                |ui| {
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            RichText::new(marker)
                                                                .monospace()
                                                                .strong()
                                                                .color(marker_fg),
                                                        );
                                                        let mut path_resp = ui.add(
                                                            egui::Label::new(
                                                                RichText::new(&row.path)
                                                                    .monospace()
                                                                    .small()
                                                                    .color(palette.primary_text),
                                                            )
                                                            .wrap_mode(egui::TextWrapMode::Truncate)
                                                            .sense(egui::Sense::click()),
                                                        );
                                                        if let Some(ref desc) = row.description {
                                                            path_resp = path_resp.on_hover_text(
                                                                format!("{} — {desc}", row.path),
                                                            );
                                                        }
                                                        if path_resp.clicked() {
                                                            if let Some(ord) = this_diff_ord {
                                                                clicked_diff_ordinal = Some(ord);
                                                            }
                                                        }
                                                    });
                                                },
                                            );

                                            // Target column cells (fixed col_w × row_h, clipped & wrapped inside border)
                                            for (cell, job_opt) in row.cells.iter().zip(target_jobs)
                                            {
                                                let cell_bg = match cell.kind {
                                                    DiffChangeKind::Unchanged => unchanged_bg,
                                                    DiffChangeKind::Modified => colors.mod_row_bg,
                                                    DiffChangeKind::Added => colors.add_row_bg,
                                                    DiffChangeKind::Removed => colors.del_row_bg,
                                                };
                                                render_clipped_diff_cell(
                                                    ui,
                                                    col_w,
                                                    row_h,
                                                    cell_pad_x,
                                                    cell_bg,
                                                    |ui| {
                                                        if let Some(job) = job_opt {
                                                            ui.add(
                                                                egui::Label::new(job)
                                                                    .wrap()
                                                                    .selectable(true),
                                                            );
                                                        } else if cell.kind
                                                            == DiffChangeKind::Removed
                                                        {
                                                            ui.label(
                                                                RichText::new("— (absent)")
                                                                    .monospace()
                                                                    .small()
                                                                    .color(colors.del_accent),
                                                            );
                                                        } else {
                                                            ui.label(
                                                                RichText::new(" ")
                                                                    .monospace()
                                                                    .color(palette.muted_text),
                                                            );
                                                        }
                                                    },
                                                );
                                            }
                                        });
                                    });

                                if is_active_diff && scroll_to_active {
                                    row_frame.response.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        }
                    });
                // Preserve horizontal scroll offset (`x`) during vertical diff navigation so the
                // Baseline column is never scrolled off the left edge.
                if scroll_to_active || reset_scroll {
                    let target_x = if reset_scroll { 0.0 } else { prev_scroll_x };
                    if let Some(mut st) = egui::scroll_area::State::load(ui.ctx(), scroll_out.id) {
                        st.offset.x = target_x;
                        if reset_scroll {
                            st.offset.y = 0.0;
                        }
                        st.store(ui.ctx(), scroll_out.id);
                    }
                }
                return;
            }

            // Column headers for 2-column Left | Center Gutter | Right
            let side_w = ((total_w - gutter_w - 20.0) * 0.5).max(160.0);

            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(side_w, 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.strong(format!("Left (Baseline): {}", modal_snap.left_label));
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(gutter_w, 20.0),
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        ui.weak("HL7 Path / Δ");
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(side_w, 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.strong(format!("Right (Current): {}", modal_snap.right_label));
                    },
                );
            });
            ui.separator();

            if modal_snap.diff.blocks.is_empty() {
                ui.centered_and_justified(|ui| {
                    if modal_snap.left_raw.trim().is_empty()
                        && modal_snap.right_raw.trim().is_empty()
                    {
                        ui.weak(
                            "Select 1 or 2 messages or paste a baseline HL7 message to compare.",
                        );
                    } else {
                        ui.colored_label(
                            palette.accent_green,
                            "No differences found with current options.",
                        );
                    }
                });
            } else {
                let scroll_h = (ui.available_height() - 8.0).max(160.0);
                let mut diff_ordinal = 0usize;

                let mut two_col_scroll = ScrollArea::vertical()
                    .id_salt("winmerge_compare_scroll")
                    .max_height(scroll_h)
                    .auto_shrink([false, false]);
                if reset_scroll {
                    two_col_scroll = two_col_scroll.scroll_offset(egui::vec2(0.0, 0.0));
                }
                two_col_scroll.show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;

                        for block in &modal_snap.diff.blocks {
                            let seg_title = if block.occurrence > 1 {
                                format!("{}({})", block.segment_tag, block.occurrence)
                            } else {
                                block.segment_tag.clone()
                            };
                            let seg_desc = block
                                .description
                                .as_deref()
                                .map(|d| format!(" — {d}"))
                                .unwrap_or_default();
                            let seg_badge = match block.kind {
                                DiffChangeKind::Unchanged => "",
                                DiffChangeKind::Modified => " [Modified]",
                                DiffChangeKind::Added => " [Added]",
                                DiffChangeKind::Removed => " [Removed]",
                            };
                            Frame::NONE
                                .fill(palette.elevated_bg)
                                .stroke(Stroke::new(1.0_f32, palette.divider))
                                .corner_radius(CornerRadius::same(4))
                                .inner_margin(Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.set_width((total_w - 16.0).max(200.0));
                                    ui.horizontal(|ui| {
                                        let seg_color = match block.kind {
                                            DiffChangeKind::Modified => colors.mod_accent,
                                            DiffChangeKind::Added => colors.add_accent,
                                            DiffChangeKind::Removed => colors.del_accent,
                                            DiffChangeKind::Unchanged => palette.primary_text,
                                        };
                                        ui.label(
                                            RichText::new(format!(
                                                "{seg_title}{seg_desc}{seg_badge}"
                                            ))
                                            .monospace()
                                            .strong()
                                            .color(seg_color),
                                        );
                                    });
                                });

                            for (row_idx, row) in block.rows.iter().enumerate() {
                                let this_diff_ord = if row.is_navigable_diff {
                                    let o = diff_ordinal;
                                    diff_ordinal += 1;
                                    Some(o)
                                } else {
                                    None
                                };
                                let is_active_diff =
                                    this_diff_ord.is_some() && this_diff_ord == active_cursor;

                                let unchanged_bg = if row_idx % 2 == 1 {
                                    zebra_stripe_bg
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let (left_bg, right_bg, marker, marker_fg) = match row.kind {
                                    DiffChangeKind::Unchanged => {
                                        (unchanged_bg, unchanged_bg, "==", palette.muted_text)
                                    }
                                    DiffChangeKind::Modified => (
                                        colors.mod_row_bg,
                                        colors.mod_row_bg,
                                        "!=",
                                        colors.mod_accent,
                                    ),
                                    DiffChangeKind::Removed => (
                                        colors.del_row_bg,
                                        colors.blank_row_bg,
                                        "-",
                                        colors.del_accent,
                                    ),
                                    DiffChangeKind::Added => (
                                        colors.blank_row_bg,
                                        colors.add_row_bg,
                                        "+",
                                        colors.add_accent,
                                    ),
                                };

                                let row_stroke = if is_active_diff {
                                    Stroke::new(1.5_f32, palette.accent)
                                } else {
                                    Stroke::NONE
                                };

                                let row_frame = Frame::NONE
                                    .stroke(row_stroke)
                                    .corner_radius(CornerRadius::same(3))
                                    .inner_margin(Margin::symmetric(2, 1))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;

                                            // Left pane cell
                                            Frame::NONE
                                                .fill(left_bg)
                                                .corner_radius(CornerRadius::same(2))
                                                .inner_margin(Margin::symmetric(6, 2))
                                                .show(ui, |ui| {
                                                    let cell_text_w = (side_w - 28.0).max(120.0);
                                                    ui.set_width((side_w - 16.0).max(120.0));
                                                    ui.set_max_width((side_w - 16.0).max(120.0));
                                                    if row.left_text.is_some() {
                                                        let job = build_char_diff_layout_job(
                                                            &row.left_spans,
                                                            mono_font.clone(),
                                                            palette.primary_text,
                                                            &colors,
                                                            cell_text_w,
                                                        );
                                                        ui.add(
                                                            egui::Label::new(job)
                                                                .wrap()
                                                                .selectable(true),
                                                        );
                                                    } else if row.kind == DiffChangeKind::Added {
                                                        ui.label(
                                                            RichText::new("— (absent)")
                                                                .monospace()
                                                                .small()
                                                                .italics()
                                                                .color(palette.muted_text),
                                                        );
                                                    } else {
                                                        ui.label(
                                                            RichText::new(" ")
                                                                .monospace()
                                                                .color(palette.muted_text),
                                                        );
                                                    }
                                                });

                                            // Center gutter: HL7 path + change marker (`!=`, `+`, `-`, `==`)
                                            Frame::NONE
                                                .fill(palette.side_panel_bg)
                                                .corner_radius(CornerRadius::same(2))
                                                .inner_margin(Margin::symmetric(5, 2))
                                                .show(ui, |ui| {
                                                    ui.set_width((gutter_w - 12.0).max(90.0));
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            RichText::new(marker)
                                                                .monospace()
                                                                .strong()
                                                                .color(marker_fg),
                                                        );
                                                        let mut path_resp = ui.add(
                                                            egui::Label::new(
                                                                RichText::new(&row.path)
                                                                    .monospace()
                                                                    .small()
                                                                    .color(palette.primary_text),
                                                            )
                                                            .truncate()
                                                            .sense(egui::Sense::click()),
                                                        );
                                                        if let Some(ref desc) = row.description {
                                                            path_resp = path_resp.on_hover_text(
                                                                format!("{} — {desc}", row.path),
                                                            );
                                                        }
                                                        if path_resp.clicked() {
                                                            if let Some(ord) = this_diff_ord {
                                                                clicked_diff_ordinal = Some(ord);
                                                            }
                                                        }
                                                    });
                                                });

                                            // Right pane cell
                                            Frame::NONE
                                                .fill(right_bg)
                                                .corner_radius(CornerRadius::same(2))
                                                .inner_margin(Margin::symmetric(6, 2))
                                                .show(ui, |ui| {
                                                    let cell_text_w = (side_w - 28.0).max(120.0);
                                                    ui.set_width((side_w - 16.0).max(120.0));
                                                    ui.set_max_width((side_w - 16.0).max(120.0));
                                                    if row.right_text.is_some() {
                                                        let job = build_char_diff_layout_job(
                                                            &row.right_spans,
                                                            mono_font.clone(),
                                                            palette.primary_text,
                                                            &colors,
                                                            cell_text_w,
                                                        );
                                                        ui.add(
                                                            egui::Label::new(job)
                                                                .wrap()
                                                                .selectable(true),
                                                        );
                                                    } else if row.kind == DiffChangeKind::Removed {
                                                        ui.label(
                                                            RichText::new("— (absent)")
                                                                .monospace()
                                                                .small()
                                                                .italics()
                                                                .color(palette.muted_text),
                                                        );
                                                    } else {
                                                        ui.label(
                                                            RichText::new(" ")
                                                                .monospace()
                                                                .color(palette.muted_text),
                                                        );
                                                    }
                                                });
                                        });
                                    });

                                if is_active_diff && scroll_to_active {
                                    row_frame.response.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        }
                    });
            }
        };

        if class == egui::ViewportClass::Embedded {
            egui::Window::new("Compare HL7 Messages")
                .id(egui::Id::new("winmerge_compare_messages_modal"))
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size([1120.0, 660.0])
                .min_size([720.0, 380.0])
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(vp_ctx, render_contents);
        } else {
            egui::CentralPanel::default().show(vp_ctx, render_contents);
        }

        if let Some(ref mut modal) = state.compare_modal {
            modal.scroll_to_active_diff = false;
            modal.reset_scroll_to_top_left = false;
            modal.focus_baseline_input = false;
            if baseline_draft_changed {
                modal.baseline_draft = baseline_draft.clone();
            }
        }

        if !open {
            state.compare_modal = None;
            return;
        }
        if let Some(doc_idx) = selected_loaded_baseline {
            state.compare_select_loaded_baseline(doc_idx);
        } else if do_apply_baseline {
            if !baseline_draft.trim().is_empty() {
                state.compare_apply_baseline(&baseline_draft, Some("Baseline (Pasted)"));
            }
        } else if baseline_draft_changed
            && had_paste_event
            && is_valid_hl7_msh_payload(&baseline_draft)
        {
            state.compare_apply_baseline(&baseline_draft, Some("Baseline (Pasted)"));
        } else if do_cancel_edit_baseline {
            if let Some(ref mut modal) = state.compare_modal {
                modal.editing_baseline = false;
                modal.focus_baseline_input = false;
                modal.baseline_draft = normalize_hl7_for_multiline(&modal.left_raw);
            }
        } else if do_edit_baseline {
            state.compare_begin_edit_baseline();
        }
        if new_opts != modal_snap.options {
            if let Some(ref mut modal) = state.compare_modal {
                modal.options = new_opts;
            }
            state.compare_recompute();
        }
        if do_prev {
            state.compare_prev_diff();
        } else if do_next {
            state.compare_next_diff();
        } else if let Some(ord) = clicked_diff_ordinal {
            if let Some(ref mut modal) = state.compare_modal {
                modal.active_diff_cursor = Some(ord);
                modal.scroll_to_active_diff = true;
            }
        }
        if let Some(t_idx) = promote_target_idx {
            state.compare_promote_target_to_baseline(t_idx);
        } else if let Some(t_idx) = remove_target_idx {
            state.compare_remove_target(t_idx);
        }
        if do_swap {
            state.compare_swap_sides();
        }
        if do_paste_baseline {
            match read_clipboard(vp_ctx) {
                Ok(text) if !text.trim().is_empty() => {
                    state.compare_paste_baseline(&text);
                }
                _ => {
                    state.status_message =
                        "Clipboard does not contain text to paste as Compare baseline.".to_string();
                }
            }
        }
        if do_copy_summary {
            if let Some(ref modal) = state.compare_modal {
                let summary = if modal.is_multi_target() {
                    modal.multi_diff.format_summary(&modal.left_label)
                } else {
                    modal
                        .diff
                        .format_summary(&modal.left_label, &modal.right_label)
                };
                vp_ctx.copy_text(summary);
                state.status_message = "Copied HL7 Diff Summary to clipboard.".to_string();
            }
        }
    });
}

/// Returns `true` when `text` starts with a valid HL7 `MSH` header (after trimming BOM/whitespace).
fn is_valid_hl7_msh_payload(text: &str) -> bool {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.len() >= 9 && trimmed.starts_with("MSH") && trimmed.as_bytes()[3].is_ascii_punctuation()
}

/// Render a prominent primary-accent button with a leading 13×13 vector icon and crisp white text.
fn primary_icon_labeled_button(
    ui: &mut egui::Ui,
    icon: IconKind,
    label: &str,
    enabled: bool,
    is_dark: bool,
) -> egui::Response {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let fg = if enabled {
        Color32::WHITE
    } else {
        ui.visuals().weak_text_color()
    };
    let fill = if enabled {
        if is_dark {
            Color32::from_rgb(0x1D, 0x6F, 0xC2)
        } else {
            Color32::from_rgb(0x15, 0x65, 0xC0)
        }
    } else {
        ui.visuals().widgets.noninteractive.bg_fill
    };
    let label_galley = ui.painter().layout_no_wrap(label.to_string(), font_id, fg);
    let left_pad = 10.0_f32;
    let gutter = 20.0_f32;
    let right_pad = 12.0_f32;
    let desired = egui::vec2(
        left_pad + gutter + label_galley.size().x + right_pad,
        (label_galley.size().y + 10.0).max(26.0),
    );
    let resp = ui.add_enabled(
        enabled,
        egui::Button::new("")
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, fill))
            .corner_radius(CornerRadius::same(4))
            .min_size(desired),
    );
    if ui.is_rect_visible(resp.rect) {
        let icon_center = egui::pos2(resp.rect.left() + left_pad + 6.5, resp.rect.center().y);
        paint_icon(ui.painter(), icon, icon_center, fg, fill, 13.0);
        let label_pos = egui::pos2(
            resp.rect.left() + left_pad + gutter,
            resp.rect.center().y - label_galley.size().y * 0.5,
        );
        ui.painter().galley(label_pos, label_galley, fg);
    }
    resp
}

struct DiffColors {
    mod_row_bg: Color32,
    del_row_bg: Color32,
    add_row_bg: Color32,
    blank_row_bg: Color32,
    del_char_bg: Color32,
    add_char_bg: Color32,
    mod_accent: Color32,
    del_accent: Color32,
    add_accent: Color32,
}

impl DiffColors {
    fn for_theme(is_dark: bool) -> Self {
        if is_dark {
            Self {
                mod_row_bg: Color32::from_rgb(0x2F, 0x26, 0x12),
                del_row_bg: Color32::from_rgb(0x36, 0x1A, 0x1E),
                add_row_bg: Color32::from_rgb(0x16, 0x31, 0x20),
                blank_row_bg: Color32::from_rgb(0x21, 0x25, 0x2B),
                del_char_bg: Color32::from_rgb(0x8E, 0x24, 0x2E),
                add_char_bg: Color32::from_rgb(0x1D, 0x6F, 0x3B),
                mod_accent: Color32::from_rgb(0xF2, 0xCC, 0x60),
                del_accent: Color32::from_rgb(0xF8, 0x71, 0x71),
                add_accent: Color32::from_rgb(0x4A, 0xDE, 0x80),
            }
        } else {
            Self {
                mod_row_bg: Color32::from_rgb(0xFF, 0xF8, 0xDC),
                del_row_bg: Color32::from_rgb(0xFF, 0xEB, 0xEE),
                add_row_bg: Color32::from_rgb(0xE8, 0xF5, 0xE9),
                blank_row_bg: Color32::from_rgb(0xEE, 0xF0, 0xF3),
                del_char_bg: Color32::from_rgb(0xFF, 0xB3, 0xBA),
                add_char_bg: Color32::from_rgb(0xA5, 0xD6, 0xA7),
                mod_accent: Color32::from_rgb(0x9A, 0x67, 0x00),
                del_accent: Color32::from_rgb(0xC5, 0x30, 0x30),
                add_accent: Color32::from_rgb(0x1B, 0x7F, 0x37),
            }
        }
    }
}

/// Render a fixed-size, clipped diff cell rectangle (`width × height`) with inner horizontal padding
/// so wrapped text and background fills stay strictly inside the cell borders.
fn render_clipped_diff_cell(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    pad_x: f32,
    bg: Color32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(2), bg);
    }
    let inner_rect = rect.shrink2(egui::vec2(pad_x, 2.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.set_max_width(inner_rect.width().max(1.0));
    add_contents(&mut child);
}

fn build_plain_wrapped_layout_job(
    text: &str,
    font_id: FontId,
    fg: Color32,
    max_width: f32,
) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: max_width.max(40.0),
            break_anywhere: true,
            ..Default::default()
        },
        ..Default::default()
    };
    job.append(
        text,
        0.0,
        TextFormat {
            font_id,
            color: fg,
            ..Default::default()
        },
    );
    job
}

fn build_char_diff_layout_job(
    spans: &[CharDiffSpan],
    font_id: FontId,
    fg: Color32,
    colors: &DiffColors,
    max_width: f32,
) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: max_width.max(40.0),
            break_anywhere: true,
            ..Default::default()
        },
        ..Default::default()
    };
    for span in spans {
        let bg = match span.kind {
            CharDiffKind::Equal => Color32::TRANSPARENT,
            CharDiffKind::Deleted => colors.del_char_bg,
            CharDiffKind::Inserted => colors.add_char_bg,
        };
        job.append(
            &span.text,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: fg,
                background: bg,
                ..Default::default()
            },
        );
    }
    job
}
