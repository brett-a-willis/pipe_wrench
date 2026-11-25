//! Dual Dark & Light Modern Developer-Tool Design System (`§5.1`, `NFR-7`, `D-26`).
//!
//! Inspired by Zed and VS Code Dark Modern & Light Modern, respecting the user's
//! `System`, `Dark`, and `Light` theme preference everywhere:
//! - **Dark Mode:** Base canvas `#141519`, side/record panel `#1A1C22`, elevated toolbar/cards `#22252E`,
//!   hover/selection tint `#2A3140`, hair-thin `1px` dividers `#2B303B`, muted secondary text `#8E96A8`,
//!   and clinical blue/teal accent `#38BDF8`.
//! - **Light Mode:** Base canvas `#F6F8FA`, side/record panel `#FFFFFF`, elevated toolbar/cards `#EEF2F6`,
//!   hover/selection tint `#DCE8F8`, hair-thin `1px` dividers `#D0D7DE`, muted secondary text `#57606A`,
//!   and high-contrast clinical blue accent `#0284C7`.

use egui::{Color32, Context, CornerRadius, Frame, Margin, RichText, Stroke, Ui, Visuals};

/// Theme-aware surface and semantic palette for the main application chrome (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    /// Whether this palette is for Dark mode (`true`) or Light mode (`false`).
    pub is_dark: bool,
    /// Base application canvas background (`#141519` dark / `#F6F8FA` light).
    pub base_canvas: Color32,
    /// Side / record panel surface (`#1A1C22` dark / `#FFFFFF` light).
    pub side_panel_bg: Color32,
    /// Elevated toolbar / card surface (`#22252E` dark / `#EEF2F6` light).
    pub elevated_bg: Color32,
    /// Row hover / selection tint (`#2A3140` dark / `#DCE8F8` light).
    pub hover_selection_tint: Color32,
    /// Hair-thin `1px` divider stroke color (`#2B303B` dark / `#D0D7DE` light).
    pub divider: Color32,
    /// Primary text color (`#E5E9F0` dark / `#1F2328` light).
    pub primary_text: Color32,
    /// Muted secondary text color (`#8E96A8` dark / `#57606A` light).
    pub muted_text: Color32,
    /// Clinical blue/teal accent color (`#38BDF8` dark / `#0284C7` light).
    pub accent: Color32,
    /// Positive / insert action accent (`#22C55E` dark / `#16A34A` light, `D-29`).
    pub accent_green: Color32,
    /// Destructive / delete action accent (`#EF4444` dark / `#DC2626` light, `D-29`).
    pub accent_red: Color32,
}

impl ThemePalette {
    /// Dark Modern palette (`#141519` canvas, `#38BDF8` accent).
    pub const DARK: Self = Self {
        is_dark: true,
        base_canvas: Color32::from_rgb(0x14, 0x15, 0x19),
        side_panel_bg: Color32::from_rgb(0x1A, 0x1C, 0x22),
        elevated_bg: Color32::from_rgb(0x22, 0x25, 0x2E),
        hover_selection_tint: Color32::from_rgb(0x2A, 0x31, 0x40),
        divider: Color32::from_rgb(0x2B, 0x30, 0x3B),
        primary_text: Color32::from_rgb(0xE5, 0xE9, 0xF0),
        muted_text: Color32::from_rgb(0x8E, 0x96, 0xA8),
        accent: Color32::from_rgb(0x38, 0xBD, 0xF8),
        accent_green: Color32::from_rgb(0x22, 0xC5, 0x5E),
        accent_red: Color32::from_rgb(0xEF, 0x44, 0x44),
    };

    /// Light Modern palette (`#F6F8FA` canvas, `#0284C7` accent).
    pub const LIGHT: Self = Self {
        is_dark: false,
        base_canvas: Color32::from_rgb(0xF6, 0xF8, 0xFA),
        side_panel_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        elevated_bg: Color32::from_rgb(0xEE, 0xF2, 0xF6),
        hover_selection_tint: Color32::from_rgb(0xDC, 0xE8, 0xF8),
        divider: Color32::from_rgb(0xD0, 0xD7, 0xDE),
        primary_text: Color32::from_rgb(0x1F, 0x23, 0x28),
        muted_text: Color32::from_rgb(0x57, 0x60, 0x6A),
        accent: Color32::from_rgb(0x02, 0x84, 0xC7),
        accent_green: Color32::from_rgb(0x16, 0xA3, 0x4A),
        accent_red: Color32::from_rgb(0xDC, 0x26, 0x26),
    };

    /// Resolve the active [`ThemePalette`] from `egui::Visuals`.
    pub fn for_visuals(visuals: &Visuals) -> Self {
        if visuals.dark_mode {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

/// Dual Dark (`VS Code Dark+` `#1E1E1E`) and Light (`VS Code Light+` `#FFFFFF`) syntax-highlighting
/// palette for the in-place raw HL7 message editor (`D-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorPalette {
    /// Whether this editor palette is for Dark mode.
    pub is_dark: bool,
    /// Editor canvas background (`#1E1E1E` dark / `#FFFFFF` light).
    pub background: Color32,
    /// Editor border stroke (`#2B303B` dark / `#D0D7DE` light).
    pub border: Color32,
    /// Base field text color (`#D4D4D4` dark / `#1F2328` light).
    pub text_fg: Color32,
    /// Text selection background (`#264F78` dark / `#ADD6FF` light).
    pub selection_bg: Color32,
    /// Subtle active segment line highlight (`#2A2D2E` dark / `#F0F6FC` light).
    pub active_line_bg: Color32,
    /// Segment tag (`MSH`, `PID`, `OBX`) color (`#569CD6` dark / `#0550AE` light).
    pub segment_tag: Color32,
    /// Field delimiter (`|`) color (`#C586C0` dark / `#8250DF` light).
    pub delim_field: Color32,
    /// Component delimiter (`^`) color (`#4EC9B0` dark / `#0969DA` light).
    pub delim_component: Color32,
    /// Repetition delimiter (`~`) color (`#DCDCAA` dark / `#9A6700` light).
    pub delim_repetition: Color32,
    /// Escape delimiter (`\`) color (`#CE9178` dark / `#CF222E` light).
    pub delim_escape: Color32,
    /// Full HL7 escape sequence (`\F\`, `\.br\`, `\X0D\`) color (`#D7BA7D` dark / `#B35900` light, `D-28`).
    pub escape_sequence: Color32,
    /// Subcomponent delimiter (`&`) color (`#9CDCFE` dark / `#116329` light).
    pub delim_subcomponent: Color32,
    /// Muted foreground color for inline control character markers (`[CR]`, `[LF]`, `·`, `→`) (`D-28`).
    pub control_char_fg: Color32,
    /// Subtle pill background for inline control character markers (`[CR]`, `[LF]`, `[VT]`, `[FS]`) (`D-28`).
    pub control_char_bg: Color32,
}

impl EditorPalette {
    /// VS Code Dark+ editor palette (`#1E1E1E` background).
    pub const DARK_PLUS: Self = Self {
        is_dark: true,
        background: Color32::from_rgb(0x1E, 0x1E, 0x1E),
        border: Color32::from_rgb(0x2B, 0x30, 0x3B),
        text_fg: Color32::from_rgb(0xD4, 0xD4, 0xD4),
        selection_bg: Color32::from_rgb(0x26, 0x4F, 0x78),
        active_line_bg: Color32::from_rgb(0x2A, 0x2D, 0x2E),
        segment_tag: Color32::from_rgb(0x56, 0x9C, 0xD6),
        delim_field: Color32::from_rgb(0xC5, 0x86, 0xC0),
        delim_component: Color32::from_rgb(0x4E, 0xC9, 0xB0),
        delim_repetition: Color32::from_rgb(0xDC, 0xDC, 0xAA),
        delim_escape: Color32::from_rgb(0xCE, 0x91, 0x78),
        escape_sequence: Color32::from_rgb(0xD7, 0xBA, 0x7D),
        delim_subcomponent: Color32::from_rgb(0x9C, 0xDC, 0xFE),
        control_char_fg: Color32::from_rgb(0x6E, 0x76, 0x81),
        control_char_bg: Color32::from_rgb(0x2B, 0x30, 0x3B),
    };

    /// VS Code Light+ editor palette (`#FFFFFF` background, WCAG-AA high-contrast tokens).
    pub const LIGHT_PLUS: Self = Self {
        is_dark: false,
        background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        border: Color32::from_rgb(0xD0, 0xD7, 0xDE),
        text_fg: Color32::from_rgb(0x1F, 0x23, 0x28),
        selection_bg: Color32::from_rgb(0xAD, 0xD6, 0xFF),
        active_line_bg: Color32::from_rgb(0xF0, 0xF6, 0xFC),
        segment_tag: Color32::from_rgb(0x05, 0x50, 0xAE),
        delim_field: Color32::from_rgb(0x82, 0x50, 0xDF),
        delim_component: Color32::from_rgb(0x09, 0x69, 0xDA),
        delim_repetition: Color32::from_rgb(0x9A, 0x67, 0x00),
        delim_escape: Color32::from_rgb(0xCF, 0x22, 0x2E),
        escape_sequence: Color32::from_rgb(0xB3, 0x59, 0x00),
        delim_subcomponent: Color32::from_rgb(0x11, 0x63, 0x29),
        control_char_fg: Color32::from_rgb(0x6E, 0x77, 0x81),
        control_char_bg: Color32::from_rgb(0xEA, 0xEE, 0xF2),
    };

    /// Resolve the active [`EditorPalette`] from `egui::Visuals`.
    pub fn for_visuals(visuals: &Visuals) -> Self {
        if visuals.dark_mode {
            Self::DARK_PLUS
        } else {
            Self::LIGHT_PLUS
        }
    }
}

/// Semantic pill badge style (`(background_fill, foreground_text, border_stroke)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PillBadgeColors {
    /// Tinted background fill.
    pub bg: Color32,
    /// High-contrast foreground text color.
    pub fg: Color32,
    /// Subtle border color.
    pub border: Color32,
}

/// Compute theme-aware pill badge colors for an HL7 message type (`MSH-9.1` family + `MSH-9.2` trigger event)
/// using the standard `'^'` component delimiter (`D-26`, `D-29`).
pub fn message_type_pill_colors(msg_type: &str, is_dark: bool) -> PillBadgeColors {
    message_type_pill_colors_with_delim(msg_type, '^', is_dark)
}

/// Compute theme-aware pill badge colors for an HL7 message type (`MSH-9.1` family + `MSH-9.2` trigger event)
/// using the record's active `MSH-2[0]` component separator (e.g. `'^'` for `ADT^A01` or `':'` for `ADT:A01`) (`D-26`, `D-29`).
///
/// Each HL7 message family has a dedicated hue band (`ADT` blue, `ORU` emerald/teal-green,
/// `ORM`/`OMG`/`OML` amber/warm gold, `SIU` violet/purple, `DFT` coral/rose, `MDM` indigo/periwinkle,
/// `FHS`/`BHS`/`BTS`/`FTS` warm ochre/bronze, other neutral cool slate), while trigger events (`MSH-9.2`)
/// apply a deterministic hue/lightness/border variation within that family band so e.g. `ADT^A01` and
/// `ADT^A03` (or `ADT:A01` and `ADT:A03`) remain immediately recognizable as `ADT` yet visually distinct at a glance.
pub fn message_type_pill_colors_with_delim(
    msg_type: &str,
    component_sep: char,
    is_dark: bool,
) -> PillBadgeColors {
    let mut parts = msg_type.split([component_sep, '^', '_']);
    let family = parts.next().unwrap_or(msg_type).trim().to_ascii_uppercase();
    let trigger = parts.next().unwrap_or("").trim().to_ascii_uppercase();

    // Curated high-contrast ADT trigger variants within the Blue family
    if family == "ADT" {
        match trigger.as_str() {
            "A01" => {
                return if is_dark {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0x12, 0x36, 0x54),
                        fg: Color32::from_rgb(0x7D, 0xD3, 0xFC),
                        border: Color32::from_rgb(0x02, 0x84, 0xC7),
                    }
                } else {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0xE0, 0xF2, 0xFE),
                        fg: Color32::from_rgb(0x03, 0x69, 0xA1),
                        border: Color32::from_rgb(0x38, 0xBD, 0xF8),
                    }
                };
            }
            "A03" => {
                return if is_dark {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0x1B, 0x26, 0x54),
                        fg: Color32::from_rgb(0xA5, 0xB4, 0xFC),
                        border: Color32::from_rgb(0x4F, 0x46, 0xE5),
                    }
                } else {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0xE0, 0xE7, 0xFF),
                        fg: Color32::from_rgb(0x37, 0x30, 0xA3),
                        border: Color32::from_rgb(0x81, 0x8C, 0xF8),
                    }
                };
            }
            "A04" => {
                return if is_dark {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0x0F, 0x3A, 0x4A),
                        fg: Color32::from_rgb(0x67, 0xE8, 0xF9),
                        border: Color32::from_rgb(0x08, 0x91, 0xB2),
                    }
                } else {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0xCF, 0xFA, 0xFE),
                        fg: Color32::from_rgb(0x0E, 0x74, 0x90),
                        border: Color32::from_rgb(0x22, 0xD3, 0xEE),
                    }
                };
            }
            "A08" => {
                return if is_dark {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0x1E, 0x2F, 0x45),
                        fg: Color32::from_rgb(0x93, 0xC5, 0xFD),
                        border: Color32::from_rgb(0x3B, 0x82, 0xF6),
                    }
                } else {
                    PillBadgeColors {
                        bg: Color32::from_rgb(0xDB, 0xEA, 0xFE),
                        fg: Color32::from_rgb(0x1D, 0x4E, 0xD8),
                        border: Color32::from_rgb(0x60, 0xA5, 0xFA),
                    }
                };
            }
            _ => {}
        }
    }

    // (base_hue_degrees, base_saturation, hue_band_spread)
    let (base_h, base_s, spread): (f32, f32, f32) = match family.as_str() {
        "ADT" => (208.0, 0.72, 24.0),         // Blue family
        "ORU" | "OBX" => (158.0, 0.66, 22.0), // Emerald / Teal-Green
        "ORM" | "OMG" | "OML" | "RDE" | "RAS" => (42.0, 0.78, 16.0), // Amber / Warm Gold
        "SIU" => (272.0, 0.68, 22.0),         // Violet / Purple
        "DFT" => (348.0, 0.72, 18.0),         // Coral / Rose
        "MDM" => (238.0, 0.66, 18.0),         // Indigo / Periwinkle
        "FHS" | "BHS" | "BTS" | "FTS" => (30.0, 0.68, 12.0), // Warm Ochre / Bronze
        _ => (215.0, 0.22, 20.0),             // Neutral Cool Slate
    };

    // Deterministic subtle variation from MSH-9.2 trigger event within the family hue band
    let (h_shift, l_shift) = if trigger.is_empty() {
        (0.0_f32, 0.0_f32)
    } else {
        let mut hash: u32 = 2166136261;
        for b in trigger.bytes() {
            hash ^= u32::from(b);
            hash = hash.wrapping_mul(16777619);
        }
        let norm_h = ((hash % 11) as f32 / 10.0) * 2.0 - 1.0; // -1.0..=1.0
        let norm_l = (((hash >> 8) % 5) as f32 / 4.0) * 2.0 - 1.0; // -1.0..=1.0
        (norm_h * (spread * 0.5), norm_l * 0.045)
    };

    let h = (base_h + h_shift).rem_euclid(360.0);
    if is_dark {
        PillBadgeColors {
            bg: hsl_to_color32(
                h,
                (base_s * 0.78).clamp(0.14, 0.78),
                (0.19 + l_shift).clamp(0.14, 0.25),
            ),
            fg: hsl_to_color32(
                h,
                (base_s * 0.95).clamp(0.20, 0.95),
                (0.78 + l_shift * 0.5).clamp(0.72, 0.85),
            ),
            border: hsl_to_color32(
                h,
                (base_s * 0.90).clamp(0.22, 0.90),
                (0.44 + l_shift).clamp(0.36, 0.54),
            ),
        }
    } else {
        PillBadgeColors {
            bg: hsl_to_color32(
                h,
                (base_s * 0.85).clamp(0.16, 0.88),
                (0.92 + l_shift * 0.4).clamp(0.87, 0.95),
            ),
            fg: hsl_to_color32(
                h,
                (base_s * 0.95).clamp(0.24, 0.95),
                (0.28 + l_shift * 0.4).clamp(0.22, 0.34),
            ),
            border: hsl_to_color32(
                h,
                (base_s * 0.85).clamp(0.22, 0.88),
                (0.64 + l_shift * 0.5).clamp(0.55, 0.74),
            ),
        }
    }
}

fn hsl_to_color32(h_deg: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = (h_deg.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h_prime as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    let to_u8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Compute theme-aware pill badge colors for an HL7 segment tag (`MSH`, `PID`, `OBX`, etc.) (`D-26`).
pub fn segment_tag_pill_colors(tag: &str, is_dark: bool) -> PillBadgeColors {
    match tag.trim().to_ascii_uppercase().as_str() {
        "MSH" | "FHS" | "BHS" | "BTS" | "FTS" => message_type_pill_colors("ADT", is_dark),
        "PID" | "PD1" | "NK1" | "PV1" | "PV2" => message_type_pill_colors("ORM", is_dark),
        "OBR" | "OBX" | "ORC" | "RXA" => message_type_pill_colors("ORU", is_dark),
        _ => message_type_pill_colors("OTHER", is_dark),
    }
}

/// Compute theme-aware pill badge colors for an ACK code (`AA`/`CA`, `AE`/`CE`, `AR`/`CR`) (`D-26`).
pub fn ack_code_pill_colors(code: &str, is_dark: bool) -> PillBadgeColors {
    match code.trim().to_ascii_uppercase().as_str() {
        "AA" | "CA" | "ACK" => message_type_pill_colors("ORU", is_dark),
        "AE" | "CE" => message_type_pill_colors("FHS", is_dark),
        _ => {
            if is_dark {
                PillBadgeColors {
                    bg: Color32::from_rgb(0x45, 0x1A, 0x1A),
                    fg: Color32::from_rgb(0xFC, 0xA5, 0xA5),
                    border: Color32::from_rgb(0xDC, 0x26, 0x26),
                }
            } else {
                PillBadgeColors {
                    bg: Color32::from_rgb(0xFE, 0xE2, 0xE2),
                    fg: Color32::from_rgb(0x99, 0x1B, 0x1B),
                    border: Color32::from_rgb(0xF8, 0x71, 0x71),
                }
            }
        }
    }
}

/// Render a rounded scannable pill badge with `4px` corner rounding and `1px` border (`D-26`).
pub fn render_pill_badge(ui: &mut Ui, label: &str, colors: PillBadgeColors) -> egui::Response {
    Frame::NONE
        .fill(colors.bg)
        .stroke(Stroke::new(1.0_f32, colors.border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(label)
                        .monospace()
                        .small()
                        .strong()
                        .color(colors.fg),
                )
                .selectable(false),
            )
        })
        .response
}

/// Paint a `3px` vertical left accent indicator bar on the active selected row (`D-26`).
pub fn paint_left_accent_bar(ui: &Ui, rect: egui::Rect, accent: Color32) {
    let bar_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + 2.0),
        egui::pos2(rect.left() + 3.0, rect.bottom() - 2.0),
    );
    ui.painter()
        .rect_filled(bar_rect, CornerRadius::same(1), accent);
}

/// Build a two-tone `LayoutJob` for an action button with a colored prefix symbol
/// (e.g. green `+` or red `−`) followed by `label` in the standard button text color.
///
/// When `enabled` is `false`, both the prefix and label render in the muted/disabled text color.
pub fn action_button_job(
    ui: &Ui,
    prefix: &str,
    prefix_color: Color32,
    label: &str,
    enabled: bool,
) -> egui::text::LayoutJob {
    let visuals = ui.visuals();
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let label_color = if enabled {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    let sym_color = if enabled {
        prefix_color
    } else {
        visuals.weak_text_color()
    };

    let mut job = egui::text::LayoutJob::default();
    job.append(
        prefix,
        0.0,
        egui::text::TextFormat {
            font_id: font_id.clone(),
            color: sym_color,
            ..Default::default()
        },
    );
    if !label.is_empty() {
        job.append(
            " ",
            0.0,
            egui::text::TextFormat {
                font_id: font_id.clone(),
                color: label_color,
                ..Default::default()
            },
        );
        job.append(
            label,
            0.0,
            egui::text::TextFormat {
                font_id,
                color: label_color,
                ..Default::default()
            },
        );
    }
    job
}

/// Build a two-tone green `+` action button label (`#22C55E` Dark / `#16A34A` Light) with standard button text.
pub fn add_action_button_job(ui: &Ui, label: &str, enabled: bool) -> egui::text::LayoutJob {
    let palette = ThemePalette::for_visuals(ui.visuals());
    action_button_job(ui, "+", palette.accent_green, label, enabled)
}

/// Build a two-tone red `−` action button label (`#EF4444` Dark / `#DC2626` Light) with standard button text.
pub fn remove_action_button_job(ui: &Ui, label: &str, enabled: bool) -> egui::text::LayoutJob {
    let palette = ThemePalette::for_visuals(ui.visuals());
    action_button_job(ui, "−", palette.accent_red, label, enabled)
}

/// Apply the complete Dual Dark & Light Design System (`ThemePalette`, spacing, rounding, and hair-thin `1px` dividers)
/// to the `egui::Context` (`§5.1`, `NFR-7`, `D-26`).
pub fn apply_design_system(ctx: &Context, dark_mode_pref: Option<bool>) {
    let is_dark = match dark_mode_pref {
        Some(d) => d,
        None => ctx.style().visuals.dark_mode,
    };
    let visuals = build_visuals(is_dark);
    let palette = ThemePalette::for_visuals(&visuals);

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;

    // Spacing, Rounding & Typography (D-26):
    // item_spacing = [8.0, 6.0], button_padding = [10.0, 5.0], interact_size.y = 26.0
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.window_margin = Margin::same(10);

    let _ = palette;
    ctx.set_style(style);
}

/// Build `egui::Visuals` for either Dark Modern (`is_dark = true`) or Light Modern (`is_dark = false`) (`D-26`).
pub fn build_visuals(is_dark: bool) -> Visuals {
    let palette = if is_dark {
        ThemePalette::DARK
    } else {
        ThemePalette::LIGHT
    };
    let mut v = if is_dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    v.dark_mode = is_dark;
    v.panel_fill = palette.base_canvas;
    v.window_fill = palette.elevated_bg;
    v.faint_bg_color = palette.side_panel_bg;
    v.extreme_bg_color = palette.side_panel_bg;
    v.override_text_color = Some(palette.primary_text);
    v.hyperlink_color = palette.accent;

    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_stroke = Stroke::new(1.0_f32, palette.divider);

    v.selection.bg_fill = palette.hover_selection_tint;
    v.selection.stroke = Stroke::new(1.0_f32, palette.accent);

    // 6px rounding on buttons/inputs, hair-thin 1px dividers
    let btn_radius = CornerRadius::same(6);
    v.widgets.noninteractive.bg_fill = palette.base_canvas;
    v.widgets.noninteractive.weak_bg_fill = palette.side_panel_bg;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.divider);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.muted_text);
    v.widgets.noninteractive.corner_radius = btn_radius;

    v.widgets.inactive.bg_fill = palette.elevated_bg;
    v.widgets.inactive.weak_bg_fill = palette.elevated_bg;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.divider);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.primary_text);
    v.widgets.inactive.corner_radius = btn_radius;

    v.widgets.hovered.bg_fill = palette.hover_selection_tint;
    v.widgets.hovered.weak_bg_fill = palette.hover_selection_tint;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, palette.primary_text);
    v.widgets.hovered.corner_radius = btn_radius;

    v.widgets.active.bg_fill = palette.hover_selection_tint;
    v.widgets.active.weak_bg_fill = palette.hover_selection_tint;
    v.widgets.active.bg_stroke = Stroke::new(1.5_f32, palette.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, palette.primary_text);
    v.widgets.active.corner_radius = btn_radius;

    v.widgets.open.bg_fill = palette.elevated_bg;
    v.widgets.open.weak_bg_fill = palette.elevated_bg;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, palette.primary_text);
    v.widgets.open.corner_radius = btn_radius;

    v
}
