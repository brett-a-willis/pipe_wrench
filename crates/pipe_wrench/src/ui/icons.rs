//! Crisp 14×14 pixel-snapped vector action icons rendered via `egui::Painter` primitives.
//!
//! Avoids Unicode emojis and missing-glyph tofu boxes across all top-bar menus and right-click
//! context menus, automatically tinting with `ui.visuals().text_color()` / `weak_text_color()`.

use egui::{
    pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    TextStyle, Ui,
};

/// Vector icon kinds used across top-bar dropdown menus and context menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// Folder icon (`Open`, `Append`).
    Open,
    /// Floppy / save tray icon (`Save`, `Save As`, `Export`).
    Save,
    /// Directory scan / magnifying folder icon.
    Scan,
    /// Sliders / preferences icon.
    Settings,
    /// Curved left arrow (`Undo`).
    Undo,
    /// Curved right arrow (`Redo`).
    Redo,
    /// Overlapping sheets (`Copy`, `Copy HL7 Path`, `Copy Value`).
    Copy,
    /// Clipboard (`Paste`).
    Paste,
    /// Diagonal pencil (`Edit`, `Assign`).
    Edit,
    /// Plus sign (`Add`, `Insert`).
    Add,
    /// Trash can (`Clear`, `Delete`).
    Delete,
    /// Delimiter bars (`Change Message Delimiters`).
    Delimiters,
    /// Backslash + asterisk/sparkle (`Decode Escape Sequences`).
    DecodeEscapes,
    /// Return arrow `↵` (`Insert Line Break`).
    InsertLineBreak,
    /// Counter-clockwise reset arrow (`Revert`).
    Revert,
    /// Magnifying glass (`Find`, `Search`).
    Find,
    /// Opposing swap arrows (`Replace`).
    Replace,
    /// Funnel (`Filter`, `Add Filter`, `Replace Filter`).
    Filter,
    /// Upward arrow (`Move Up`).
    MoveUp,
    /// Downward arrow (`Move Down`).
    MoveDown,
    /// Up/down sort arrows (`Sort`).
    Sort,
    /// Zoom in (`+` lens).
    ZoomIn,
    /// Zoom out (`−` lens).
    ZoomOut,
    /// Reset zoom (`1:1` frame).
    ZoomReset,
    /// Bar chart (`Reports`).
    Report,
    /// Connected network nodes (`Network`, `TCP/IP`).
    Network,
    /// Side-by-side split comparison panes (`Compare Messages`).
    Compare,
    /// Question mark badge (`Help`, `Shortcuts`).
    Help,
    /// Rightward disclosure chevron (`>`).
    ChevronRight,
    /// Downward disclosure chevron (`v`).
    ChevronDown,
    /// Three horizontal menu/filter lines (`≡`).
    MenuLines,
    /// Crisp diagonal cross (`×` for Close / Clear).
    Close,
    /// Crisp checkmark (`✓` for Apply / Confirm).
    Check,
    /// Single match replace icon (source box with arrow to target box).
    ReplaceOne,
    /// Replace-all icon (stacked target boxes with replace arrow).
    ReplaceAll,
}

/// Standardized inline icon canvas size (`14.0 px`).
pub const ICON_SIZE_INLINE: f32 = 14.0;
/// Standardized toolbar/button icon canvas size (`16.0 px`).
pub const ICON_SIZE_TOOLBAR: f32 = 16.0;

/// Paint a crisp pixel-snapped vector icon centered at `c` with logical bounding box `size × size`.
pub fn paint_icon(
    painter: &Painter,
    icon: IconKind,
    c: Pos2,
    fg: Color32,
    fill_bg: Color32,
    size: f32,
) {
    let s = size / 14.0;
    let cx = c.x.round();
    let cy = c.y.round();
    let p = |dx: f32, dy: f32| pos2(cx + dx * s, cy + dy * s);
    let stroke = Stroke::new(1.5_f32, fg);
    let thin = Stroke::new(1.25_f32, fg);

    match icon {
        IconKind::Open => {
            // Folder tab + body
            let pts = vec![
                p(-5.5, -4.0),
                p(-1.5, -4.0),
                p(-0.2, -2.5),
                p(5.5, -2.5),
                p(5.5, 4.5),
                p(-5.5, 4.5),
                p(-5.5, -4.0),
            ];
            painter.add(egui::Shape::line(pts, stroke));
        }
        IconKind::Save => {
            // Floppy disk outline + top label slot + bottom shutter
            let outer = Rect::from_min_max(p(-5.0, -5.0), p(5.0, 5.0));
            painter.rect_stroke(outer, CornerRadius::same(1), stroke, StrokeKind::Middle);
            let top = Rect::from_min_max(p(-3.0, -5.0), p(2.5, -1.5));
            painter.rect_stroke(top, CornerRadius::ZERO, thin, StrokeKind::Middle);
            let bot = Rect::from_min_max(p(-2.8, 1.0), p(2.8, 5.0));
            painter.rect_stroke(bot, CornerRadius::ZERO, thin, StrokeKind::Middle);
        }
        IconKind::Scan => {
            // Document lines + small magnifier
            painter.line_segment([p(-5.5, -4.5), p(1.5, -4.5)], thin);
            painter.line_segment([p(-5.5, -1.5), p(0.0, -1.5)], thin);
            painter.line_segment([p(-5.5, 1.5), p(-1.0, 1.5)], thin);
            painter.circle_stroke(p(2.2, 1.2), 2.8 * s, stroke);
            painter.line_segment([p(4.2, 3.2), p(6.0, 5.0)], stroke);
        }
        IconKind::Settings => {
            // Three horizontal slider tracks with knobs
            for (y, kx) in [(-4.0, -2.0), (0.0, 2.2), (4.0, -0.8)] {
                painter.line_segment([p(-5.5, y), p(5.5, y)], thin);
                painter.circle_filled(p(kx, y), 1.7 * s, fg);
            }
        }
        IconKind::Undo => {
            // Curved arrow pointing left
            let pts = vec![
                p(-4.5, -1.5),
                p(1.5, -1.5),
                p(4.0, -0.5),
                p(4.8, 1.5),
                p(4.0, 3.8),
                p(1.5, 4.5),
                p(-2.5, 4.5),
            ];
            painter.add(egui::Shape::line(pts, stroke));
            painter.line_segment([p(-4.8, -1.5), p(-1.8, -4.5)], stroke);
            painter.line_segment([p(-4.8, -1.5), p(-1.8, 1.5)], stroke);
        }
        IconKind::Redo => {
            // Curved arrow pointing right
            let pts = vec![
                p(4.5, -1.5),
                p(-1.5, -1.5),
                p(-4.0, -0.5),
                p(-4.8, 1.5),
                p(-4.0, 3.8),
                p(-1.5, 4.5),
                p(2.5, 4.5),
            ];
            painter.add(egui::Shape::line(pts, stroke));
            painter.line_segment([p(4.8, -1.5), p(1.8, -4.5)], stroke);
            painter.line_segment([p(4.8, -1.5), p(1.8, 1.5)], stroke);
        }
        IconKind::Copy => {
            let back = Rect::from_min_max(p(-5.0, -5.5), p(1.5, 2.0));
            painter.rect_stroke(back, CornerRadius::same(1), thin, StrokeKind::Middle);
            let front = Rect::from_min_max(p(-1.5, -2.0), p(5.0, 5.5));
            painter.rect_filled(front, CornerRadius::same(1), fill_bg);
            painter.rect_stroke(front, CornerRadius::same(1), stroke, StrokeKind::Middle);
        }
        IconKind::Paste => {
            let board = Rect::from_min_max(p(-4.5, -3.5), p(4.5, 5.5));
            painter.rect_stroke(board, CornerRadius::same(1), stroke, StrokeKind::Middle);
            let clip = Rect::from_min_max(p(-2.2, -5.5), p(2.2, -2.5));
            painter.rect_filled(clip, CornerRadius::same(1), fill_bg);
            painter.rect_stroke(clip, CornerRadius::same(1), thin, StrokeKind::Middle);
            painter.line_segment([p(-2.2, 0.5), p(2.2, 0.5)], thin);
            painter.line_segment([p(-2.2, 2.8), p(1.2, 2.8)], thin);
        }
        IconKind::Edit => {
            // Diagonal pencil pointing bottom-left
            let body = vec![
                p(1.8, -5.0),
                p(4.8, -2.0),
                p(-1.8, 4.6),
                p(-5.0, 5.0),
                p(-4.6, 1.8),
                p(1.8, -5.0),
            ];
            painter.add(egui::Shape::line(body, stroke));
            painter.line_segment([p(0.3, -3.5), p(3.3, -0.5)], thin);
        }
        IconKind::Add => {
            painter.line_segment([p(-4.8, 0.0), p(4.8, 0.0)], stroke);
            painter.line_segment([p(0.0, -4.8), p(0.0, 4.8)], stroke);
        }
        IconKind::Delete => {
            // Trash can lid + handle + tapered bin
            painter.line_segment([p(-5.0, -3.2), p(5.0, -3.2)], stroke);
            let handle = Rect::from_min_max(p(-2.0, -5.2), p(2.0, -3.2));
            painter.rect_stroke(handle, CornerRadius::ZERO, thin, StrokeKind::Middle);
            let bin = vec![p(-3.8, -3.0), p(-3.2, 5.0), p(3.2, 5.0), p(3.8, -3.0)];
            painter.add(egui::Shape::line(bin, stroke));
            painter.line_segment([p(-1.3, -1.0), p(-1.1, 3.2)], thin);
            painter.line_segment([p(1.3, -1.0), p(1.1, 3.2)], thin);
        }
        IconKind::Delimiters => {
            // HL7 `|^` glyph in crisp vectors
            painter.line_segment([p(-3.2, -5.0), p(-3.2, 5.0)], stroke);
            painter.line_segment([p(0.2, 0.5), p(2.8, -4.2)], stroke);
            painter.line_segment([p(5.4, 0.5), p(2.8, -4.2)], stroke);
            painter.line_segment([p(0.2, 4.0), p(5.4, 4.0)], thin);
        }
        IconKind::DecodeEscapes => {
            // Stylized `\*` (backslash + 6-ray asterisk/sparkle) for HL7 escape decoding
            painter.line_segment([p(-5.2, -4.6), p(-1.4, 4.6)], stroke);
            let ax = 2.4_f32;
            let ay = -0.3_f32;
            painter.line_segment([p(ax, ay - 3.4), p(ax, ay + 3.4)], thin);
            painter.line_segment([p(ax - 2.9, ay - 1.7), p(ax + 2.9, ay + 1.7)], thin);
            painter.line_segment([p(ax + 2.9, ay - 1.7), p(ax - 2.9, ay + 1.7)], thin);
        }
        IconKind::InsertLineBreak => {
            // Clean `↵` return arrow
            let stem = vec![p(4.4, -4.2), p(4.4, 1.4), p(-4.4, 1.4)];
            painter.add(egui::Shape::line(stem, stroke));
            painter.line_segment([p(-4.6, 1.4), p(-1.6, -1.6)], stroke);
            painter.line_segment([p(-4.6, 1.4), p(-1.6, 4.4)], stroke);
        }
        IconKind::Revert => {
            // Counter-clockwise reset/revert circular arrow
            let arc = vec![
                p(-3.8, -1.4),
                p(-2.2, -3.8),
                p(0.6, -4.6),
                p(3.4, -3.4),
                p(4.6, -0.6),
                p(4.0, 2.4),
                p(1.8, 4.4),
                p(-1.2, 4.6),
                p(-3.6, 3.0),
            ];
            painter.add(egui::Shape::line(arc, stroke));
            painter.line_segment([p(-4.2, -1.0), p(-4.4, -4.4)], stroke);
            painter.line_segment([p(-4.2, -1.0), p(-0.9, -1.0)], stroke);
        }
        IconKind::Find => {
            painter.circle_stroke(p(-1.2, -1.2), 3.6 * s, stroke);
            painter.line_segment([p(1.6, 1.6), p(5.2, 5.2)], Stroke::new(1.6_f32, fg));
        }
        IconKind::Replace => {
            // Right arrow on top, left arrow on bottom
            painter.line_segment([p(-5.0, -2.5), p(4.8, -2.5)], stroke);
            painter.line_segment([p(2.2, -5.0), p(4.8, -2.5)], stroke);
            painter.line_segment([p(2.2, 0.0), p(4.8, -2.5)], stroke);
            painter.line_segment([p(5.0, 2.8), p(-4.8, 2.8)], stroke);
            painter.line_segment([p(-2.2, 0.3), p(-4.8, 2.8)], stroke);
            painter.line_segment([p(-2.2, 5.3), p(-4.8, 2.8)], stroke);
        }
        IconKind::Filter => {
            // Funnel outline
            let pts = vec![
                p(-5.5, -4.8),
                p(5.5, -4.8),
                p(1.5, 0.2),
                p(1.5, 4.8),
                p(-1.5, 3.4),
                p(-1.5, 0.2),
                p(-5.5, -4.8),
            ];
            painter.add(egui::Shape::line(pts, stroke));
        }
        IconKind::MoveUp => {
            painter.line_segment([p(0.0, -4.6), p(0.0, 4.8)], stroke);
            painter.line_segment([p(-3.6, -1.0), p(0.0, -4.6)], stroke);
            painter.line_segment([p(3.6, -1.0), p(0.0, -4.6)], stroke);
        }
        IconKind::MoveDown => {
            painter.line_segment([p(0.0, -4.8), p(0.0, 4.6)], stroke);
            painter.line_segment([p(-3.6, 1.0), p(0.0, 4.6)], stroke);
            painter.line_segment([p(3.6, 1.0), p(0.0, 4.6)], stroke);
        }
        IconKind::Sort => {
            // Up arrow left, down arrow right
            painter.line_segment([p(-2.8, -4.5), p(-2.8, 4.5)], stroke);
            painter.line_segment([p(-5.2, -2.0), p(-2.8, -4.5)], thin);
            painter.line_segment([p(-0.4, -2.0), p(-2.8, -4.5)], thin);
            painter.line_segment([p(2.8, -4.5), p(2.8, 4.5)], stroke);
            painter.line_segment([p(0.4, 2.0), p(2.8, 4.5)], thin);
            painter.line_segment([p(5.2, 2.0), p(2.8, 4.5)], thin);
        }
        IconKind::ZoomIn => {
            painter.circle_stroke(p(-1.0, -1.0), 3.8 * s, stroke);
            painter.line_segment([p(1.8, 1.8), p(5.2, 5.2)], stroke);
            painter.line_segment([p(-2.8, -1.0), p(0.8, -1.0)], thin);
            painter.line_segment([p(-1.0, -2.8), p(-1.0, 0.8)], thin);
        }
        IconKind::ZoomOut => {
            painter.circle_stroke(p(-1.0, -1.0), 3.8 * s, stroke);
            painter.line_segment([p(1.8, 1.8), p(5.2, 5.2)], stroke);
            painter.line_segment([p(-2.8, -1.0), p(0.8, -1.0)], thin);
        }
        IconKind::ZoomReset => {
            let r = Rect::from_min_max(p(-4.8, -4.8), p(4.8, 4.8));
            painter.rect_stroke(r, CornerRadius::same(2), stroke, StrokeKind::Middle);
            painter.circle_filled(p(0.0, 0.0), 1.6 * s, fg);
        }
        IconKind::Report => {
            // 3 vertical bars + baseline
            painter.line_segment([p(-5.5, 4.8), p(5.5, 4.8)], thin);
            painter.rect_filled(
                Rect::from_min_max(p(-4.5, 0.5), p(-2.2, 4.6)),
                CornerRadius::ZERO,
                fg,
            );
            painter.rect_filled(
                Rect::from_min_max(p(-1.1, -3.5), p(1.2, 4.6)),
                CornerRadius::ZERO,
                fg,
            );
            painter.rect_filled(
                Rect::from_min_max(p(2.3, -1.2), p(4.6, 4.6)),
                CornerRadius::ZERO,
                fg,
            );
        }
        IconKind::Network => {
            // 3 connected network nodes
            let top = p(0.0, -4.2);
            let bl = p(-4.2, 3.5);
            let br = p(4.2, 3.5);
            painter.line_segment([top, bl], thin);
            painter.line_segment([top, br], thin);
            painter.line_segment([bl, br], thin);
            for node in [top, bl, br] {
                painter.circle_filled(node, 1.9 * s, fg);
            }
        }
        IconKind::Compare => {
            // Side-by-side split-pane frame with opposing diff bars
            let frame = Rect::from_min_max(p(-5.5, -4.5), p(5.5, 4.5));
            painter.rect_stroke(frame, CornerRadius::same(1), thin, StrokeKind::Middle);
            painter.line_segment([p(0.0, -4.5), p(0.0, 4.5)], thin);
            painter.line_segment([p(-4.0, -1.8), p(-1.5, -1.8)], stroke);
            painter.line_segment([p(-4.0, 1.5), p(-2.0, 1.5)], thin);
            painter.line_segment([p(1.5, -1.8), p(4.0, -1.8)], thin);
            painter.line_segment([p(1.5, 1.5), p(4.0, 1.5)], stroke);
        }
        IconKind::Help => {
            // Circle badge with `?` stem + dot
            painter.circle_stroke(p(0.0, 0.0), 5.2 * s, thin);
            let q = vec![
                p(-1.6, -1.8),
                p(-0.8, -3.0),
                p(0.8, -3.0),
                p(1.8, -1.8),
                p(1.2, -0.4),
                p(0.0, 0.4),
                p(0.0, 1.5),
            ];
            painter.add(egui::Shape::line(q, thin));
            painter.circle_filled(p(0.0, 3.2), 0.85 * s, fg);
        }
        IconKind::ChevronRight => {
            let pts = vec![p(-2.0, -4.0), p(2.4, 0.0), p(-2.0, 4.0)];
            painter.add(egui::Shape::line(pts, stroke));
        }
        IconKind::ChevronDown => {
            let pts = vec![p(-4.0, -2.0), p(0.0, 2.4), p(4.0, -2.0)];
            painter.add(egui::Shape::line(pts, stroke));
        }
        IconKind::MenuLines => {
            for dy in [-3.6_f32, 0.0, 3.6] {
                painter.line_segment([p(-4.8, dy), p(4.8, dy)], stroke);
            }
        }
        IconKind::Close => {
            painter.line_segment([p(-3.8, -3.8), p(3.8, 3.8)], stroke);
            painter.line_segment([p(3.8, -3.8), p(-3.8, 3.8)], stroke);
        }
        IconKind::Check => {
            let pts = vec![p(-4.6, 0.2), p(-1.2, 3.8), p(4.8, -3.6)];
            painter.add(egui::Shape::line(pts, stroke));
        }
        IconKind::ReplaceOne => {
            // Top-left source box + curved/angled arrow to bottom-right target box
            let src = Rect::from_min_max(p(-5.2, -5.0), p(-0.6, -0.6));
            painter.rect_stroke(src, CornerRadius::same(1), thin, StrokeKind::Middle);
            let dst = Rect::from_min_max(p(0.6, 0.6), p(5.2, 5.0));
            painter.rect_filled(dst, CornerRadius::same(1), fill_bg);
            painter.rect_stroke(dst, CornerRadius::same(1), stroke, StrokeKind::Middle);
            painter.line_segment([p(1.4, -3.0), p(4.2, -3.0)], thin);
            painter.line_segment([p(4.2, -3.0), p(4.2, -0.8)], thin);
            painter.line_segment([p(2.7, -1.8), p(4.2, -0.6)], thin);
        }
        IconKind::ReplaceAll => {
            // Top-left source box + double stacked target boxes on bottom-right
            let src = Rect::from_min_max(p(-5.4, -5.2), p(-1.0, -1.0));
            painter.rect_stroke(src, CornerRadius::same(1), thin, StrokeKind::Middle);
            let dst_back = Rect::from_min_max(p(-0.2, -0.2), p(3.8, 3.6));
            painter.rect_stroke(dst_back, CornerRadius::same(1), thin, StrokeKind::Middle);
            let dst_front = Rect::from_min_max(p(1.4, 1.4), p(5.4, 5.2));
            painter.rect_filled(dst_front, CornerRadius::same(1), fill_bg);
            painter.rect_stroke(dst_front, CornerRadius::same(1), stroke, StrokeKind::Middle);
            painter.line_segment([p(1.2, -3.4), p(4.4, -3.4)], thin);
            painter.line_segment([p(4.4, -3.4), p(4.4, -1.2)], thin);
        }
    }
}

/// Left padding inside a menu row before the 14×14 vector icon.
const MENU_LEFT_PAD: f32 = 8.0;
/// Logical width/height of menu vector icons.
const MENU_ICON_SIZE: f32 = 14.0;
/// Dedicated horizontal gutter reserved for the vector icon + spacing before the label text.
pub const ICON_GUTTER_WIDTH: f32 = 22.0;
/// Minimum horizontal clearance between the label and the right-aligned shortcut hint.
const MENU_SHORTCUT_GAP: f32 = 20.0;
/// Right padding inside a menu row.
const MENU_RIGHT_PAD: f32 = 10.0;

/// Render a full-width menu item button with a dedicated 14×14 pixel-snapped vector icon gutter
/// on the left (zero icon/text overlap) and an optional right-aligned keyboard shortcut hint.
pub fn icon_menu_button(
    ui: &mut Ui,
    icon: IconKind,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> Response {
    icon_menu_button_tinted(ui, icon, label, shortcut, enabled, None)
}

/// Render a full-width menu item button with a custom accent color for the vector icon when enabled
/// (e.g., green for insert/add or red for delete/clear).
pub fn icon_menu_button_tinted(
    ui: &mut Ui,
    icon: IconKind,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
    accent: Option<Color32>,
) -> Response {
    let font_id = TextStyle::Button.resolve(ui.style());
    let text_color = if enabled {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let weak_color = ui.visuals().weak_text_color();

    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id.clone(), text_color);
    let shortcut_galley = shortcut.map(|sc| {
        ui.painter()
            .layout_no_wrap(sc.to_string(), font_id, weak_color)
    });

    let sc_width = shortcut_galley
        .as_ref()
        .map_or(0.0, |g| MENU_SHORTCUT_GAP + g.size().x);
    let content_width =
        MENU_LEFT_PAD + ICON_GUTTER_WIDTH + label_galley.size().x + sc_width + MENU_RIGHT_PAD;
    let desired_w = content_width.max(ui.available_width()).max(140.0);
    let desired_h = (label_galley.size().y + 8.0).max(24.0);

    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(desired_w, desired_h), sense);

    if ui.is_rect_visible(rect) {
        let hovered = enabled && (resp.hovered() || resp.has_focus());
        let fill_bg = if hovered {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().window_fill
        };
        if hovered {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(4), fill_bg);
        }

        let icon_fg = if enabled {
            accent.unwrap_or(text_color)
        } else {
            weak_color
        };
        let icon_center = pos2(
            rect.left() + MENU_LEFT_PAD + MENU_ICON_SIZE * 0.5,
            rect.center().y,
        );
        paint_icon(
            ui.painter(),
            icon,
            icon_center,
            icon_fg,
            fill_bg,
            MENU_ICON_SIZE,
        );

        let label_pos = pos2(
            rect.left() + MENU_LEFT_PAD + ICON_GUTTER_WIDTH,
            rect.center().y - label_galley.size().y * 0.5,
        );
        ui.painter().galley(label_pos, label_galley, text_color);

        if let Some(sc_galley) = shortcut_galley {
            let sc_pos = pos2(
                rect.right() - MENU_RIGHT_PAD - sc_galley.size().x,
                rect.center().y - sc_galley.size().y * 0.5,
            );
            ui.painter().galley(sc_pos, sc_galley, weak_color);
        }
    }

    resp
}

/// Render a submenu trigger (`ui.menu_button`) with a leading 14×14 vector icon in the same
/// `ICON_GUTTER_WIDTH` column so submenu labels align vertically with `icon_menu_button` rows.
pub fn icon_submenu_button(
    ui: &mut Ui,
    icon: IconKind,
    label: &str,
    enabled: bool,
    accent: Option<Color32>,
    add_contents: impl FnOnce(&mut Ui),
) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let (icon_rect, _) = ui.allocate_exact_size(
                vec2(MENU_LEFT_PAD + ICON_GUTTER_WIDTH - 4.0, 22.0),
                Sense::hover(),
            );
            let fg = if enabled {
                accent.unwrap_or_else(|| ui.visuals().text_color())
            } else {
                ui.visuals().weak_text_color()
            };
            let icon_center = pos2(
                icon_rect.left() + MENU_LEFT_PAD + MENU_ICON_SIZE * 0.5,
                icon_rect.center().y,
            );
            paint_icon(
                ui.painter(),
                icon,
                icon_center,
                fg,
                ui.visuals().window_fill,
                MENU_ICON_SIZE,
            );
            ui.menu_button(label, add_contents);
        });
    });
}

/// Render a compact icon-only toolbar/chip button with a centered pixel-snapped vector icon.
/// Uses `accent` when `enabled == true` and `ui.visuals().weak_text_color()` when disabled.
pub fn icon_button(
    ui: &mut Ui,
    icon: IconKind,
    enabled: bool,
    accent: Option<Color32>,
    min_size: egui::Vec2,
    tooltip: &str,
) -> Response {
    let resp = ui
        .add_enabled(enabled, egui::Button::new("").min_size(min_size))
        .on_hover_text(tooltip);
    let fg = if enabled {
        accent.unwrap_or_else(|| ui.visuals().text_color())
    } else {
        ui.visuals().weak_text_color()
    };
    let fill_bg = if resp.hovered() && enabled {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    let icon_size = (min_size.y - 8.0).clamp(11.0, MENU_ICON_SIZE);
    paint_icon(
        ui.painter(),
        icon,
        resp.rect.center(),
        fg,
        fill_bg,
        icon_size,
    );
    resp
}

/// Render a menu checkbox row with a crisp 14×14 vector icon between the checkbox and label.
pub fn icon_menu_checkbox(
    ui: &mut Ui,
    icon: IconKind,
    checked: &mut bool,
    label: &str,
    active_accent: Option<Color32>,
) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let mut resp = ui.checkbox(checked, "");
        let (icon_rect, icon_resp) =
            ui.allocate_exact_size(vec2(MENU_ICON_SIZE, MENU_ICON_SIZE), Sense::click());
        let fg = if *checked {
            active_accent.unwrap_or_else(|| ui.visuals().text_color())
        } else {
            ui.visuals().text_color()
        };
        paint_icon(
            ui.painter(),
            icon,
            icon_rect.center(),
            fg,
            ui.visuals().window_fill,
            MENU_ICON_SIZE,
        );
        let label_resp = ui.add(egui::Label::new(label).sense(Sense::click()));
        if icon_resp.clicked() || label_resp.clicked() {
            *checked = !*checked;
            resp.mark_changed();
        }
        resp
    })
    .inner
}

/// Render a standard bordered button with a leading 14×14 vector icon (zero overlap) and label.
/// Uses `accent` for the icon when `enabled == true` and `ui.visuals().weak_text_color()` when disabled.
pub fn icon_labeled_button(
    ui: &mut Ui,
    icon: IconKind,
    label: &str,
    enabled: bool,
    accent: Option<Color32>,
) -> Response {
    let font_id = TextStyle::Button.resolve(ui.style());
    let text_color = if enabled {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, text_color);
    let left_pad = 8.0_f32;
    let gutter = 20.0_f32;
    let right_pad = 10.0_f32;
    let desired = vec2(
        left_pad + gutter + label_galley.size().x + right_pad,
        (label_galley.size().y + 8.0).max(24.0),
    );
    let resp = ui.add_enabled(enabled, egui::Button::new("").min_size(desired));
    if ui.is_rect_visible(resp.rect) {
        let fg = if enabled {
            accent.unwrap_or(text_color)
        } else {
            ui.visuals().weak_text_color()
        };
        let fill_bg = if resp.hovered() && enabled {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        let icon_center = pos2(resp.rect.left() + left_pad + 6.5, resp.rect.center().y);
        paint_icon(
            ui.painter(),
            icon,
            icon_center,
            fg,
            fill_bg,
            ICON_SIZE_INLINE,
        );
        let label_pos = pos2(
            resp.rect.left() + left_pad + gutter,
            resp.rect.center().y - label_galley.size().y * 0.5,
        );
        ui.painter().galley(label_pos, label_galley, text_color);
    }
    resp
}

/// Render a toggleable toolbar button with a leading vector icon and active selection fill/stroke.
pub fn icon_toggle_labeled_button(
    ui: &mut Ui,
    icon: IconKind,
    label: &str,
    active: bool,
    enabled: bool,
    accent: Color32,
) -> Response {
    let font_id = TextStyle::Button.resolve(ui.style());
    let text_color = if !enabled {
        ui.visuals().weak_text_color()
    } else if active {
        accent
    } else {
        ui.visuals().text_color()
    };
    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, text_color);
    let left_pad = 8.0_f32;
    let gutter = 20.0_f32;
    let right_pad = 10.0_f32;
    let desired = vec2(
        left_pad + gutter + label_galley.size().x + right_pad,
        (label_galley.size().y + 8.0).max(24.0),
    );
    let mut btn = egui::Button::new("").min_size(desired);
    if active && enabled {
        btn = btn
            .fill(accent.gamma_multiply(0.18))
            .stroke(Stroke::new(1.0_f32, accent));
    }
    let resp = ui.add_enabled(enabled, btn);
    if ui.is_rect_visible(resp.rect) {
        let fill_bg = if active && enabled {
            accent.gamma_multiply(0.18)
        } else if resp.hovered() && enabled {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        let icon_center = pos2(resp.rect.left() + left_pad + 6.5, resp.rect.center().y);
        paint_icon(
            ui.painter(),
            icon,
            icon_center,
            text_color,
            fill_bg,
            ICON_SIZE_INLINE,
        );
        let label_pos = pos2(
            resp.rect.left() + left_pad + gutter,
            resp.rect.center().y - label_galley.size().y * 0.5,
        );
        ui.painter().galley(label_pos, label_galley, text_color);
    }
    resp
}
