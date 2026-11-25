//! Procedural `|` (HL7 pipe) + RIDGID-style plumbing pipe wrench application icon (`Phase 18`).

include!("icon_render.rs");

/// Generate an anti-aliased `egui::IconData` (`size × size` RGBA) for the desktop window title bar
/// and taskbar switcher (`Phase 18`).
pub fn generate_app_icon(size: u32) -> egui::IconData {
    egui::IconData {
        rgba: render_icon_rgba(size),
        width: size,
        height: size,
    }
}
