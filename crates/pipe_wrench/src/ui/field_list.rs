//! Field list pane for the selected record (`§5.1`, `§5.2`, `§5.3`, `§5.5`, `G-2`, `NET-4`).

use egui::{Color32, RichText, ScrollArea, Ui};

use crate::config::AppSettings;
use crate::state::SessionState;
use crate::strings;

/// Render the field breakdown pane (`<address> [<description>] <value>`).
pub fn show_field_list(ui: &mut Ui, state: &mut SessionState, settings: &AppSettings) {
    let field_count = state
        .record_cache
        .as_ref()
        .map_or(0, |c| c.field_rows.len());

    ui.horizontal(|ui| {
        ui.heading("Fields");
        ui.label(format!("({field_count} rows)"));
        if settings.decode_escape_sequences {
            ui.colored_label(
                Color32::from_rgb(100, 180, 240),
                "[Escapes Decoded — Read-Only]",
            );
        }
    });

    if let Some(ref cache) = state.record_cache {
        // G-2: Visible indicator when MSH-18 names an unknown charset.
        if cache.charset.is_unknown_fallback {
            let declared = cache.charset.declared.as_deref().unwrap_or("?");
            ui.colored_label(
                Color32::from_rgb(230, 160, 40),
                format!("{}{declared}", strings::BANNER_UNKNOWN_CHARSET_PREFIX),
            );
        }
        // NET-4: Visible warning marker when an MLLP frame lacked a Start Block.
        if cache.missing_start_block {
            ui.colored_label(
                Color32::from_rgb(230, 120, 40),
                strings::BADGE_MISSING_START_BLOCK,
            );
        }
    }
    ui.separator();

    let Some(ref cache) = state.record_cache else {
        ui.weak("No record selected.");
        return;
    };

    let selected_seg = state.selected_segment;
    let selected_row = state.selected_field_row;
    let scroll_to_row = state.scroll_to_field_row;
    let mut clicked_row = None;
    let mut double_clicked_row = None;

    ScrollArea::vertical()
        .id_salt("field_list_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for frow in &cache.field_rows {
                let is_row_selected = selected_row == Some(frow.row_index);
                let is_in_selected_seg = selected_seg == Some(frow.seg_index);

                let addr_str = frow.address.map(|a| a.to_string()).unwrap_or_default();
                let line_text = match &frow.description {
                    Some(desc) => format!("{addr_str} [{desc}] {}", frow.display_value),
                    None => format!("{addr_str} {}", frow.display_value),
                };

                let mut rich = RichText::new(line_text).monospace();
                if is_in_selected_seg && !is_row_selected {
                    rich = rich.color(Color32::from_rgb(130, 210, 255));
                }

                let resp = ui.selectable_label(is_row_selected, rich);
                if is_row_selected && scroll_to_row {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
                if resp.double_clicked() {
                    double_clicked_row = Some(frow.row_index);
                } else if resp.clicked() {
                    clicked_row = Some(frow.row_index);
                }
            }
        });

    state.scroll_to_field_row = false;
    if let Some(row_idx) = double_clicked_row {
        state.select_field_row(row_idx);
        let _ = state.open_edit_field_dialog(settings);
    } else if let Some(row_idx) = clicked_row {
        state.select_field_row(row_idx);
    }
}
