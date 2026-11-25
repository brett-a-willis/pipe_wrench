//! Unified Record Detail Area (`§5.1`, `§5.2`, `EDIT-2`, `EDIT-3`, `D-26`).
//!
//! Replaces separate `Segments`, `Fields`, and bottom `Raw Record` panes with:
//! 1. **In-Place Dual Dark/Light (`VS Code Dark+` / `VS Code Light+`) Raw Message Editor:**
//!    Directly editable multiline editor driven by [`EditorPalette::for_visuals`] (`#1E1E1E` in Dark
//!    mode, `#FFFFFF` in Light mode) with active-line highlight and dynamic syntax colors for
//!    segment tags (`MSH`, `PID`, `OBX`, ...) and each active delimiter (`field`, `component`,
//!    `repetition`, `escape`, `subcomponent`), committing edits via [`EditRecordCmd`] onto [`UndoStack`].
//! 2. **Expandable Segment Accordion & Aligned 3-Column Field Breakdown:** Clickable segment
//!    header rows with scannable pill badges and `3px` left accent indicators that expand/collapse
//!    inline into a 3-column aligned layout (`Address` | `[Description]` | `Value`).
//! 3. **Empty-State Dropzone Card:** Clean centered card (`Open File...`, `Paste HL7 from Clipboard`,
//!    `Start TCP/IP Listener`) when no records are loaded.

use std::num::NonZeroU16;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, FontId, Frame, Margin, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};
use egui_extras::{Column, TableBuilder};
use hl7_core::{Delimiters, FieldAddress, SourceTerminator};

use crate::config::AppSettings;
use crate::state::{DetailViewMode, SessionState};
use crate::strings;
use crate::ui::icons::{icon_menu_button, icon_menu_button_tinted, paint_icon, IconKind};
use crate::ui::theme::{
    add_action_button_job, paint_left_accent_bar, render_pill_badge, segment_tag_pill_colors,
    EditorPalette, ThemePalette,
};

/// Resolve the 0-based segment index and optional canonical leaf [`FieldAddress`]
/// (`SEG-F[(R)][.C[.S]]`) at `byte_offset` inside a raw HL7 message text buffer using
/// the message's active [`Delimiters`] (`D-30`).
///
/// Honors `MSH-1`/`MSH-2` (and `FHS`/`BHS`) header indexing rules and matches the exact
/// canonical path format emitted by `Record::field_entries` (omitting `(1)` when a field
/// has only a single repetition, adding `.C` when components exist, and adding `.S` when
/// subcomponents exist).
pub fn resolve_raw_cursor_location(
    raw_text: &str,
    byte_offset: usize,
    delims: Delimiters,
) -> Option<(usize, Option<FieldAddress>)> {
    if raw_text.is_empty() {
        return None;
    }
    let clamped = byte_offset.min(raw_text.len());
    let bytes = raw_text.as_bytes();

    // Locate the 0-based segment line containing `clamped`.
    let mut seg_index = 0usize;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let mut line_end = pos;
        while line_end < bytes.len() && bytes[line_end] != b'\n' && bytes[line_end] != b'\r' {
            line_end += 1;
        }
        let next_pos = if line_end + 1 < bytes.len()
            && bytes[line_end] == b'\r'
            && bytes[line_end + 1] == b'\n'
        {
            line_end + 2
        } else if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };

        if clamped <= line_end || next_pos >= bytes.len() {
            let seg_bytes = &bytes[pos..line_end];
            let col = clamped.saturating_sub(pos).min(seg_bytes.len());
            let addr = resolve_segment_cursor_address(seg_bytes, col, delims);
            return Some((seg_index, addr));
        }

        if line_end > pos {
            seg_index += 1;
        }
        pos = next_pos;
    }

    None
}

/// Convenience wrapper returning `(seg_index, FieldAddress)` when the cursor lies inside a field (`D-30`).
pub fn resolve_raw_cursor_field_path(
    raw_text: &str,
    byte_offset: usize,
    delims: Delimiters,
) -> Option<(usize, FieldAddress)> {
    let (seg_idx, addr_opt) = resolve_raw_cursor_location(raw_text, byte_offset, delims)?;
    Some((seg_idx, addr_opt?))
}

fn resolve_segment_cursor_address(
    seg: &[u8],
    col: usize,
    delims: Delimiters,
) -> Option<FieldAddress> {
    if seg.len() < 3 || !seg[0..3].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    let tag = [seg[0], seg[1], seg[2]];
    let is_header = tag == *b"MSH" || tag == *b"FHS" || tag == *b"BHS";

    if is_header {
        if col < 3 {
            return None;
        }
        if col == 3 {
            return Some(FieldAddress {
                segment: tag,
                field: NonZeroU16::new(1)?,
                repetition: None,
                component: None,
                subcomponent: None,
            });
        }
        if (4..=8).contains(&col) && seg.len() >= 8 {
            return Some(FieldAddress {
                segment: tag,
                field: NonZeroU16::new(2)?,
                repetition: None,
                component: None,
                subcomponent: None,
            });
        }
        if seg.len() <= 8 || seg[8] != delims.field {
            return None;
        }
        return resolve_field_in_slice(tag, &seg[9..], col.saturating_sub(9), 3, delims);
    }

    if col <= 3 || seg.len() < 4 || seg[3] != delims.field {
        return None;
    }
    resolve_field_in_slice(tag, &seg[4..], col.saturating_sub(4), 1, delims)
}

fn resolve_field_in_slice(
    tag: [u8; 3],
    fields_slice: &[u8],
    rel_col: usize,
    first_field_num: u16,
    delims: Delimiters,
) -> Option<FieldAddress> {
    let col = rel_col.min(fields_slice.len());

    // Locate the field byte range containing `col`.
    let mut field_num = first_field_num;
    let mut field_start = 0usize;
    let mut field_end = fields_slice.len();
    for (idx, &b) in fields_slice.iter().enumerate() {
        if b == delims.field {
            if col <= idx {
                field_end = idx;
                break;
            }
            field_num = field_num.saturating_add(1);
            field_start = idx + 1;
        }
    }

    let field_bytes = &fields_slice[field_start..field_end];
    let field_rel = col.saturating_sub(field_start).min(field_bytes.len());
    let field = NonZeroU16::new(field_num)?;

    // 1. Repetitions (`~`)
    let has_rep = field_bytes.contains(&delims.repetition);
    let mut rep_num: u16 = 1;
    let mut rep_start = 0usize;
    let mut rep_end = field_bytes.len();
    if has_rep {
        for (idx, &b) in field_bytes.iter().enumerate() {
            if b == delims.repetition {
                if field_rel <= idx {
                    rep_end = idx;
                    break;
                }
                rep_num = rep_num.saturating_add(1);
                rep_start = idx + 1;
            }
        }
    }
    let repetition = if has_rep {
        NonZeroU16::new(rep_num)
    } else {
        None
    };

    let rep_bytes = &field_bytes[rep_start..rep_end];
    let rep_rel = field_rel.saturating_sub(rep_start).min(rep_bytes.len());

    // 2. Components (`^`)
    let rep_has_comp = rep_bytes.contains(&delims.component);
    let rep_has_sub = rep_bytes.contains(&delims.subcomponent);
    if !rep_has_comp && !rep_has_sub {
        return Some(FieldAddress {
            segment: tag,
            field,
            repetition,
            component: None,
            subcomponent: None,
        });
    }

    let mut comp_num: u16 = 1;
    let mut comp_start = 0usize;
    let mut comp_end = rep_bytes.len();
    if rep_has_comp {
        for (idx, &b) in rep_bytes.iter().enumerate() {
            if b == delims.component {
                if rep_rel <= idx {
                    comp_end = idx;
                    break;
                }
                comp_num = comp_num.saturating_add(1);
                comp_start = idx + 1;
            }
        }
    }
    let component = NonZeroU16::new(comp_num);

    let comp_bytes = &rep_bytes[comp_start..comp_end];
    let comp_rel = rep_rel.saturating_sub(comp_start).min(comp_bytes.len());

    // 3. Subcomponents (`&`)
    let comp_has_sub = comp_bytes.contains(&delims.subcomponent);
    if !comp_has_sub {
        return Some(FieldAddress {
            segment: tag,
            field,
            repetition,
            component,
            subcomponent: None,
        });
    }

    let mut sub_num: u16 = 1;
    for (idx, &b) in comp_bytes.iter().enumerate() {
        if b == delims.subcomponent {
            if comp_rel <= idx {
                break;
            }
            sub_num = sub_num.saturating_add(1);
        }
    }

    Some(FieldAddress {
        segment: tag,
        field,
        repetition,
        component,
        subcomponent: NonZeroU16::new(sub_num),
    })
}

/// Actions requested from the empty-state dropzone card in the Record Detail Area (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPaneAction {
    /// No external action requested.
    None,
    /// Open File dialog requested (`Ctrl+O`).
    OpenFiles,
    /// Paste HL7 from clipboard requested.
    PasteFromClipboard,
    /// Open the docked TCP/IP Listener tab.
    StartListener,
}

/// Classification of syntax tokens in the raw HL7 editor (`D-26`, `D-28`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxSpanKind {
    /// 3-character segment tag at the start of a line (`MSH`, `PID`, `OBX`, ...).
    SegmentTag,
    /// Active field separator byte (default `|`).
    FieldDelimiter,
    /// Active component separator byte (default `^`).
    ComponentDelimiter,
    /// Active repetition separator byte (default `~`).
    RepetitionDelimiter,
    /// Active escape character byte (default `\`).
    EscapeDelimiter,
    /// Complete HL7 escape sequence (`\F\`, `\S\`, `\T\`, `\R\`, `\E\`, `\H\`, `\N\`, `\.br\`, `\X0D\`, `\Z..\`) (`D-28`).
    EscapeSequence,
    /// Active subcomponent separator byte (default `&`).
    SubcomponentDelimiter,
    /// Normal field value text or newline.
    Text,
}

impl SyntaxSpanKind {
    /// Resolve foreground color from the active [`EditorPalette`] (`VS Code Dark+` or `VS Code Light+`).
    pub fn color_in(self, palette: &EditorPalette) -> Color32 {
        match self {
            Self::SegmentTag => palette.segment_tag,
            Self::FieldDelimiter => palette.delim_field,
            Self::ComponentDelimiter => palette.delim_component,
            Self::RepetitionDelimiter => palette.delim_repetition,
            Self::EscapeDelimiter => palette.delim_escape,
            Self::EscapeSequence => palette.escape_sequence,
            Self::SubcomponentDelimiter => palette.delim_subcomponent,
            Self::Text => palette.text_fg,
        }
    }
}

/// One contiguous syntax-highlighted span in a raw HL7 message string (`D-26`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSpan {
    /// Byte range within the text.
    pub range: std::ops::Range<usize>,
    /// Token kind.
    pub kind: SyntaxSpanKind,
    /// `true` if this span falls on the active segment line.
    pub on_active_line: bool,
}

/// Tokenize `text` into [`SyntaxSpan`]s using the record's active [`Delimiters`] (`D-26`).
pub fn tokenize_hl7_syntax(
    text: &str,
    delims: Delimiters,
    active_segment_line: Option<usize>,
) -> Vec<SyntaxSpan> {
    if text.is_empty() {
        return Vec::new();
    }

    let bytes = text.as_bytes();
    let mut spans: Vec<SyntaxSpan> = Vec::new();
    let mut line_idx = 0usize;
    let mut line_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let on_active = active_segment_line == Some(line_idx);
        let b = bytes[i];

        if b == b'\n' {
            push_or_merge_span(&mut spans, i..i + 1, SyntaxSpanKind::Text, on_active);
            i += 1;
            line_idx += 1;
            line_start = i;
            continue;
        }

        // 3-character ASCII segment tag at the beginning of a line followed by field delimiter or EOL
        if i == line_start
            && i + 3 <= bytes.len()
            && bytes[i..i + 3].iter().all(|c| c.is_ascii_alphanumeric())
            && (i + 3 == bytes.len()
                || bytes[i + 3] == delims.field
                || bytes[i + 3] == b'\n'
                || bytes[i + 3] == b'\r')
        {
            push_or_merge_span(&mut spans, i..i + 3, SyntaxSpanKind::SegmentTag, on_active);
            i += 3;
            continue;
        }

        // Recognize complete HL7 escape sequences (\F\, \S\, \T\, \R\, \E\, \H\, \N\, \.br\, \X..\, \Z..\)
        // outside of MSH-2 / FHS-2 / BHS-2 header encoding characters (D-28).
        if b == delims.escape {
            let is_header_encoding_pos = i >= line_start + 4
                && i < line_start + 8
                && line_start + 3 <= bytes.len()
                && (&bytes[line_start..line_start + 3] == b"MSH"
                    || &bytes[line_start..line_start + 3] == b"FHS"
                    || &bytes[line_start..line_start + 3] == b"BHS");
            if !is_header_encoding_pos {
                if let Some(end_idx) = match_hl7_escape_sequence(bytes, i, delims) {
                    push_or_merge_span(
                        &mut spans,
                        i..end_idx,
                        SyntaxSpanKind::EscapeSequence,
                        on_active,
                    );
                    i = end_idx;
                    continue;
                }
            }
        }

        let kind = if b == delims.field {
            SyntaxSpanKind::FieldDelimiter
        } else if b == delims.component {
            SyntaxSpanKind::ComponentDelimiter
        } else if b == delims.repetition {
            SyntaxSpanKind::RepetitionDelimiter
        } else if b == delims.escape {
            SyntaxSpanKind::EscapeDelimiter
        } else if b == delims.subcomponent {
            SyntaxSpanKind::SubcomponentDelimiter
        } else {
            SyntaxSpanKind::Text
        };

        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        push_or_merge_span(&mut spans, i..i + ch_len, kind, on_active);
        i += ch_len;
    }

    spans
}

fn match_hl7_escape_sequence(bytes: &[u8], start: usize, delims: Delimiters) -> Option<usize> {
    let esc = delims.escape;
    if bytes.get(start) != Some(&esc) {
        return None;
    }
    let mut j = start + 1;
    while j < bytes.len() {
        let c = bytes[j];
        if c == esc {
            let inner = &bytes[start + 1..j];
            if is_valid_hl7_escape_body(inner) {
                return Some(j + 1);
            }
            return None;
        }
        if c == b'\r' || c == b'\n' || c == delims.field || (j - start) > 64 {
            return None;
        }
        j += 1;
    }
    None
}

fn is_valid_hl7_escape_body(inner: &[u8]) -> bool {
    if inner.is_empty() {
        return false;
    }
    if inner.len() == 1 {
        return matches!(inner[0], b'F' | b'S' | b'T' | b'R' | b'E' | b'H' | b'N');
    }
    if inner[0] == b'.' {
        return inner[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'-');
    }
    if matches!(inner[0], b'X' | b'C' | b'M') {
        return inner[1..].iter().all(u8::is_ascii_hexdigit);
    }
    if inner[0] == b'Z' {
        return inner[1..].iter().all(|b| b.is_ascii_graphic());
    }
    false
}

fn push_or_merge_span(
    spans: &mut Vec<SyntaxSpan>,
    range: std::ops::Range<usize>,
    kind: SyntaxSpanKind,
    on_active_line: bool,
) {
    if let Some(last) = spans.last_mut() {
        if last.kind == kind
            && last.on_active_line == on_active_line
            && last.range.end == range.start
        {
            last.range.end = range.end;
            return;
        }
    }
    spans.push(SyntaxSpan {
        range,
        kind,
        on_active_line,
    });
}

/// Build an `egui::text::LayoutJob` using the provided [`EditorPalette`] (`VS Code Dark+` or `VS Code Light+`)
/// and dynamic delimiter + HL7 escape sequence syntax highlighting (`D-26`, `D-28`).
pub fn build_editor_layout_job(
    text: &str,
    wrap_width: f32,
    delims: Delimiters,
    active_segment_line: Option<usize>,
    palette: &EditorPalette,
) -> LayoutJob {
    build_editor_layout_job_with_controls(
        text,
        wrap_width,
        delims,
        active_segment_line,
        palette,
        false,
    )
}

/// Return the Notepad++-style end-of-line badge label for a record's [`SourceTerminator`] (`D-28`).
pub fn eol_badge_for_terminator(term: SourceTerminator) -> &'static str {
    match term {
        SourceTerminator::Cr | SourceTerminator::Unknown => "[CR]",
        SourceTerminator::CrLf => "[CR][LF]",
        SourceTerminator::Lf => "[LF]",
    }
}

/// Compute non-intrusive visual overlay badges (`(line_index, label)`) for control characters
/// (`[CR]`, `[LF]`, `[CR][LF]`, `[VT]`, `[FS]`) without mutating or expanding the `TextEdit`
/// character buffer (`D-28`).
pub fn control_char_overlay_badges(
    text: &str,
    source_term: SourceTerminator,
) -> Vec<(usize, &'static str)> {
    let mut badges = Vec::new();
    let bytes = text.as_bytes();
    let mut line_idx = 0usize;
    let mut i = 0usize;
    let mut line_has_content = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x0B {
            badges.push((line_idx, "[VT]"));
            line_has_content = true;
            i += 1;
        } else if b == 0x1C {
            badges.push((line_idx, "[FS]"));
            line_has_content = true;
            i += 1;
        } else if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            badges.push((line_idx, "[CR][LF]"));
            line_idx += 1;
            line_has_content = false;
            i += 2;
        } else if b == b'\r' {
            badges.push((line_idx, "[CR]"));
            line_idx += 1;
            line_has_content = false;
            i += 1;
        } else if b == b'\n' {
            badges.push((line_idx, eol_badge_for_terminator(source_term)));
            line_idx += 1;
            line_has_content = false;
            i += 1;
        } else {
            line_has_content = true;
            i += 1;
        }
    }
    if line_has_content {
        badges.push((line_idx, eol_badge_for_terminator(source_term)));
    }
    badges
}

/// Build an `egui::text::LayoutJob` with optional 1:1 cursor-safe inline whitespace/control markers (`¶`):
/// `' '` → `·`, `'\t'` → `→`, `\x0B` → `␋`, `\x1C` → `␜`.
///
/// **Invariant:** Every `char` in `text` maps to **exactly 1 `char`** in `job.text`
/// (`job.text.chars().count() == text.chars().count()`), guaranteeing that `egui::TextEdit`'s
/// `CCursor` / text-insertion indices never drift. Multi-character end-of-line badges (`[CR]`,
/// `[LF]`, `[CR][LF]`) are painted as galley overlays outside `job.text` (`D-28`).
pub fn build_editor_layout_job_with_controls(
    text: &str,
    wrap_width: f32,
    delims: Delimiters,
    active_segment_line: Option<usize>,
    palette: &EditorPalette,
    show_control_chars: bool,
) -> LayoutJob {
    let font_id = FontId::monospace(13.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;

    for span in tokenize_hl7_syntax(text, delims, active_segment_line) {
        let bg = if span.on_active_line {
            palette.active_line_bg
        } else {
            Color32::TRANSPARENT
        };
        let fg = span.kind.color_in(palette);
        let slice = &text[span.range];

        if !show_control_chars {
            let fmt = TextFormat {
                font_id: font_id.clone(),
                color: fg,
                background: bg,
                ..Default::default()
            };
            job.append(slice, 0.0, fmt);
            continue;
        }

        let ctrl_badge_fmt = TextFormat {
            font_id: font_id.clone(),
            color: palette.control_char_fg,
            background: palette.control_char_bg,
            ..Default::default()
        };
        let ws_fmt = TextFormat {
            font_id: font_id.clone(),
            color: palette.control_char_fg,
            background: bg,
            ..Default::default()
        };
        let normal_fmt = TextFormat {
            font_id: font_id.clone(),
            color: fg,
            background: bg,
            ..Default::default()
        };

        // Strict 1-char-to-1-char substitution so `CCursor` indices in `TextEdit` never shift!
        let mut run_start = 0usize;
        for (idx, ch) in slice.char_indices() {
            let replacement: Option<(&str, bool)> = match ch {
                ' ' => Some(("·", false)),
                '\t' => Some(("→", false)),
                '\x0B' => Some(("␋", true)),
                '\x1C' => Some(("␜", true)),
                _ => None,
            };
            if let Some((sym, is_ctrl)) = replacement {
                if idx > run_start {
                    job.append(&slice[run_start..idx], 0.0, normal_fmt.clone());
                }
                job.append(
                    sym,
                    0.0,
                    if is_ctrl {
                        ctrl_badge_fmt.clone()
                    } else {
                        ws_fmt.clone()
                    },
                );
                run_start = idx + ch.len_utf8();
            }
        }
        if run_start < slice.len() {
            job.append(&slice[run_start..], 0.0, normal_fmt);
        }
    }

    job
}

/// Render the Unified Record Detail Area (`§5.1`, `§5.2`, `D-26`, `D-28`).
pub fn show_record_detail_area(
    ui: &mut Ui,
    state: &mut SessionState,
    settings: &mut AppSettings,
) -> DetailPaneAction {
    let theme_palette = ThemePalette::for_visuals(ui.visuals());

    // Top bar inside Detail Area: segmented view mode control + active message metadata + badges
    ui.horizontal(|ui| {
        ui.heading("Message Detail");
        ui.separator();

        // Segmented mode switch control
        Frame::NONE
            .fill(theme_palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, theme_palette.divider))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(4, 2))
            .show(ui, |ui| {
                ui.selectable_value(
                    &mut state.detail_view_mode,
                    DetailViewMode::Split,
                    "Split View",
                );
                ui.selectable_value(
                    &mut state.detail_view_mode,
                    DetailViewMode::RawOnly,
                    "Raw Editor",
                );
                ui.selectable_value(
                    &mut state.detail_view_mode,
                    DetailViewMode::AccordionOnly,
                    "Segment Accordion",
                );
            });

        if let Some(ref cache) = state.record_cache {
            ui.separator();
            ui.weak(format!("Encoding: {}", cache.charset.encoding.name()));
            if cache.charset.is_unknown_fallback {
                let declared = cache.charset.declared.as_deref().unwrap_or("?");
                ui.colored_label(
                    Color32::from_rgb(230, 160, 40),
                    format!("{}{declared}", strings::BANNER_UNKNOWN_CHARSET_PREFIX),
                );
            }
            if cache.missing_start_block {
                ui.colored_label(
                    Color32::from_rgb(230, 120, 40),
                    strings::BADGE_MISSING_START_BLOCK,
                );
            }
        }
        if settings.decode_escape_sequences {
            ui.colored_label(theme_palette.accent, "[Escapes Decoded — Read-Only]");
        }
    });
    ui.separator();

    if state.record_cache.is_none() {
        return show_empty_state_dropzone_card(ui, &theme_palette);
    }

    match state.detail_view_mode {
        DetailViewMode::RawOnly => {
            show_raw_editor(ui, state, settings, ui.available_height());
        }
        DetailViewMode::AccordionOnly => {
            show_segment_accordion(ui, state, settings, ui.available_height());
        }
        DetailViewMode::Split => {
            let total_h = ui.available_height();
            let raw_h = state
                .detail_raw_split_height
                .clamp(90.0, (total_h - 110.0).max(90.0));
            show_raw_editor(ui, state, settings, raw_h);

            // Draggable horizontal splitter bar between Raw Editor and Segment Accordion
            let (sep_rect, sep_resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), Sense::drag());
            let sep_color = if sep_resp.hovered() || sep_resp.dragged() {
                theme_palette.accent
            } else {
                theme_palette.divider
            };
            ui.painter().line_segment(
                [
                    egui::pos2(sep_rect.left(), sep_rect.center().y),
                    egui::pos2(sep_rect.right(), sep_rect.center().y),
                ],
                Stroke::new(1.5_f32, sep_color),
            );
            if sep_resp.dragged() {
                state.detail_raw_split_height = (state.detail_raw_split_height
                    + sep_resp.drag_delta().y)
                    .clamp(90.0, (total_h - 110.0).max(90.0));
            }

            let rem_h = ui.available_height();
            show_segment_accordion(ui, state, settings, rem_h);
        }
    }

    DetailPaneAction::None
}

fn show_empty_state_dropzone_card(ui: &mut Ui, palette: &ThemePalette) -> DetailPaneAction {
    let mut action = DetailPaneAction::None;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.18).max(24.0));
        Frame::NONE
            .fill(palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(28, 24))
            .show(ui, |ui| {
                ui.set_max_width(460.0);
                ui.vertical_centered(|ui| {
                    ui.heading("No Messages Loaded");
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Open an HL7 file (.hl7, .txt, .dat, .gz, .zip), paste messages from the clipboard, or start the MLLP / TLS 1.2/1.3 listener.",
                        )
                        .color(palette.muted_text),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("📂 Open File(s)... (Ctrl+O)").strong())
                            .clicked()
                        {
                            action = DetailPaneAction::OpenFiles;
                        }
                        if ui.button("📋 Paste HL7").clicked() {
                            action = DetailPaneAction::PasteFromClipboard;
                        }
                        if ui.button("🖧 Start Listener").clicked() {
                            action = DetailPaneAction::StartListener;
                        }
                    });
                });
            });
    });
    action
}

fn show_raw_editor(ui: &mut Ui, state: &mut SessionState, settings: &mut AppSettings, height: f32) {
    let Some(rec_idx) = state.selected_record else {
        return;
    };
    let _ = state.poll_inline_raw_debounce(settings);
    if state.pending_inline_raw_edit.is_some() {
        ui.ctx()
            .request_repaint_after(crate::state::INLINE_RAW_DEBOUNCE_DURATION);
    }

    let (delims, source_term) = state
        .doc
        .records
        .get(rec_idx)
        .map_or((Delimiters::STANDARD, SourceTerminator::Cr), |r| {
            (r.delimiters(), r.source_terminator())
        });
    let active_seg = state.selected_segment;
    let editor_palette = EditorPalette::for_visuals(ui.visuals());

    let can_revert = state.pending_inline_raw_edit.is_some()
        || state
            .record_cache
            .as_ref()
            .is_some_and(|c| c.raw_text != state.inline_raw_text);

    let line_break_tooltip = match source_term {
        SourceTerminator::Cr | SourceTerminator::Unknown => {
            "Insert segment line break using active terminator (CR / \\r)"
        }
        SourceTerminator::Lf => "Insert segment line break using active terminator (LF / \\n)",
        SourceTerminator::CrLf => {
            "Insert segment line break using active terminator (CRLF / \\r\\n)"
        }
    };

    let theme_palette = ThemePalette::for_visuals(ui.visuals());
    let delim_font = FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size);
    let delim_job = build_delimiters_pill_job(delims, delim_font, &editor_palette);
    let narrow_header = ui.available_width() < 350.0;

    let mut do_insert_line_break = false;
    let mut do_revert = false;
    let mut do_toggle_controls = false;

    let render_context = |ui: &mut Ui, delim_job: LayoutJob| {
        ui.strong("Raw Message Editor");
        ui.separator();
        Frame::NONE
            .fill(theme_palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, theme_palette.divider))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(7, 2))
            .show(ui, |ui| {
                ui.set_min_height(SUBHEADER_CONTROL_HEIGHT - 6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(
                        RichText::new("Delimiters:")
                            .small()
                            .color(theme_palette.muted_text),
                    );
                    ui.label(delim_job);
                });
            });
    };

    if narrow_header {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            render_context(ui, delim_job);
            ui.separator();
            if subheader_icon_button(
                ui,
                IconKind::InsertLineBreak,
                true,
                Some(theme_palette.accent_green),
                line_break_tooltip,
                &theme_palette,
            )
            .clicked()
            {
                do_insert_line_break = true;
            }
            if subheader_icon_button(
                ui,
                IconKind::Revert,
                can_revert,
                None,
                "Revert uncommitted raw editor changes",
                &theme_palette,
            )
            .clicked()
            {
                do_revert = true;
            }
            ui.separator();
            if subheader_square_toggle_button(
                ui,
                settings.show_control_chars,
                true,
                RichText::new("¶").strong(),
                &theme_palette,
            )
            .on_hover_text("Show Line Endings & Control Characters ([CR], [LF], [VT], [FS], ·, →)")
            .clicked()
            {
                do_toggle_controls = true;
            }
        });
    } else {
        ui.horizontal(|ui| {
            render_context(ui, delim_job);

            // Right Group (Editing Actions | Display Toggle), laid out right-to-left so
            // visually left-to-right they read: [↵] [↺] | [¶]
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;

                if subheader_square_toggle_button(
                    ui,
                    settings.show_control_chars,
                    true,
                    RichText::new("¶").strong(),
                    &theme_palette,
                )
                .on_hover_text(
                    "Show Line Endings & Control Characters ([CR], [LF], [VT], [FS], ·, →)",
                )
                .clicked()
                {
                    do_toggle_controls = true;
                }

                ui.separator();

                if subheader_icon_button(
                    ui,
                    IconKind::Revert,
                    can_revert,
                    None,
                    "Revert uncommitted raw editor changes",
                    &theme_palette,
                )
                .clicked()
                {
                    do_revert = true;
                }

                if subheader_icon_button(
                    ui,
                    IconKind::InsertLineBreak,
                    true,
                    Some(theme_palette.accent_green),
                    line_break_tooltip,
                    &theme_palette,
                )
                .clicked()
                {
                    do_insert_line_break = true;
                }
            });
        });
    }

    if do_insert_line_break {
        state.insert_eos_in_inline_raw();
        state.apply_live_inline_raw_edit(settings);
    }
    if do_revert {
        state.revert_inline_raw_edit_with_settings(settings);
    }
    if do_toggle_controls {
        settings.show_control_chars = !settings.show_control_chars;
        settings.save_default();
    }

    let mut text_changed = false;
    let mut lost_focus = false;
    let prev_cursor_pos = state.inline_raw_cursor;
    let mut sync_cursor_offset = None;
    let mut new_cursor_pos = state.inline_raw_cursor;
    let show_ctrl = settings.show_control_chars;

    Frame::NONE
        .fill(editor_palette.background)
        .inner_margin(Margin::same(6))
        .corner_radius(CornerRadius::same(6))
        .stroke(Stroke::new(1.0_f32, editor_palette.border))
        .show(ui, |ui| {
            ui.set_max_height(height.saturating_sub_f32(28.0));
            ui.visuals_mut().selection.bg_fill = editor_palette.selection_bg;
            ui.visuals_mut().selection.stroke = Stroke::new(1.0_f32, editor_palette.text_fg);
            ui.visuals_mut().extreme_bg_color = editor_palette.background;

            let mut layouter = |ui: &Ui, text: &str, wrap_width: f32| {
                let job = build_editor_layout_job_with_controls(
                    text,
                    wrap_width,
                    delims,
                    active_seg,
                    &editor_palette,
                    show_ctrl,
                );
                ui.fonts(|f| f.layout_job(job))
            };

            ScrollArea::vertical()
                .id_salt("vscode_inline_raw_scroll")
                .max_height(height.saturating_sub_f32(34.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let output = TextEdit::multiline(&mut state.inline_raw_text)
                        .id_salt("vscode_inline_raw_textedit")
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6)
                        .frame(false)
                        .layouter(&mut layouter)
                        .show(ui);

                    // Paint non-intrusive end-of-line control-character badges ([CR], [LF], [CR][LF])
                    // as galley overlays so TextEdit's character/cursor indices remain strictly 1:1 (D-28).
                    if show_ctrl {
                        let source_term = state
                            .doc
                            .records
                            .get(rec_idx)
                            .map_or(SourceTerminator::Cr, |r| r.source_terminator());
                        let eol_label = eol_badge_for_terminator(source_term);
                        let badge_font = FontId::monospace(10.0);
                        let painter = ui.painter();
                        let row_count = output.galley.rows.len();

                        for (row_idx, row) in output.galley.rows.iter().enumerate() {
                            let is_segment_end = row.ends_with_newline
                                || (row_idx + 1 == row_count && row.rect.width() > 0.0);
                            if !is_segment_end {
                                continue;
                            }
                            let pos = output.galley_pos
                                + egui::vec2(row.rect.right() + 4.0, row.rect.top() + 1.0);
                            let badge_galley = painter.layout_no_wrap(
                                eol_label.to_string(),
                                badge_font.clone(),
                                editor_palette.control_char_fg,
                            );
                            let badge_rect = egui::Rect::from_min_size(
                                pos,
                                badge_galley.size() + egui::vec2(4.0, 1.0),
                            );
                            painter.rect_filled(
                                badge_rect,
                                CornerRadius::same(2),
                                editor_palette.control_char_bg,
                            );
                            painter.galley(
                                pos + egui::vec2(2.0, 0.5),
                                badge_galley,
                                editor_palette.control_char_fg,
                            );
                        }
                    }

                    if output.response.changed() {
                        text_changed = true;
                    }
                    if output.response.lost_focus() {
                        lost_focus = true;
                    }

                    if let Some(range) = output.cursor_range {
                        let char_idx = range.primary.ccursor.index;
                        let byte_pos = state
                            .inline_raw_text
                            .char_indices()
                            .nth(char_idx)
                            .map_or(state.inline_raw_text.len(), |(i, _)| i);
                        new_cursor_pos = Some(byte_pos);
                        if !text_changed
                            && (output.response.clicked() || new_cursor_pos != prev_cursor_pos)
                        {
                            sync_cursor_offset = Some(byte_pos);
                        }
                    }
                });
        });

    state.inline_raw_cursor = new_cursor_pos;
    if text_changed {
        state.apply_live_inline_raw_edit(settings);
    }
    if lost_focus {
        let _ = state.flush_pending_inline_raw_edit(settings);
    } else if let Some(offset) = sync_cursor_offset {
        state.select_from_raw_offset(offset);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentContextAction {
    EditSegment(usize),
    InsertAbove(usize),
    InsertBelow(usize),
    InsertAtEnd,
    MoveUp(usize),
    MoveDown(usize),
    DeleteSegment(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldContextAction {
    EditField(usize),
    ClearField(usize),
    SaveAttachment(usize),
    ShowUniqueValues(String),
    ReplaceFilterByPath { path: String, value: String },
    AddFilterByPath { path: String, value: String },
    ExcludeFilterByPath { path: String, value: String },
    FilterPresentByPath(String),
    AddPathToReport(String),
    MarkCustomPhiField(String),
    FilterBySelectedRows { append: bool },
    BuildReportFromSelectedFields,
}

fn format_filter_menu_label(prefix: &str, path: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{prefix}{path} = \"\"")
    } else if value.chars().count() > 28 {
        let short: String = value.chars().take(28).collect();
        format!("{prefix}{path} = {short}…")
    } else {
        format!("{prefix}{path} = {value}")
    }
}

/// Build a monospace `LayoutJob` for a Segment Accordion value cell using the record's active
/// HL7 delimiters and the exact same delimiter/escape highlight colors as the Raw Message Editor.
fn build_accordion_value_layout_job(
    text: &str,
    delims: Delimiters,
    editor_palette: &EditorPalette,
    base_color: Color32,
    font_id: FontId,
    is_header_delim_field: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    if text.is_empty() {
        return job;
    }
    if is_header_delim_field {
        job.append(
            text,
            0.0,
            TextFormat {
                font_id,
                color: base_color,
                ..Default::default()
            },
        );
        return job;
    }

    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut run_start = 0usize;

    let flush_text = |job: &mut LayoutJob, slice: &str, font: &FontId, color: Color32| {
        if !slice.is_empty() {
            job.append(
                slice,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                },
            );
        }
    };

    while i < bytes.len() {
        let b = bytes[i];
        if b == delims.escape {
            if let Some(end_idx) = match_hl7_escape_sequence(bytes, i, delims) {
                flush_text(&mut job, &text[run_start..i], &font_id, base_color);
                flush_text(
                    &mut job,
                    &text[i..end_idx],
                    &font_id,
                    editor_palette.escape_sequence,
                );
                i = end_idx;
                run_start = i;
                continue;
            }
        }

        let delim_color = if b == delims.field {
            Some(editor_palette.delim_field)
        } else if b == delims.component {
            Some(editor_palette.delim_component)
        } else if b == delims.repetition {
            Some(editor_palette.delim_repetition)
        } else if b == delims.subcomponent {
            Some(editor_palette.delim_subcomponent)
        } else if b == delims.escape {
            Some(editor_palette.delim_escape)
        } else {
            None
        };

        if let Some(color) = delim_color {
            flush_text(&mut job, &text[run_start..i], &font_id, base_color);
            flush_text(&mut job, &text[i..i + 1], &font_id, color);
            i += 1;
            run_start = i;
        } else {
            let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
            i += ch_len;
        }
    }

    flush_text(&mut job, &text[run_start..], &font_id, base_color);
    job
}

fn show_segment_accordion(
    ui: &mut Ui,
    state: &mut SessionState,
    settings: &mut AppSettings,
    height: f32,
) {
    let seg_count = state.record_cache.as_ref().map_or(0, |c| c.segments.len());
    let mut seg_action: Option<SegmentContextAction> = None;
    let mut field_action: Option<FieldContextAction> = None;

    let has_messages = state.has_messages();
    let mut toggle_show_empty = false;
    let mut toggle_decode_escapes = false;
    let header_bar = ui.horizontal_wrapped(|ui| {
        let header_palette = ThemePalette::for_visuals(ui.visuals());
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.strong(format!("Segments & Fields ({seg_count})"));
        if subheader_button(ui, "Expand All", has_messages, &header_palette).clicked() {
            state.expand_all_segments();
        }
        if subheader_button(ui, "Collapse All", has_messages, &header_palette).clicked() {
            state.collapse_all_segments();
        }
        if subheader_toggle_button(
            ui,
            state.show_empty_fields,
            has_messages,
            "Show Empty",
            &header_palette,
        )
        .on_hover_text(
            "Reveal empty HL7 fields (including standard dictionary slots) so they can be inspected or populated inline",
        )
        .clicked()
        {
            toggle_show_empty = true;
        }
        if subheader_icon_toggle_button(
            ui,
            IconKind::DecodeEscapes,
            settings.decode_escape_sequences,
            has_messages,
            "Decode HL7 escape sequences (\\F\\, \\S\\, \\T\\, \\R\\, \\E\\, \\.br\\) in preview (read-only)",
            &header_palette,
        )
        .clicked()
        {
            toggle_decode_escapes = true;
        }
        if subheader_button(
            ui,
            add_action_button_job(ui, "Segment", has_messages),
            has_messages,
            &header_palette,
        )
        .on_hover_text("Insert a new HL7 segment")
        .clicked()
        {
            seg_action = Some(SegmentContextAction::InsertAtEnd);
        }
    });
    if toggle_show_empty {
        state.set_show_empty_fields(!state.show_empty_fields, settings);
    }
    if toggle_decode_escapes {
        settings.decode_escape_sequences = !settings.decode_escape_sequences;
        settings.save_default();
        state.rebuild_selected_record_cache(settings);
    }
    header_bar.response.context_menu(|ui| {
        let menu_palette = ThemePalette::for_visuals(ui.visuals());
        if icon_menu_button_tinted(
            ui,
            IconKind::Add,
            "Insert Segment at End...",
            None,
            true,
            Some(menu_palette.accent_green),
        )
        .clicked()
        {
            seg_action = Some(SegmentContextAction::InsertAtEnd);
            ui.close_menu();
        }
    });

    let Some(cache) = state.record_cache.clone() else {
        return;
    };

    let theme_palette = ThemePalette::for_visuals(ui.visuals());
    let editor_palette = EditorPalette::for_visuals(ui.visuals());
    let active_delims = state
        .selected_record
        .and_then(|idx| state.doc.records.get(idx))
        .map_or(Delimiters::STANDARD, |r| r.delimiters());
    let value_font = egui::TextStyle::Monospace.resolve(ui.style());
    let selected_seg = state.selected_segment;
    let selected_row = state.selected_field_row;
    let scroll_accordion = state.scroll_accordion_to_selection;
    let active_search_match = state.current_search_match().cloned();

    // Ensure the target segment is expanded BEFORE computing row rects on this frame.
    if scroll_accordion {
        if let Some(seg_idx) = selected_seg {
            state.expanded_segments.insert(seg_idx);
        }
    }

    let active_filter_present = state.has_active_filter();
    let mut toggle_seg: Option<usize> = None;
    let mut clicked_row: Option<(usize, bool, bool)> = None;
    let mut double_clicked_row: Option<usize> = None;
    let mut inline_commit = false;
    let mut inline_cancel = false;
    let mut inline_changed = false;
    let mut target_scroll_rect: Option<egui::Rect> = None;

    ScrollArea::vertical()
        .id_salt("unified_segment_accordion_scroll")
        .max_height(height.saturating_sub_f32(28.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for seg in &cache.segments {
                let is_expanded = state.expanded_segments.contains(&seg.seg_index);
                let is_seg_selected = selected_seg == Some(seg.seg_index);
                let arrow = if is_expanded { "▼" } else { "▶" };
                let can_insert_above = state.can_insert_segment_above(seg.seg_index);
                let can_move_up = state.can_move_segment_up(seg.seg_index);
                let can_move_down = state.can_move_segment_down(seg.seg_index);
                let can_delete_seg = seg.seg_index > 0;

                let avail_w = ui.available_width();
                let header_est_rect =
                    egui::Rect::from_min_size(ui.cursor().min, egui::vec2(avail_w, 26.0));
                let header_hovered = ui.rect_contains_pointer(header_est_rect);
                let header_bg = if is_seg_selected || header_hovered {
                    theme_palette.hover_selection_tint
                } else {
                    theme_palette.elevated_bg
                };

                let frame_resp = Frame::NONE
                    .fill(header_bg)
                    .corner_radius(CornerRadius::same(6))
                    .stroke(Stroke::new(1.0_f32, theme_palette.divider))
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        // Span the full available width of the accordion scroll area (minus 16px horizontal margin)
                        ui.set_width((avail_w - 16.0).max(80.0));
                        ui.horizontal(|ui| {
                            let prefix = format!("{arrow} {}.", seg.seg_index + 1);
                            let r_prefix = ui.add(
                                egui::Label::new(RichText::new(prefix).monospace().strong())
                                    .sense(Sense::click()),
                            );
                            let tag_label = if seg.occurrence > 1 {
                                format!("{}({})", seg.tag, seg.occurrence)
                            } else {
                                seg.tag.clone()
                            };
                            let pill_colors =
                                segment_tag_pill_colors(&seg.tag, theme_palette.is_dark);
                            let r_pill = render_pill_badge(ui, &tag_label, pill_colors)
                                .interact(Sense::click());

                            let desc_text = seg
                                .description
                                .as_deref()
                                .map_or_else(String::new, |d| format!("— {d}"));
                            let r_desc = ui.add(
                                egui::Label::new(RichText::new(desc_text).strong())
                                    .sense(Sense::click()),
                            );
                            let count_color = if theme_palette.is_dark {
                                Color32::from_rgb(0xA6, 0xB1, 0xC4)
                            } else {
                                theme_palette.muted_text
                            };
                            let r_count = ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "({} fields)",
                                        seg.field_rows_range.len()
                                    ))
                                    .small()
                                    .color(count_color),
                                )
                                .sense(Sense::click()),
                            );
                            r_prefix.union(r_pill).union(r_desc).union(r_count)
                        })
                        .inner
                    });

                let seg_resp = frame_resp
                    .response
                    .interact(Sense::click())
                    .union(frame_resp.inner.clone());
                if is_seg_selected {
                    paint_left_accent_bar(ui, frame_resp.response.rect, theme_palette.accent);
                    if target_scroll_rect.is_none() {
                        target_scroll_rect = Some(frame_resp.response.rect);
                    }
                }
                if seg_resp.clicked() {
                    toggle_seg = Some(seg.seg_index);
                }
                seg_resp.context_menu(|ui| {
                    if icon_menu_button(ui, IconKind::Edit, "Edit Segment...", None, true).clicked()
                    {
                        seg_action = Some(SegmentContextAction::EditSegment(seg.seg_index));
                        ui.close_menu();
                    }
                    ui.separator();
                    if icon_menu_button_tinted(
                        ui,
                        IconKind::Add,
                        "Insert Segment Above...",
                        None,
                        can_insert_above,
                        Some(theme_palette.accent_green),
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::InsertAbove(seg.seg_index));
                        ui.close_menu();
                    }
                    if icon_menu_button_tinted(
                        ui,
                        IconKind::Add,
                        "Insert Segment Below...",
                        None,
                        true,
                        Some(theme_palette.accent_green),
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::InsertBelow(seg.seg_index));
                        ui.close_menu();
                    }
                    if icon_menu_button_tinted(
                        ui,
                        IconKind::Add,
                        "Insert Segment at End...",
                        None,
                        true,
                        Some(theme_palette.accent_green),
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::InsertAtEnd);
                        ui.close_menu();
                    }
                    ui.separator();
                    if icon_menu_button(
                        ui,
                        IconKind::MoveUp,
                        "Move Segment Up",
                        None,
                        can_move_up,
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::MoveUp(seg.seg_index));
                        ui.close_menu();
                    }
                    if icon_menu_button(
                        ui,
                        IconKind::MoveDown,
                        "Move Segment Down",
                        None,
                        can_move_down,
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::MoveDown(seg.seg_index));
                        ui.close_menu();
                    }
                    ui.separator();
                    if icon_menu_button_tinted(
                        ui,
                        IconKind::Delete,
                        "Delete Segment",
                        None,
                        can_delete_seg,
                        Some(theme_palette.accent_red),
                    )
                    .clicked()
                    {
                        seg_action = Some(SegmentContextAction::DeleteSegment(seg.seg_index));
                        ui.close_menu();
                    }
                });

                if is_expanded && !seg.field_rows_range.is_empty() {
                    let base_text_color = theme_palette.primary_text;
                    let addr_color = editor_palette.segment_tag;
                    let search_hit_color = if theme_palette.is_dark {
                        Color32::from_rgb(255, 215, 70)
                    } else {
                        Color32::from_rgb(0x9A, 0x67, 0x00)
                    };

                    ui.indent(("seg_indent", seg.seg_index), |ui| {
                        TableBuilder::new(ui)
                            .id_salt(("seg_fields_table", seg.seg_index))
                            .vscroll(false)
                            .striped(true)
                            .resizable(true)
                            .sense(Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(110.0).at_least(75.0)) // Address
                            .column(Column::initial(220.0).at_least(100.0)) // [Description]
                            .column(Column::remainder().at_least(140.0)) // Value
                            .body(|mut body| {
                                for row_idx in seg.field_rows_range.clone() {
                                    let frow = &cache.field_rows[row_idx];
                                    let is_primary_row = selected_row == Some(row_idx);
                                    let is_row_selected = is_primary_row
                                        || state.selected_field_rows.contains(&row_idx);
                                    let is_editable = frow.is_editable;
                                    let is_search_hit =
                                        active_search_match.as_ref().is_some_and(|m| {
                                            m.record_index == cache.record_index
                                                && m.segment_index == seg.seg_index
                                                && (m.field_address.is_none()
                                                    || m.field_address == frow.address)
                                        });

                                    body.row(20.0, |mut row| {
                                        row.set_selected(is_row_selected || is_search_hit);
                                        let addr_str =
                                            frow.address.map(|a| a.to_string()).unwrap_or_default();
                                        let desc_str = frow
                                            .description
                                            .as_ref()
                                            .map(|d| format!("[{d}]"))
                                            .unwrap_or_default();
                                        let display_val = frow.display_value.clone();

                                        let val_color = if is_search_hit {
                                            search_hit_color
                                        } else {
                                            base_text_color
                                        };

                                        // Column 1: Address (non-intercepting so full row catches clicks)
                                        row.col(|ui| {
                                            if is_primary_row {
                                                paint_left_accent_bar(
                                                    ui,
                                                    ui.max_rect(),
                                                    theme_palette.accent,
                                                );
                                            }
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&addr_str)
                                                        .monospace()
                                                        .strong()
                                                        .color(addr_color),
                                                )
                                                .selectable(false)
                                                .sense(Sense::hover()),
                                            );
                                        });

                                        // Column 2: [Description]
                                        row.col(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&desc_str)
                                                        .monospace()
                                                        .color(theme_palette.muted_text),
                                                )
                                                .selectable(false)
                                                .sense(Sense::hover()),
                                            );
                                        });

                                        // Column 3: Value (inline TextEdit when actively editing this row; non-intercepting Label otherwise)
                                        let is_editing_this_row = state.editing_field_row
                                            == Some(row_idx)
                                            && state.editing_field_path == frow.address
                                            && state.editing_field_path.is_some();
                                        row.col(|ui| {
                                            if is_editing_this_row {
                                                let resp = ui.add(
                                                    TextEdit::singleline(
                                                        &mut state.editing_field_buffer,
                                                    )
                                                    .id_salt((
                                                        "inline_field_edit",
                                                        seg.seg_index,
                                                        frow.address,
                                                    ))
                                                    .font(egui::TextStyle::Monospace)
                                                    .desired_width(ui.available_width().max(80.0)),
                                                );
                                                if state.editing_field_just_opened {
                                                    resp.request_focus();
                                                    state.editing_field_just_opened = false;
                                                } else if ui
                                                    .input(|i| i.key_pressed(egui::Key::Escape))
                                                {
                                                    inline_cancel = true;
                                                } else if ui
                                                    .input(|i| i.key_pressed(egui::Key::Enter))
                                                    || resp.lost_focus()
                                                {
                                                    inline_commit = true;
                                                } else if resp.changed() {
                                                    inline_changed = true;
                                                }
                                            } else if display_val.is_empty() {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new("(empty)")
                                                            .italics()
                                                            .small()
                                                            .color(ui.visuals().weak_text_color()),
                                                    )
                                                    .selectable(false)
                                                    .sense(Sense::hover()),
                                                );
                                            } else {
                                                if let Some(ref att) = frow.attachment {
                                                    let badge_colors =
                                                        crate::ui::theme::message_type_pill_colors(
                                                            "ORU",
                                                            theme_palette.is_dark,
                                                        );
                                                    render_pill_badge(
                                                        ui,
                                                        &att.badge_text(),
                                                        badge_colors,
                                                    );
                                                    if ui
                                                        .small_button("Save Attachment...")
                                                        .on_hover_text(format!(
                                                            "Decode and save {} attachment to disk",
                                                            att.label
                                                        ))
                                                        .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::SaveAttachment(
                                                                row_idx,
                                                            ),
                                                        );
                                                    }
                                                }
                                                let is_header_delim = frow
                                                    .address
                                                    .is_some_and(|a| a.is_header_delimiter_field());
                                                let val_job = build_accordion_value_layout_job(
                                                    &display_val,
                                                    active_delims,
                                                    &editor_palette,
                                                    val_color,
                                                    value_font.clone(),
                                                    is_header_delim,
                                                );
                                                ui.add(
                                                    egui::Label::new(val_job)
                                                        .selectable(false)
                                                        .sense(Sense::hover()),
                                                );
                                            }
                                        });

                                        // Full-row hit-testing across all columns & empty whitespace (zero dead zones)
                                        let mut row_resp = row.response();
                                        if !is_editing_this_row {
                                            if let Some(ref tip) = frow.tooltip {
                                                row_resp = row_resp.on_hover_text(tip);
                                            }
                                        }
                                        if is_primary_row {
                                            target_scroll_rect = Some(row_resp.rect);
                                        }
                                        if !is_editing_this_row {
                                            if row_resp.double_clicked() {
                                                double_clicked_row = Some(row_idx);
                                            } else if row_resp.clicked() {
                                                let (ctrl, shift) = row_resp.ctx.input(|i| {
                                                    (
                                                        i.modifiers.command || i.modifiers.ctrl,
                                                        i.modifiers.shift,
                                                    )
                                                });
                                                clicked_row = Some((row_idx, ctrl, shift));
                                            } else if row_resp.secondary_clicked()
                                                && !state.selected_field_rows.contains(&row_idx)
                                            {
                                                clicked_row = Some((row_idx, false, false));
                                            }
                                        }
                                        let is_multi_ctx = state.selected_field_rows.len() > 1
                                            && state.selected_field_rows.contains(&row_idx);
                                        let multi_count = state.selected_field_rows.len();
                                        let single_summary =
                                            crate::state::format_field_row_summary_line(frow);
                                        row_resp.context_menu(|ui| {
                                            if is_multi_ctx {
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Report,
                                                    &format!(
                                                        "Build Report with Selected Fields ({multi_count})..."
                                                    ),
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    field_action = Some(
                                                        FieldContextAction::BuildReportFromSelectedFields,
                                                    );
                                                    ui.close_menu();
                                                }
                                                if active_filter_present {
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Filter,
                                                        &format!("Add {multi_count} Filters"),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::FilterBySelectedRows {
                                                                append: true,
                                                            },
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Filter,
                                                        &format!(
                                                            "Replace Filter with Selected Values ({multi_count})"
                                                        ),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::FilterBySelectedRows {
                                                                append: false,
                                                            },
                                                        );
                                                        ui.close_menu();
                                                    }
                                                } else if icon_menu_button(
                                                    ui,
                                                    IconKind::Filter,
                                                    &format!(
                                                        "Filter by Selected Values ({multi_count})"
                                                    ),
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    field_action = Some(
                                                        FieldContextAction::FilterBySelectedRows {
                                                            append: false,
                                                        },
                                                    );
                                                    ui.close_menu();
                                                }
                                                ui.separator();
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Copy,
                                                    &format!("Copy Values ({multi_count})"),
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    ui.ctx().copy_text(
                                                        state.format_accordion_copy_values(),
                                                    );
                                                    ui.close_menu();
                                                }
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Copy,
                                                    &format!("Copy HL7 Paths ({multi_count})"),
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    ui.ctx().copy_text(
                                                        state.format_accordion_copy_paths(),
                                                    );
                                                    ui.close_menu();
                                                }
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Copy,
                                                    &format!(
                                                        "Copy Path, Name & Value ({multi_count})"
                                                    ),
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    ui.ctx().copy_text(
                                                        state.format_accordion_copy_summary(),
                                                    );
                                                    ui.close_menu();
                                                }
                                            } else {
                                                if !addr_str.is_empty() {
                                                    let hdr_text = match &frow.description {
                                                        Some(d) => format!("{addr_str} — {d}"),
                                                        None => addr_str.clone(),
                                                    };
                                                    ui.label(
                                                        RichText::new(hdr_text)
                                                            .monospace()
                                                            .strong()
                                                            .color(theme_palette.accent),
                                                    );
                                                    ui.separator();
                                                }
                                                if let Some(ref att) = frow.attachment {
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Save,
                                                        &format!(
                                                            "Save Attachment... ({})",
                                                            att.label
                                                        ),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::SaveAttachment(
                                                                row_idx,
                                                            ),
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                }
                                                if !addr_str.is_empty() {
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Report,
                                                        &format!("Show Unique Values ({addr_str})"),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::ShowUniqueValues(
                                                                addr_str.clone(),
                                                            ),
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    if !display_val.is_empty() {
                                                        if active_filter_present {
                                                            if icon_menu_button(
                                                                ui,
                                                                IconKind::Filter,
                                                                &format_filter_menu_label(
                                                                    "Filter: ",
                                                                    &addr_str,
                                                                    &display_val,
                                                                ),
                                                                None,
                                                                true,
                                                            )
                                                            .clicked()
                                                            {
                                                                field_action = Some(
                                                                    FieldContextAction::AddFilterByPath {
                                                                        path: addr_str.clone(),
                                                                        value: display_val.clone(),
                                                                    },
                                                                );
                                                                ui.close_menu();
                                                            }
                                                        } else if icon_menu_button(
                                                            ui,
                                                            IconKind::Filter,
                                                            &format_filter_menu_label(
                                                                "Filter: ",
                                                                &addr_str,
                                                                &display_val,
                                                            ),
                                                            None,
                                                            true,
                                                        )
                                                        .clicked()
                                                        {
                                                            field_action = Some(
                                                                FieldContextAction::ReplaceFilterByPath {
                                                                    path: addr_str.clone(),
                                                                    value: display_val.clone(),
                                                                },
                                                            );
                                                            ui.close_menu();
                                                        }
                                                        let excl_label = if display_val.chars().count() > 24 {
                                                            let short: String = display_val.chars().take(24).collect();
                                                            format!("Exclude: {addr_str} != \"{short}…\"")
                                                        } else {
                                                            format!("Exclude: {addr_str} != \"{display_val}\"")
                                                        };
                                                        if icon_menu_button(
                                                            ui,
                                                            IconKind::Filter,
                                                            &excl_label,
                                                            None,
                                                            true,
                                                        )
                                                        .clicked()
                                                        {
                                                            field_action = Some(
                                                                FieldContextAction::ExcludeFilterByPath {
                                                                    path: addr_str.clone(),
                                                                    value: display_val.clone(),
                                                                },
                                                            );
                                                            ui.close_menu();
                                                        }
                                                    }
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Filter,
                                                        &format!(
                                                            "Filter: {addr_str} is present (non-empty)"
                                                        ),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::FilterPresentByPath(
                                                                addr_str.clone(),
                                                            ),
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    if icon_menu_button(
                                                        ui,
                                                        IconKind::Report,
                                                        &format!("Add \"{addr_str}\" to Report"),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::AddPathToReport(
                                                                addr_str.clone(),
                                                            ),
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    if !frow.address.is_some_and(|a| {
                                                        a.is_header_delimiter_field()
                                                    }) && icon_menu_button(
                                                        ui,
                                                        IconKind::Edit,
                                                        &format!(
                                                            "Mark \"{addr_str}\" as Custom PHI Field"
                                                        ),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                    {
                                                        field_action = Some(
                                                            FieldContextAction::MarkCustomPhiField(
                                                                addr_str.clone(),
                                                            ),
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                }
                                                if !addr_str.is_empty()
                                                    && icon_menu_button(
                                                        ui,
                                                        IconKind::Copy,
                                                        &format!("Copy HL7 Path ({addr_str})"),
                                                        None,
                                                        true,
                                                    )
                                                    .clicked()
                                                {
                                                    ui.ctx().copy_text(addr_str.clone());
                                                    ui.close_menu();
                                                }
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Copy,
                                                    "Copy Field Value",
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    ui.ctx().copy_text(display_val.clone());
                                                    ui.close_menu();
                                                }
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Copy,
                                                    "Copy Path, Name & Value",
                                                    None,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    ui.ctx().copy_text(single_summary.clone());
                                                    ui.close_menu();
                                                }
                                                ui.separator();
                                                if icon_menu_button(
                                                    ui,
                                                    IconKind::Edit,
                                                    "Edit Value...",
                                                    Some("F2"),
                                                    is_editable,
                                                )
                                                .clicked()
                                                {
                                                    field_action = Some(
                                                        FieldContextAction::EditField(row_idx),
                                                    );
                                                    ui.close_menu();
                                                }
                                                if icon_menu_button_tinted(
                                                    ui,
                                                    IconKind::Delete,
                                                    "Clear Value",
                                                    None,
                                                    is_editable,
                                                    Some(theme_palette.accent_red),
                                                )
                                                .clicked()
                                                {
                                                    field_action = Some(
                                                        FieldContextAction::ClearField(row_idx),
                                                    );
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                    });
                                }
                            });
                    });
                }
                ui.add_space(3.0);
            }

            if scroll_accordion {
                if let Some(rect) = target_scroll_rect {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            }

            // Empty-area right-click menu in the Segment Inspector when clicking below segments
            let rem_h = ui.available_height().max(24.0);
            let (_, empty_resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), rem_h), Sense::click());
            if empty_resp.clicked() {
                state.selected_segment = None;
                state.selected_field_row = None;
                state.selected_field_rows.clear();
                state.field_selection_anchor = None;
            }
            empty_resp.context_menu(|ui| {
                if icon_menu_button_tinted(
                    ui,
                    IconKind::Add,
                    "Insert Segment at End...",
                    None,
                    true,
                    Some(theme_palette.accent_green),
                )
                .clicked()
                {
                    seg_action = Some(SegmentContextAction::InsertAtEnd);
                    ui.close_menu();
                }
            });
        });

    state.scroll_to_segment = false;
    state.scroll_to_field_row = false;
    state.scroll_accordion_to_selection = false;

    if inline_cancel {
        state.cancel_inline_field_edit();
        state.rebuild_selected_record_cache(settings);
    } else if inline_commit {
        let _ = state.commit_inline_field_edit(settings);
    } else if inline_changed {
        state.apply_live_inline_field_preview(settings);
    }

    if let Some(seg_idx) = toggle_seg {
        state.toggle_segment_accordion(seg_idx);
    }
    if let Some(row_idx) = double_clicked_row {
        state.select_field_row(row_idx);
        let _ = state.start_inline_field_edit(settings);
    } else if let Some((row_idx, ctrl, shift)) = clicked_row {
        state.select_field_row_with_modifiers(row_idx, ctrl, shift);
    }

    if let Some(act) = seg_action {
        match act {
            SegmentContextAction::EditSegment(idx) => state.open_edit_segment_dialog(idx),
            SegmentContextAction::InsertAbove(idx) => state.open_insert_segment_above_dialog(idx),
            SegmentContextAction::InsertBelow(idx) => state.open_insert_segment_below_dialog(idx),
            SegmentContextAction::InsertAtEnd => state.open_insert_segment_at_end_dialog(),
            SegmentContextAction::MoveUp(idx) => {
                let _ = state.move_segment_up(idx, settings);
            }
            SegmentContextAction::MoveDown(idx) => {
                let _ = state.move_segment_down(idx, settings);
            }
            SegmentContextAction::DeleteSegment(idx) => {
                let _ = state.request_delete_segment_at(idx, settings);
            }
        }
    }

    if let Some(act) = field_action {
        match act {
            FieldContextAction::EditField(row_idx) => {
                state.select_field_row(row_idx);
                let _ = state.start_inline_field_edit(settings);
            }
            FieldContextAction::ClearField(row_idx) => {
                let _ = state.request_clear_field_row(row_idx, settings);
            }
            FieldContextAction::SaveAttachment(row_idx) => {
                if let Some((att, suggested_name)) = state.extract_attachment_for_field_row(row_idx)
                {
                    let filter_label = format!("{} File (*.{})", att.label, att.extension);
                    let dialog = rfd::FileDialog::new()
                        .set_title("Save Encapsulated Attachment")
                        .set_file_name(&suggested_name)
                        .add_filter(&filter_label, &[att.extension])
                        .add_filter("All Files (*.*)", &["*"]);
                    if let Some(path) = dialog.save_file() {
                        if let Err(e) = state.save_attachment_for_row_to_path(row_idx, path) {
                            state.status_message = e.clone();
                            state.modal_alert = Some(e);
                        }
                    }
                }
            }
            FieldContextAction::ShowUniqueValues(path) => {
                state.open_unique_values_for_path(&path);
            }
            FieldContextAction::ReplaceFilterByPath { path, value } => {
                state.open_filter_for_path(&path, &value, settings);
            }
            FieldContextAction::AddFilterByPath { path, value } => {
                state.add_filter_for_path(&path, &value, settings);
            }
            FieldContextAction::ExcludeFilterByPath { path, value } => {
                state.exclude_filter_for_path(&path, &value, settings);
            }
            FieldContextAction::FilterPresentByPath(path) => {
                state.filter_present_for_path(&path, settings);
            }
            FieldContextAction::AddPathToReport(path) => {
                state.add_path_to_report_and_open(&path);
            }
            FieldContextAction::MarkCustomPhiField(path) => {
                let rule = hl7_core::CustomPhiRule {
                    path: path.clone(),
                    mode: hl7_core::CustomPhiMode::Synthetic,
                };
                match settings.add_custom_phi_rule(rule) {
                    Ok(()) => {
                        settings.save_default();
                        state.status_message = format!(
                            "Marked '{path}' as a Custom PHI field (Synthetic) for Scrub PHI."
                        );
                    }
                    Err(err) => {
                        state.status_message = err;
                    }
                }
            }
            FieldContextAction::FilterBySelectedRows { append } => {
                state.filter_by_selected_accordion_rows(append, settings);
            }
            FieldContextAction::BuildReportFromSelectedFields => {
                state.open_report_from_selected_accordion_fields();
            }
        }
    }
}

/// Uniform compact height for buttons, toggle buttons, and metadata pills across
/// both the Raw Message Editor and Segment Accordion subheaders.
const SUBHEADER_CONTROL_HEIGHT: f32 = 22.0;

/// Render a unified compact subheader action button with consistent corner radius,
/// padding, border stroke, and disabled styling across both detail-pane subheaders.
fn subheader_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    enabled: bool,
    palette: &ThemePalette,
) -> egui::Response {
    let mut btn = egui::Button::new(label)
        .small()
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(0.0, SUBHEADER_CONTROL_HEIGHT));
    if !enabled {
        btn = btn
            .fill(palette.side_panel_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    }
    ui.add_enabled(enabled, btn)
}

/// Render a unified compact subheader toggle button matching `subheader_button` geometry
/// and border treatment, with an active accent border/fill when `active == true`.
fn subheader_toggle_button(
    ui: &mut Ui,
    active: bool,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
    palette: &ThemePalette,
) -> egui::Response {
    let mut btn = egui::Button::new(label)
        .small()
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(0.0, SUBHEADER_CONTROL_HEIGHT));
    if !enabled {
        btn = btn
            .fill(palette.side_panel_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    } else if active {
        btn = btn
            .fill(palette.hover_selection_tint)
            .stroke(Stroke::new(1.0_f32, palette.accent));
    } else {
        btn = btn
            .fill(palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    }
    ui.add_enabled(enabled, btn)
}

/// Render a unified compact square subheader toggle button matching `subheader_icon_toggle_button`
/// dimensions (26×22) for single-glyph toggles such as `¶`.
fn subheader_square_toggle_button(
    ui: &mut Ui,
    active: bool,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
    palette: &ThemePalette,
) -> egui::Response {
    let mut btn = egui::Button::new(label)
        .small()
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(26.0, SUBHEADER_CONTROL_HEIGHT));
    if !enabled {
        btn = btn
            .fill(palette.side_panel_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    } else if active {
        btn = btn
            .fill(palette.hover_selection_tint)
            .stroke(Stroke::new(1.0_f32, palette.accent));
    } else {
        btn = btn
            .fill(palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    }
    ui.add_enabled(enabled, btn)
}

/// Render a unified compact subheader icon-only action button (26×22) with a centered
/// pixel-snapped vector icon and optional accent tint when enabled.
fn subheader_icon_button(
    ui: &mut Ui,
    icon: IconKind,
    enabled: bool,
    accent: Option<Color32>,
    tooltip: &str,
    palette: &ThemePalette,
) -> egui::Response {
    let mut btn = egui::Button::new("")
        .small()
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(26.0, SUBHEADER_CONTROL_HEIGHT));
    if !enabled {
        btn = btn
            .fill(palette.side_panel_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    } else {
        btn = btn
            .fill(palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    }
    let resp = ui.add_enabled(enabled, btn).on_hover_text(tooltip);
    if ui.is_rect_visible(resp.rect) {
        let fg = if !enabled {
            ui.visuals().weak_text_color()
        } else {
            accent.unwrap_or(palette.primary_text)
        };
        let fill_bg = if resp.hovered() && enabled {
            palette.hover_selection_tint
        } else if enabled {
            palette.elevated_bg
        } else {
            palette.side_panel_bg
        };
        paint_icon(ui.painter(), icon, resp.rect.center(), fg, fill_bg, 13.0);
    }
    resp
}

/// Render a unified compact subheader icon-only toggle button with a centered pixel-snapped
/// vector icon and active accent border/fill when `active == true`.
fn subheader_icon_toggle_button(
    ui: &mut Ui,
    icon: IconKind,
    active: bool,
    enabled: bool,
    tooltip: &str,
    palette: &ThemePalette,
) -> egui::Response {
    let mut btn = egui::Button::new("")
        .small()
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(26.0, SUBHEADER_CONTROL_HEIGHT));
    if !enabled {
        btn = btn
            .fill(palette.side_panel_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    } else if active {
        btn = btn
            .fill(palette.hover_selection_tint)
            .stroke(Stroke::new(1.0_f32, palette.accent));
    } else {
        btn = btn
            .fill(palette.elevated_bg)
            .stroke(Stroke::new(1.0_f32, palette.divider));
    }
    let resp = ui.add_enabled(enabled, btn).on_hover_text(tooltip);
    if ui.is_rect_visible(resp.rect) {
        let fg = if !enabled {
            ui.visuals().weak_text_color()
        } else if active {
            palette.accent
        } else {
            palette.muted_text
        };
        let fill_bg = if active || (resp.hovered() && enabled) {
            palette.hover_selection_tint
        } else {
            palette.elevated_bg
        };
        paint_icon(ui.painter(), icon, resp.rect.center(), fg, fill_bg, 13.0);
    }
    resp
}

fn build_delimiters_pill_job(
    delims: Delimiters,
    font_id: FontId,
    editor_palette: &EditorPalette,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let specs = [
        (delims.field, editor_palette.delim_field),
        (delims.component, editor_palette.delim_component),
        (delims.repetition, editor_palette.delim_repetition),
        (delims.escape, editor_palette.delim_escape),
        (delims.subcomponent, editor_palette.delim_subcomponent),
    ];
    let mut buf = [0u8; 4];
    for (byte, color) in specs {
        let ch = char::from(byte);
        let text = ch.encode_utf8(&mut buf);
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color,
                ..Default::default()
            },
        );
    }
    job
}

trait SaturatingSubF32 {
    fn saturating_sub_f32(self, rhs: f32) -> f32;
}

impl SaturatingSubF32 for f32 {
    fn saturating_sub_f32(self, rhs: f32) -> f32 {
        (self - rhs).max(40.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiters_pill_job_uses_editor_palette_colors_for_standard_and_custom_delims() {
        let dark_palette = EditorPalette::for_visuals(&egui::Visuals::dark());
        let font = FontId::monospace(11.0);

        let std_job = build_delimiters_pill_job(Delimiters::STANDARD, font.clone(), &dark_palette);
        assert_eq!(std_job.text, "|^~\\&");
        assert_eq!(std_job.sections.len(), 5);
        assert_eq!(std_job.sections[0].format.color, dark_palette.delim_field);
        assert_eq!(
            std_job.sections[1].format.color,
            dark_palette.delim_component
        );
        assert_eq!(
            std_job.sections[2].format.color,
            dark_palette.delim_repetition
        );
        assert_eq!(std_job.sections[3].format.color, dark_palette.delim_escape);
        assert_eq!(
            std_job.sections[4].format.color,
            dark_palette.delim_subcomponent
        );

        let custom = Delimiters::from_five_bytes(*b":!@#$").expect("valid custom delimiters");
        let light_palette = EditorPalette::for_visuals(&egui::Visuals::light());
        let custom_job = build_delimiters_pill_job(custom, font, &light_palette);
        assert_eq!(custom_job.text, ":!@#$");
        assert_eq!(custom_job.sections.len(), 5);
        assert_eq!(
            custom_job.sections[0].format.color,
            light_palette.delim_field
        );
        assert_eq!(
            custom_job.sections[1].format.color,
            light_palette.delim_component
        );
        assert_eq!(
            custom_job.sections[2].format.color,
            light_palette.delim_repetition
        );
        assert_eq!(
            custom_job.sections[3].format.color,
            light_palette.delim_escape
        );
        assert_eq!(
            custom_job.sections[4].format.color,
            light_palette.delim_subcomponent
        );
    }
}
