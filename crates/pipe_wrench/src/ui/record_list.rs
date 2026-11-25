//! Virtualized Record list pane (`§5.1`, `NFR-1`, `NFR-6`, `D-14`, `D-26`, `D-29`).
//!
//! Uses `egui_extras::TableBuilder` with uniform row height and full-row hit-testing (`Sense::click_and_drag()`)
//! so 250,000+ records stay smooth with zero dead click zones.
//! Features:
//! - Top navigation toolbar (`|<` First, `<` Prev, clickable `Rec # / Total` inline numeric jump
//!   control, `>` Next, `>|` Last, plus `Prev Page` / `Page X of Y` / `Next Page` / `Load All` when paged)
//! - Full-row selection (`Click`, `Ctrl+Click`, `Shift+Click`), keyboard navigation (`Arrow`/`Page`/`Home`/`End`),
//!   drag-and-drop row reordering with accent drop-target line, `Ctrl+C` clipboard copy, and right-click context menu
//! - Dedicated bottom CRUD / Reorder footer bar (`+` green Insert Record, `↑` Move Up, `↓` Move Down, `-` red Delete).

use egui::{CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};

use crate::config::AppSettings;
use crate::state::SessionState;
use crate::ui::icons::{icon_button, icon_menu_button, icon_menu_button_tinted, IconKind};
use crate::ui::theme::{
    message_type_pill_colors_with_delim, paint_left_accent_bar, render_pill_badge, ThemePalette,
};

/// Actions triggered from the `Records` table context menu or keyboard shortcuts (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordListAction {
    /// No external action needed.
    None,
    /// Open the Save Selected Records file dialog (`FS-1`).
    SaveSelectedRecords,
    /// Paste HL7 message(s) from the system clipboard (`Edit -> Paste Message(s)`).
    PasteFromClipboard,
    /// Open the WinMerge-style side-by-side Compare window for two explicitly selected messages.
    CompareMessages(usize, usize),
    /// Open the WinMerge-style side-by-side Compare window from the current 1- or 2-message selection.
    CompareSelected,
}

/// Maximum number of messages that can be compared simultaneously (1 Baseline + up to 4 Targets).
pub use crate::state::MAX_COMPARE_MESSAGES;

/// Returns `true` when the bottom Messages toolbar Compare button should be enabled
/// (`1..=MAX_COMPARE_MESSAGES` selected).
pub fn can_compare_selected_records(selected_count: usize) -> bool {
    (1..=MAX_COMPARE_MESSAGES).contains(&selected_count)
}

/// Render the virtualized record list pane, top navigation bar, full-row hit-testing,
/// drag-and-drop row reordering, and bottom CRUD / Reorder footer bar (`NFR-6`, `D-26`, `D-29`).
pub fn show_record_list(
    ui: &mut Ui,
    state: &mut SessionState,
    settings: &AppSettings,
) -> RecordListAction {
    let palette = ThemePalette::for_visuals(ui.visuals());
    let total_all = state.summaries.len();
    let visible_indices = state.visible_record_indices();
    let visible_count = visible_indices.len();
    let (cur_pos_1_based, _, _) = state.record_position_counts();

    // Header row: Heading + Filter status / Clear
    ui.horizontal(|ui| {
        ui.heading("Messages");
        if state.filtered_record_indices.is_some() {
            ui.colored_label(
                palette.accent,
                format!("Showing {visible_count} of {total_all}"),
            );
            if ui
                .small_button("× Clear")
                .on_hover_text("Clear Message List Filter")
                .clicked()
            {
                state.clear_filter(settings);
            }
        } else {
            ui.weak(format!(
                "({total_all} total, {} selected)",
                state.selected_records.len()
            ));
        }
    });

    // Top Navigation Bar: strictly dedicated to navigation & paging (`|<`, `<`, `Msg # / Total`, `>`, `>|`)
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let can_prev = visible_count > 0 && cur_pos_1_based > 1;
        let can_next = visible_count > 0 && cur_pos_1_based < visible_count;

        if ui
            .add_enabled(can_prev, egui::Button::new(RichText::new("|<").monospace()))
            .on_hover_text("First Message (Home)")
            .clicked()
        {
            let _ = state.navigate_first_record(settings);
        }
        if ui
            .add_enabled(can_prev, egui::Button::new(RichText::new("<").monospace()))
            .on_hover_text("Previous Message (Up Arrow)")
            .clicked()
        {
            let _ = state.navigate_prev_record(settings);
        }

        if state.record_jump_active {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.record_jump_input)
                    .id_salt("record_jump_inline_input")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(72.0),
            );
            if state.focus_record_jump_input {
                resp.request_focus();
                state.focus_record_jump_input = false;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if enter {
                let _ = state.commit_record_jump_input(settings);
            } else if esc || (resp.lost_focus() && !enter) {
                state.record_jump_active = false;
            }
        } else {
            let pos_label = state.format_record_position_label();
            if ui
                .add_enabled(
                    visible_count > 0,
                    egui::Button::new(RichText::new(pos_label).monospace().strong()),
                )
                .on_hover_text("Jump to Message (Ctrl+G)")
                .clicked()
            {
                state.begin_record_jump();
            }
        }

        if ui
            .add_enabled(can_next, egui::Button::new(RichText::new(">").monospace()))
            .on_hover_text("Next Message (Down Arrow)")
            .clicked()
        {
            let _ = state.navigate_next_record(settings);
        }
        if ui
            .add_enabled(can_next, egui::Button::new(RichText::new(">|").monospace()))
            .on_hover_text("Last Message (End)")
            .clicked()
        {
            let _ = state.navigate_last_record(settings);
        }
    });

    // Bounded Windowing / Paging Controls when visible messages exceed `settings.max_records_per_page` (§5.1, NFR-1)
    let is_paged = state.is_record_list_paged(settings);
    if is_paged {
        let total_pages = state.total_record_pages(settings);
        let cur_page_1_based = (state.record_page_index + 1).min(total_pages);
        let can_prev_page = state.record_page_index > 0;
        let can_next_page = state.record_page_index + 1 < total_pages;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .add_enabled(can_prev_page, egui::Button::new("Prev Page").small())
                .on_hover_text("Previous Message Page")
                .clicked()
            {
                let _ = state.navigate_prev_record_page(settings);
            }
            ui.colored_label(
                palette.accent,
                RichText::new(format!("Page {cur_page_1_based} of {total_pages}"))
                    .monospace()
                    .strong(),
            );
            if ui
                .add_enabled(can_next_page, egui::Button::new("Next Page").small())
                .on_hover_text("Next Message Page")
                .clicked()
            {
                let _ = state.navigate_next_record_page(settings);
            }
            if ui
                .small_button("Load All")
                .on_hover_text("Show all visible messages in a single unpaged view")
                .clicked()
            {
                state.load_all_record_pages();
            }
        });
    }
    ui.separator();

    // Keyboard navigation (ArrowUp/Down, PageUp/Down, Home/End) and Ctrl+C copy (NFR-6, D-26).
    let mut copy_requested = false;
    if visible_count > 0 && !ui.ctx().wants_keyboard_input() {
        let cur_doc_idx = state.selected_record.unwrap_or(visible_indices[0]);
        let cur_vis_pos = visible_indices
            .iter()
            .position(|&i| i == cur_doc_idx)
            .unwrap_or(0);
        let mut next_vis_pos: Option<usize> = None;

        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                next_vis_pos = Some((cur_vis_pos + 1).min(visible_count - 1));
            } else if i.key_pressed(egui::Key::ArrowUp) {
                next_vis_pos = Some(cur_vis_pos.saturating_sub(1));
            } else if i.key_pressed(egui::Key::PageDown) {
                next_vis_pos = Some((cur_vis_pos + 25).min(visible_count - 1));
            } else if i.key_pressed(egui::Key::PageUp) {
                next_vis_pos = Some(cur_vis_pos.saturating_sub(25));
            } else if i.key_pressed(egui::Key::Home) {
                next_vis_pos = Some(0);
            } else if i.key_pressed(egui::Key::End) {
                next_vis_pos = Some(visible_count - 1);
            }
            if i.events.iter().any(|ev| matches!(ev, egui::Event::Copy))
                || ((i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::C))
            {
                copy_requested = true;
            }
        });

        if let Some(pos) = next_vis_pos {
            let new_doc_idx = visible_indices[pos];
            if Some(new_doc_idx) != state.selected_record {
                state.select_record(new_doc_idx, false, settings);
                state.scroll_to_record = true;
            }
        }
    }

    let mut clicked_row: Option<(usize, bool, bool)> = None;
    let mut insert_requested = false;
    let mut move_up_requested = false;
    let mut move_down_requested = false;
    let mut delete_requested = false;
    let mut drag_drop_pair: Option<(usize, usize)> = None;
    let mut action = RecordListAction::None;

    let (page_indices, _page_start_offset) = state.current_page_visible_indices(settings);
    let page_len = page_indices.len();
    let can_reorder = state.filtered_record_indices.is_none() && state.doc.len() > 1;
    let can_move_up = state.can_move_selected_records_up();
    let can_move_down = state.can_move_selected_records_down();
    let has_selection = !state.selected_records.is_empty();

    // Dock the bottom CRUD & Reorder Action Bar flush to the bottom of the Messages pane
    // BEFORE rendering the TableBuilder inside an isolated CentralPanel so vertical column
    // divider lines terminate cleanly above the toolbar and never bleed through it (D-29).
    let bottom_bar_resp = egui::TopBottomPanel::bottom("messages_bottom_toolbar")
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::NONE
                .fill(palette.side_panel_bg)
                .inner_margin(Margin::symmetric(8, 6)),
        )
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;

                if icon_button(
                    ui,
                    IconKind::Add,
                    true,
                    Some(palette.accent_green),
                    egui::vec2(28.0, 24.0),
                    "Insert New Message below selection",
                )
                .clicked()
                {
                    insert_requested = true;
                }

                if icon_button(
                    ui,
                    IconKind::Copy,
                    has_selection,
                    None,
                    egui::vec2(28.0, 24.0),
                    "Copy Selected Message(s) (Ctrl+C)",
                )
                .clicked()
                {
                    copy_requested = true;
                }

                if icon_button(
                    ui,
                    IconKind::Paste,
                    true,
                    None,
                    egui::vec2(28.0, 24.0),
                    "Paste HL7 message(s) from clipboard",
                )
                .clicked()
                {
                    action = RecordListAction::PasteFromClipboard;
                }

                if icon_button(
                    ui,
                    IconKind::MoveUp,
                    can_move_up,
                    None,
                    egui::vec2(28.0, 24.0),
                    "Move Selected Message(s) Up",
                )
                .clicked()
                {
                    move_up_requested = true;
                }

                if icon_button(
                    ui,
                    IconKind::MoveDown,
                    can_move_down,
                    None,
                    egui::vec2(28.0, 24.0),
                    "Move Selected Message(s) Down",
                )
                .clicked()
                {
                    move_down_requested = true;
                }

                if icon_button(
                    ui,
                    IconKind::Delete,
                    has_selection,
                    Some(palette.accent_red),
                    egui::vec2(28.0, 24.0),
                    "Delete Selected Message(s)",
                )
                .clicked()
                {
                    delete_requested = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if can_reorder {
                        ui.weak(RichText::new("Drag row to reorder").small());
                    }
                });
            });
        });

    // Crisp 1px top border separating the table from the docked bottom toolbar
    let bar_rect = bottom_bar_resp.response.rect;
    ui.painter().line_segment(
        [
            egui::pos2(bar_rect.left(), bar_rect.top()),
            egui::pos2(bar_rect.right(), bar_rect.top()),
        ],
        Stroke::new(1.0_f32, palette.divider),
    );

    let scroll_target_vis_row = if state.scroll_to_record {
        state
            .selected_record
            .and_then(|sel| page_indices.iter().position(|&i| i == sel))
    } else {
        None
    };
    state.scroll_to_record = false;

    let mut dragged_from = state.dragged_record_index;
    let pointer_released = ui.input(|i| i.pointer.any_released());
    let pointer_pos = ui.input(|i| i.pointer.hover_pos());

    egui::CentralPanel::default()
        .frame(Frame::NONE)
        .show_inside(ui, |ui| {
            let clip_rect = ui.available_rect_before_wrap();
            ui.set_clip_rect(clip_rect);
            let table_max_height = ui.available_height().max(40.0);

            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(Sense::click_and_drag())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(52.0).at_least(40.0)) // #
                .column(Column::initial(94.0).at_least(68.0)) // Type (MSH-9 pill badge)
                .column(Column::initial(120.0).at_least(70.0)) // Control ID (MSH-10)
                .column(Column::remainder().at_least(100.0)) // Timestamp (MSH-7)
                .min_scrolled_height(0.0)
                .max_scroll_height(table_max_height);

            if let Some(vis_row) = scroll_target_vis_row {
                table = table.scroll_to_row(vis_row, Some(egui::Align::Center));
            }

            table
                .header(22.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("#");
                    });
                    header.col(|ui| {
                        ui.strong("Type");
                    });
                    header.col(|ui| {
                        ui.strong("Control ID");
                    });
                    header.col(|ui| {
                        ui.strong("Timestamp");
                    });
                })
                .body(|body| {
                    body.rows(22.0, page_len, |mut row| {
                        let page_row_idx = row.index();
                        let doc_idx = page_indices[page_row_idx];
                        let summary = &state.summaries[doc_idx];
                        let is_selected = state.selected_records.contains(&doc_idx);
                        let is_primary = state.selected_record == Some(doc_idx);

                        row.set_selected(is_selected);

                        // Column 1: # (with 3px left accent bar when active)
                        row.col(|ui| {
                            if is_primary {
                                paint_left_accent_bar(ui, ui.max_rect(), palette.accent);
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("{}", doc_idx + 1)).monospace().color(
                                        if is_primary {
                                            palette.primary_text
                                        } else {
                                            palette.muted_text
                                        },
                                    ),
                                )
                                .selectable(false),
                            );
                        });

                        // Column 2: Message Type Pill Badge
                        row.col(|ui| {
                            let badge_label = if summary.message_type.is_empty() {
                                "MSH"
                            } else {
                                summary.message_type.as_str()
                            };
                            let component_sep = state
                                .doc
                                .records
                                .get(doc_idx)
                                .map_or('^', |r| char::from(r.delimiters().component));
                            let pill_colors = message_type_pill_colors_with_delim(
                                badge_label,
                                component_sep,
                                palette.is_dark,
                            );
                            render_pill_badge(ui, badge_label, pill_colors);
                        });

                        // Column 3: Control ID
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&summary.control_id)
                                        .monospace()
                                        .color(palette.primary_text),
                                )
                                .selectable(false),
                            );
                        });

                        // Column 4: Timestamp
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&summary.timestamp)
                                        .monospace()
                                        .color(palette.muted_text),
                                )
                                .selectable(false),
                            );
                        });

                        // Full-row click, right-click context menu, and drag-and-drop hit-testing (zero dead zones, D-29).
                        let row_resp = row.response();
                        if row_resp.clicked() {
                            let (ctrl, shift) = row_resp.ctx.input(|i| {
                                (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift)
                            });
                            clicked_row = Some((doc_idx, ctrl, shift));
                        }
                        if row_resp.secondary_clicked() && !is_selected {
                            clicked_row = Some((doc_idx, false, false));
                        }

                        // Drag-and-drop row reordering (single or multi-selection block) with drop-target line
                        if can_reorder {
                            if row_resp.drag_started() {
                                dragged_from = Some(doc_idx);
                            }
                            if let Some(src_idx) = dragged_from {
                                let dragging_multi = state.selected_records.contains(&src_idx)
                                    && state.selected_records.len() > 1;
                                let is_part_of_drag = if dragging_multi {
                                    state.selected_records.contains(&doc_idx)
                                } else {
                                    doc_idx == src_idx
                                };
                                if let Some(pos) = pointer_pos {
                                    if row_resp.rect.contains(pos) && !is_part_of_drag {
                                        let y = if src_idx > doc_idx {
                                            row_resp.rect.top()
                                        } else {
                                            row_resp.rect.bottom()
                                        };
                                        row_resp.ctx.layer_painter(row_resp.layer_id).line_segment(
                                            [
                                                egui::pos2(row_resp.rect.left(), y),
                                                egui::pos2(row_resp.rect.right(), y),
                                            ],
                                            Stroke::new(2.0_f32, palette.accent),
                                        );
                                        if pointer_released {
                                            drag_drop_pair = Some((src_idx, doc_idx));
                                        }
                                    }
                                }
                            }
                        }

                        row_resp.context_menu(|ui| {
                            let sel_count = state.selected_records.len();
                            if can_compare_selected_records(sel_count) {
                                let label = if sel_count == 1 {
                                    "Compare Against Clipboard / Baseline...".to_string()
                                } else if sel_count == 2 {
                                    let pair: Vec<usize> =
                                        state.selected_records.iter().copied().collect();
                                    format!(
                                        "Compare Selected Messages (#{} vs #{})",
                                        pair[0] + 1,
                                        pair[1] + 1
                                    )
                                } else {
                                    format!("Compare {sel_count} Messages (First as Baseline)")
                                };
                                if icon_menu_button(ui, IconKind::Compare, &label, None, true)
                                    .clicked()
                                {
                                    action = RecordListAction::CompareSelected;
                                    ui.close_menu();
                                }
                                ui.separator();
                            }
                            if icon_menu_button_tinted(
                                ui,
                                IconKind::Add,
                                "Insert New Message",
                                None,
                                true,
                                Some(palette.accent_green),
                            )
                            .clicked()
                            {
                                insert_requested = true;
                                ui.close_menu();
                            }
                            ui.separator();
                            if icon_menu_button(
                                ui,
                                IconKind::MoveUp,
                                "Move Message Up",
                                None,
                                can_move_up,
                            )
                            .clicked()
                            {
                                move_up_requested = true;
                                ui.close_menu();
                            }
                            if icon_menu_button(
                                ui,
                                IconKind::MoveDown,
                                "Move Message Down",
                                None,
                                can_move_down,
                            )
                            .clicked()
                            {
                                move_down_requested = true;
                                ui.close_menu();
                            }
                            ui.separator();
                            if icon_menu_button(
                                ui,
                                IconKind::Copy,
                                "Copy Selected Message(s)",
                                Some("Ctrl+C"),
                                true,
                            )
                            .clicked()
                            {
                                copy_requested = true;
                                ui.close_menu();
                            }
                            if icon_menu_button(
                                ui,
                                IconKind::Save,
                                "Save Selected Messages...",
                                None,
                                true,
                            )
                            .clicked()
                            {
                                action = RecordListAction::SaveSelectedRecords;
                                ui.close_menu();
                            }
                            ui.separator();
                            if icon_menu_button_tinted(
                                ui,
                                IconKind::Delete,
                                "Delete Message(s)",
                                None,
                                true,
                                Some(palette.accent_red),
                            )
                            .clicked()
                            {
                                delete_requested = true;
                                ui.close_menu();
                            }
                        });
                    });
                });
        });

    if pointer_released {
        state.dragged_record_index = None;
    } else {
        state.dragged_record_index = dragged_from;
        if let (Some(src_idx), Some(pos)) = (dragged_from, pointer_pos) {
            let drag_count =
                if state.selected_records.contains(&src_idx) && state.selected_records.len() > 1 {
                    state.selected_records.len()
                } else {
                    1
                };
            let label = if drag_count > 1 {
                format!("Moving {drag_count} messages")
            } else {
                format!("Moving Message #{}", src_idx + 1)
            };
            let layer_id = egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("record_list_drag_ghost"),
            );
            let painter = ui.ctx().layer_painter(layer_id);
            let font = egui::TextStyle::Body.resolve(ui.style());
            let galley = painter.layout_no_wrap(label, font, palette.primary_text);
            let pad = egui::vec2(8.0, 4.0);
            let ghost_rect =
                egui::Rect::from_min_size(pos + egui::vec2(14.0, 10.0), galley.size() + pad * 2.0);
            painter.rect_filled(ghost_rect, CornerRadius::same(6), palette.elevated_bg);
            painter.rect_stroke(
                ghost_rect,
                CornerRadius::same(6),
                Stroke::new(1.0_f32, palette.accent),
                egui::StrokeKind::Middle,
            );
            painter.galley(ghost_rect.min + pad, galley, palette.primary_text);
        }
    }

    if let Some((idx, ctrl_toggle, shift_range)) = clicked_row {
        state.select_record_with_modifiers(idx, ctrl_toggle, shift_range, settings);
    }

    if let Some((from_idx, to_idx)) = drag_drop_pair {
        let _ = state.reorder_record_drag(from_idx, to_idx, settings);
    } else if insert_requested {
        let _ = state.insert_new_scaffold_record(settings);
    } else if move_up_requested {
        let _ = state.move_selected_records_up(settings);
    } else if move_down_requested {
        let _ = state.move_selected_records_down(settings);
    }

    if copy_requested && !state.selected_records.is_empty() {
        let clip_text = state.format_selected_records_for_clipboard(settings);
        ui.ctx().copy_text(clip_text);
        state.status_message = format!(
            "Copied {} message(s) to clipboard.",
            state.selected_records.len()
        );
    }

    if delete_requested && !state.selected_records.is_empty() {
        let indices: Vec<usize> = state.selected_records.iter().copied().collect();
        let _ = state.request_delete_records(indices, settings);
    }

    action
}
