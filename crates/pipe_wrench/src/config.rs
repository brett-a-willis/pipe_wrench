//! Persisted application configuration and display options (G-6, §5.1, §5.3, §5.5, FO-1, FS-4, FS-5).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use hl7_core::{
    CustomPhiRule, ExportEncoding, MllpEnvelope, ReportDefinition, ReportError, SaveTerminator,
};
use hl7_net::{ListenerProfile, SenderProfile};
use serde::{Deserialize, Serialize};

/// Default maximum number of records loaded into a single active UI page/window (`§5.1`, `NFR-1`).
///
/// Set `max_records_per_page = 0` for Unlimited.
pub const DEFAULT_MAX_RECORDS_PER_PAGE: usize = 10_000;

/// Minimum global UI zoom factor (`75%`, `D-28`).
pub const MIN_UI_ZOOM: f32 = 0.75;
/// Maximum global UI zoom factor (`200%`, `D-28`).
pub const MAX_UI_ZOOM: f32 = 2.0;
/// Default global UI zoom factor (`100%`, `D-28`).
pub const DEFAULT_UI_ZOOM: f32 = 1.0;
/// Step increment for UI zoom in/out (`10%`, `D-28`).
pub const UI_ZOOM_STEP: f32 = 0.10;

/// Persisted display options and pane layout settings (§5.1, §5.3, §5.5, G-6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Display Empty Fields in the field breakdown pane (default `false`, §5.3).
    pub display_empty_fields: bool,
    /// Display Compound Fields header row before its components (default `true`, §5.3).
    pub display_compound_fields: bool,
    /// Display Field Descriptions from the dictionary (default `true`, §5.3).
    pub display_field_descriptions: bool,
    /// Display Segment Descriptions from the dictionary (default `true`, §5.3).
    pub display_segment_descriptions: bool,
    /// Decode escape sequences (`\F\`, `\S\`, `\T\`, `\R\`, `\E\`, `\Xhh..\`, `\.br\`) for read-only display (§5.5).
    pub decode_escape_sequences: bool,
    /// Show the synchronized Raw pane (§5.1).
    pub show_raw_pane: bool,
    /// Dark mode preference (`None` = follow OS / egui default, `Some(true)` = dark, `Some(false)` = light, NFR-7).
    pub dark_mode: Option<bool>,
    /// Option Disk Envelope Characters on save (default `false`, FS-4).
    pub disk_envelope_on_save: bool,
    /// Option SAVE-TERM (`"Preserve"`, `"Cr"`, `"CrLf"`, `"Lf"`, default `"Preserve"`, FS-5).
    pub save_terminator: SaveTerminatorSetting,
    /// Configured MLLP envelope bytes (SB, ED, EB) (NET-1, FO-7, FS-4).
    pub envelope_bytes: [u8; 3],
    /// User-editable file dialog filter patterns (FO-1, FO-2, FS-3).
    pub file_filter_patterns: Vec<String>,
    /// Active filter index in `file_filter_patterns`.
    pub active_filter_index: usize,
    /// Optional path to a user Z-segment TOML dictionary file (§5.4).
    pub user_dictionary_path: Option<PathBuf>,
    /// Optional external editor executable for FO-12 fallback.
    pub external_editor_path: Option<PathBuf>,
    /// Persisted width of the left Record list pane in logical pixels (§5.1).
    pub record_pane_width: f32,
    /// Persisted width of the middle Segment list pane in logical pixels (§5.1).
    pub segment_pane_width: f32,
    /// Persisted height of the bottom Raw pane in logical pixels (§5.1).
    pub raw_pane_height: f32,
    /// Confirm before deleting segments or records (default `true`, EDIT-7, EDIT-8).
    pub prompt_before_deleting: bool,
    /// Soft memory warning limit for undo history in bytes (default 128 MiB, EDIT-1, D-12).
    pub undo_warning_bytes: usize,
    /// Default output character encoding for report exports (default `Utf8`, RPT-9, D-7).
    pub export_encoding: ExportEncoding,
    /// Maximum number of records displayed per active UI page in the `Records` list
    /// (default `10,000`; `0` = Unlimited, `§5.1`, `NFR-1`).
    pub max_records_per_page: usize,
    /// Global UI zoom factor (`0.75..=2.0`, default `1.0`, `D-28`).
    pub ui_zoom: f32,
    /// Show inline editor control-character markers (`[CR]`, `[LF]`, `[VT]`, `[FS]`, `·`, `→`)
    /// in the Raw Message Editor (`¶` toggle, default `false`, `D-28`).
    pub show_control_chars: bool,
    /// User-configured additional custom HL7 PHI field paths persisted across sessions.
    pub custom_phi_rules: Vec<CustomPhiRule>,
}

/// Serializable wrapper for [`SaveTerminator`] (FS-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SaveTerminatorSetting {
    /// Preserve detected source terminator (CRLF for clipboard/network).
    #[default]
    Preserve,
    /// Always write CR (`0x0D`).
    Cr,
    /// Always write CRLF (`0x0D 0x0A`).
    CrLf,
    /// Always write LF (`0x0A`).
    Lf,
}

impl From<SaveTerminatorSetting> for SaveTerminator {
    fn from(s: SaveTerminatorSetting) -> Self {
        match s {
            SaveTerminatorSetting::Preserve => Self::Preserve,
            SaveTerminatorSetting::Cr => Self::Cr,
            SaveTerminatorSetting::CrLf => Self::CrLf,
            SaveTerminatorSetting::Lf => Self::Lf,
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            display_empty_fields: false,
            display_compound_fields: true,
            display_field_descriptions: true,
            display_segment_descriptions: true,
            decode_escape_sequences: false,
            show_raw_pane: true,
            dark_mode: None,
            disk_envelope_on_save: false,
            save_terminator: SaveTerminatorSetting::Preserve,
            envelope_bytes: [0x0B, 0x1C, 0x0D],
            file_filter_patterns: vec![
                "*.*".to_string(),
                "*.dat".to_string(),
                "*.txt".to_string(),
                "*.hl7".to_string(),
            ],
            active_filter_index: 0,
            user_dictionary_path: None,
            external_editor_path: None,
            record_pane_width: 430.0,
            segment_pane_width: 240.0,
            raw_pane_height: 180.0,
            prompt_before_deleting: true,
            undo_warning_bytes: hl7_core::DEFAULT_UNDO_WARNING_BYTES,
            export_encoding: ExportEncoding::Utf8,
            max_records_per_page: DEFAULT_MAX_RECORDS_PER_PAGE,
            ui_zoom: DEFAULT_UI_ZOOM,
            show_control_chars: false,
            custom_phi_rules: Vec::new(),
        }
    }
}

impl AppSettings {
    /// Add or update a custom PHI path rule (deduplicated case-insensitively by canonical path).
    pub fn add_custom_phi_rule(&mut self, rule: CustomPhiRule) -> Result<(), String> {
        let addr: hl7_core::FieldAddress = rule
            .path
            .trim()
            .parse()
            .map_err(|e| format!("Invalid HL7 path '{}': {e}", rule.path.trim()))?;
        if addr.is_header_delimiter_field() {
            return Err(format!("Cannot scrub header delimiter field '{addr}'."));
        }
        let canonical = addr.to_string();
        if let Some(existing) = self
            .custom_phi_rules
            .iter_mut()
            .find(|r| r.path.eq_ignore_ascii_case(&canonical))
        {
            existing.mode = rule.mode;
        } else {
            self.custom_phi_rules.push(CustomPhiRule {
                path: canonical,
                mode: rule.mode,
            });
        }
        Ok(())
    }

    /// Remove a custom PHI rule by index.
    pub fn remove_custom_phi_rule(&mut self, index: usize) {
        if index < self.custom_phi_rules.len() {
            self.custom_phi_rules.remove(index);
        }
    }

    /// Clear all custom PHI rules.
    pub fn clear_custom_phi_rules(&mut self) {
        self.custom_phi_rules.clear();
    }
    /// Return the configured [`MllpEnvelope`].
    pub fn mllp_envelope(&self) -> MllpEnvelope {
        MllpEnvelope {
            start_block: self.envelope_bytes[0],
            end_data: self.envelope_bytes[1],
            end_block: self.envelope_bytes[2],
        }
    }

    /// Resolve the default settings file path inside the platform config directory (G-6).
    pub fn default_config_path() -> Option<PathBuf> {
        ProjectDirs::from("io", "pipe_wrench", "pipe_wrench")
            .map(|dirs| dirs.config_dir().join("settings.toml"))
    }

    /// Load settings from `path`, falling back to defaults if absent or invalid.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Load settings from the platform config directory.
    pub fn load_default() -> Self {
        Self::default_config_path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Persist settings to `path` as TOML (G-6).
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, body)?;
        Ok(())
    }

    /// Persist settings to the default platform config path.
    pub fn save_default(&self) {
        if let Some(path) = Self::default_config_path() {
            let _ = self.save_to(&path);
        }
    }

    /// Set the global UI zoom factor, clamped to `0.75..=2.0` (`D-28`).
    pub fn set_ui_zoom(&mut self, zoom: f32) {
        let clamped = zoom.clamp(MIN_UI_ZOOM, MAX_UI_ZOOM);
        self.ui_zoom = (clamped * 100.0).round() / 100.0;
    }

    /// Increase the global UI zoom factor by `+10%`, clamped to `2.0` (`D-28`).
    pub fn zoom_in(&mut self) {
        self.set_ui_zoom(self.ui_zoom + UI_ZOOM_STEP);
    }

    /// Decrease the global UI zoom factor by `-10%`, clamped to `0.75` (`D-28`).
    pub fn zoom_out(&mut self) {
        self.set_ui_zoom(self.ui_zoom - UI_ZOOM_STEP);
    }

    /// Reset the global UI zoom factor to `100%` (`1.0`) (`D-28`).
    pub fn reset_zoom(&mut self) {
        self.ui_zoom = DEFAULT_UI_ZOOM;
    }

    /// Return the global UI zoom factor as a rounded integer percentage (e.g. `100`) (`D-28`).
    pub fn zoom_percent(&self) -> u32 {
        (self.ui_zoom.clamp(MIN_UI_ZOOM, MAX_UI_ZOOM) * 100.0).round() as u32
    }

    /// Add a new file dialog filter pattern, rejecting empty or duplicate patterns (FO-1, FO-2).
    pub fn add_filter_pattern(&mut self, pattern: impl Into<String>) -> Result<(), String> {
        let trimmed = pattern.into().trim().to_string();
        if trimmed.is_empty() {
            return Err("Filter pattern cannot be empty.".to_string());
        }
        if self
            .file_filter_patterns
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&trimmed))
        {
            return Err(format!("Duplicate filter pattern '{trimmed}'."));
        }
        self.file_filter_patterns.push(trimmed);
        Ok(())
    }

    /// Remove a filter pattern by index (keeping at least one pattern).
    pub fn remove_filter_pattern(&mut self, index: usize) -> bool {
        if self.file_filter_patterns.len() <= 1 || index >= self.file_filter_patterns.len() {
            return false;
        }
        self.file_filter_patterns.remove(index);
        if self.active_filter_index >= self.file_filter_patterns.len() {
            self.active_filter_index = self.file_filter_patterns.len() - 1;
        }
        true
    }
}

/// Directory where saved report definitions (`*.toml`) are persisted (RPT-6, G-6).
pub fn default_reports_dir() -> Option<PathBuf> {
    ProjectDirs::from("io", "pipe_wrench", "pipe_wrench")
        .map(|dirs| dirs.config_dir().join("reports"))
}

/// Save a [`ReportDefinition`] as `<dir>/<name>.toml` after validating its name (RPT-6, G-6).
pub fn save_report_definition_in(
    def: &ReportDefinition,
    dir: &Path,
) -> Result<PathBuf, ReportError> {
    ReportDefinition::validate_name(&def.name)?;
    std::fs::create_dir_all(dir).map_err(|e| ReportError::Export(e.to_string()))?;
    let toml_body = def.to_toml()?;
    let path = dir.join(format!("{}.toml", def.name.trim()));
    std::fs::write(&path, toml_body).map_err(|e| ReportError::Export(e.to_string()))?;
    Ok(path)
}

/// Load a [`ReportDefinition`] from a `.toml` file on disk (RPT-6, G-6).
pub fn load_report_definition_file(path: &Path) -> Result<ReportDefinition, ReportError> {
    let body = std::fs::read_to_string(path).map_err(|e| ReportError::Export(e.to_string()))?;
    ReportDefinition::from_toml(&body)
}

/// Save a [`SenderProfile`] as `<dir>/<name>.toml` (NET-20, G-6).
pub fn save_sender_profile_in(profile: &SenderProfile, dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let safe_name = sanitize_profile_filename(&profile.name);
    let path = dir.join(format!("{safe_name}.toml"));
    let body = toml::to_string_pretty(profile)?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Load a [`SenderProfile`] from a `.toml` file on disk (NET-20, G-6).
pub fn load_sender_profile_file(path: &Path) -> anyhow::Result<SenderProfile> {
    let body = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&body)?)
}

/// Save a [`ListenerProfile`] as `<dir>/<name>.toml` (NET-10, G-6).
pub fn save_listener_profile_in(profile: &ListenerProfile, dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let safe_name = sanitize_profile_filename(&profile.name);
    let path = dir.join(format!("{safe_name}.toml"));
    let body = toml::to_string_pretty(profile)?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Load a [`ListenerProfile`] from a `.toml` file on disk (NET-10, G-6).
pub fn load_listener_profile_file(path: &Path) -> anyhow::Result<ListenerProfile> {
    let body = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&body)?)
}

fn sanitize_profile_filename(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
