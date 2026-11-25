//! Segment list pane for the selected record (`§5.1`, `§5.3`, `§5.4`).

use egui::{RichText, ScrollArea, Ui};

use crate::state::SessionState;

/// Render the segment list pane for the currently selected record.
pub fn show_segment_list(ui: &mut Ui, state: &mut SessionState) {
    let seg_count = state.record_cache.as_ref().map_or(0, |c| c.segments.len());

    ui.horizontal(|ui| {
        ui.heading("Segments");
        ui.label(format!("({seg_count})"));
    });
    ui.separator();

    let Some(ref cache) = state.record_cache else {
        ui.weak("No record selected.");
        return;
    };

    let selected_seg = state.selected_segment;
    let scroll_to_seg = state.scroll_to_segment;
    let mut clicked_seg = None;

    ScrollArea::vertical()
        .id_salt("segment_list_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for seg in &cache.segments {
                let is_selected = selected_seg == Some(seg.seg_index);
                let label = match (&seg.description, seg.occurrence > 1) {
                    (Some(desc), true) => {
                        format!(
                            "{:>2}. {}({}) — {}",
                            seg.seg_index + 1,
                            seg.tag,
                            seg.occurrence,
                            desc
                        )
                    }
                    (Some(desc), false) => {
                        format!("{:>2}. {} — {}", seg.seg_index + 1, seg.tag, desc)
                    }
                    (None, true) => {
                        format!("{:>2}. {}({})", seg.seg_index + 1, seg.tag, seg.occurrence)
                    }
                    (None, false) => format!("{:>2}. {}", seg.seg_index + 1, seg.tag),
                };

                let resp = ui.selectable_label(is_selected, RichText::new(label).monospace());
                if is_selected && scroll_to_seg {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
                if resp.clicked() {
                    clicked_seg = Some(seg.seg_index);
                }
            }
        });

    state.scroll_to_segment = false;
    if let Some(seg_idx) = clicked_seg {
        state.select_segment(seg_idx);
    }
}
