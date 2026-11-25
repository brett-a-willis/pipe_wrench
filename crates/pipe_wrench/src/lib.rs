//! Desktop GUI shell library for `pipe_wrench` (Phase 5).
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![warn(missing_docs)]

pub mod app;
pub mod config;
pub mod icon;
pub mod state;
pub mod strings;
pub mod ui;

pub use app::{PipeWrenchApp, ScreenshotConfig};
pub use config::{
    AppSettings, SaveTerminatorSetting, DEFAULT_MAX_RECORDS_PER_PAGE, DEFAULT_UI_ZOOM, MAX_UI_ZOOM,
    MIN_UI_ZOOM, UI_ZOOM_STEP,
};
pub use icon::{encode_ico, generate_app_icon, render_icon_rgba};
pub use state::{
    ActiveFilterRule, DataSource, FilterCombineMode, FindReplaceScope, FindReplaceTab,
    SessionState, PRUNE_CONFIRM_THRESHOLD,
};

/// Parse optional `--screenshot <PATH>` and `--theme dark|light` CLI flags for `pipe_wrench`.
pub fn parse_screenshot_args<I, S>(args: I) -> Option<ScreenshotConfig>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut output_path: Option<std::path::PathBuf> = None;
    let mut dark_mode = true;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        if s == "--screenshot" {
            if let Some(p) = iter.next() {
                output_path = Some(std::path::PathBuf::from(p.as_ref()));
            }
        } else if let Some(p) = s.strip_prefix("--screenshot=") {
            if !p.is_empty() {
                output_path = Some(std::path::PathBuf::from(p));
            }
        } else if s == "--theme" {
            if let Some(t) = iter.next() {
                dark_mode = !t.as_ref().eq_ignore_ascii_case("light");
            }
        } else if let Some(t) = s.strip_prefix("--theme=") {
            dark_mode = !t.eq_ignore_ascii_case("light");
        }
    }
    output_path.map(|path| ScreenshotConfig {
        output_path: path,
        dark_mode,
    })
}

/// Launch the native `eframe` desktop window.
pub fn run_native() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    tracing::info!("Starting pipe_wrench GUI");

    let screenshot_cfg = parse_screenshot_args(std::env::args().skip(1));
    let initial_size = if screenshot_cfg.is_some() {
        [1440.0, 900.0]
    } else {
        [1280.0, 800.0]
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(initial_size)
            .with_min_inner_size([800.0, 500.0])
            .with_icon(std::sync::Arc::new(generate_app_icon(64)))
            .with_title(strings::APP_TITLE),
        ..Default::default()
    };
    eframe::run_native(
        strings::APP_TITLE,
        native_options,
        Box::new(move |_cc| {
            let app = match screenshot_cfg {
                Some(cfg) => PipeWrenchApp::new_for_screenshot(cfg),
                None => PipeWrenchApp::new(),
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}
