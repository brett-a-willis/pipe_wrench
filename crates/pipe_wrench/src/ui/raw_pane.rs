//! Synchronized Raw pane (`§5.1`) with bidirectional segment/field highlighting.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, ScrollArea, TextEdit, Ui};

use crate::state::SessionState;

/// Render the synchronized raw record text pane (§5.1).
pub fn show_raw_pane(ui: &mut Ui, state: &mut SessionState) {
    ui.horizontal(|ui| {
        ui.heading("Raw Record");
        if let Some(ref cache) = state.record_cache {
            ui.weak(format!("Encoding: {}", cache.charset.encoding.name()));
        }
    });
    ui.separator();

    let Some(ref cache) = state.record_cache else {
        ui.weak("No record selected.");
        return;
    };

    let seg_range: Option<Range<usize>> = state
        .selected_segment
        .and_then(|s| cache.segments.get(s))
        .map(|s| s.raw_text_range.clone());

    let field_range: Option<Range<usize>> = state
        .selected_field_row
        .and_then(|r| cache.field_rows.get(r))
        .filter(|f| !f.is_parsed_marker)
        .map(|f| f.raw_text_range.clone());

    let raw_clone = cache.raw_text.clone();
    let mut mutable_view = raw_clone.as_str();

    let mut layouter = |ui: &Ui, text: &str, wrap_width: f32| {
        let job = build_highlighted_layout_job(
            ui,
            text,
            wrap_width,
            seg_range.clone(),
            field_range.clone(),
        );
        ui.fonts(|f| f.layout_job(job))
    };

    let mut clicked_cursor_offset = None;

    ScrollArea::vertical()
        .id_salt("raw_pane_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let output = TextEdit::multiline(&mut mutable_view)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter)
                .show(ui);

            if output.response.clicked() {
                if let Some(cursor_range) = output.cursor_range {
                    let char_idx = cursor_range.primary.ccursor.index;
                    // Convert char index to byte offset in raw_clone
                    let byte_offset = raw_clone
                        .char_indices()
                        .nth(char_idx)
                        .map_or(raw_clone.len(), |(b, _)| b);
                    clicked_cursor_offset = Some(byte_offset);
                }
            }
        });

    if let Some(byte_offset) = clicked_cursor_offset {
        state.select_from_raw_offset(byte_offset);
    }
}

fn build_highlighted_layout_job(
    ui: &Ui,
    text: &str,
    wrap_width: f32,
    seg_range: Option<Range<usize>>,
    field_range: Option<Range<usize>>,
) -> LayoutJob {
    let font_id = FontId::monospace(13.0);
    let base_color = ui.visuals().text_color();

    let normal_fmt = TextFormat {
        font_id: font_id.clone(),
        color: base_color,
        ..Default::default()
    };
    let seg_fmt = TextFormat {
        font_id: font_id.clone(),
        color: base_color,
        background: Color32::from_rgba_unmultiplied(60, 110, 170, 55),
        ..Default::default()
    };
    let field_fmt = TextFormat {
        font_id,
        color: Color32::WHITE,
        background: Color32::from_rgb(200, 110, 20),
        ..Default::default()
    };

    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;

    let Some(s_range) = seg_range else {
        job.append(text, 0.0, normal_fmt);
        return job;
    };

    let s_start = s_range.start.min(text.len());
    let s_end = s_range.end.min(text.len());

    if s_start > 0 {
        job.append(&text[..s_start], 0.0, normal_fmt.clone());
    }

    if let Some(f_range) = field_range {
        let f_start = f_range.start.clamp(s_start, s_end);
        let f_end = f_range.end.clamp(f_start, s_end);

        if f_start > s_start {
            job.append(&text[s_start..f_start], 0.0, seg_fmt.clone());
        }
        if f_end > f_start {
            job.append(&text[f_start..f_end], 0.0, field_fmt);
        }
        if s_end > f_end {
            job.append(&text[f_end..s_end], 0.0, seg_fmt);
        }
    } else if s_end > s_start {
        job.append(&text[s_start..s_end], 0.0, seg_fmt);
    }

    if s_end < text.len() {
        job.append(&text[s_end..], 0.0, normal_fmt);
    }

    job
}
