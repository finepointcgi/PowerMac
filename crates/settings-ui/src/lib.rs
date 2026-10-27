//! PowerMac's settings window (eframe/egui).
//!
//! It runs as its own process (`powermac settings`, started from the tray
//! menu) so its event loop doesn't compete with the tray app's main loop. It
//! only edits files: `config.json` (via [`pm_config::AppConfig`]) and
//! `fancyzones.json` next to it. Edits are saved automatically ~300 ms after
//! the last change, and the running app's config watcher applies them.

mod app;
pub mod editor;
pub mod model;
pub mod permissions;
mod platform;
mod record;
#[cfg(test)]
mod shots;
pub(crate) use pm_ui::theme;
pub(crate) use pm_ui::widgets;
mod mu_color;
mod zone_style;

use std::path::PathBuf;
use std::sync::OnceLock;

static CLI_EXE: OnceLock<PathBuf> = OnceLock::new();

/// The `powermac` executable this window belongs to. With it the window
/// asks a fresh `powermac permissions --json` for the live permission state
/// (macOS caches some answers per process), runs `powermac mwb doctor` and
/// starts the app or the Workspaces editor when the app isn't running.
/// Without it (tests) those fall back to in-process checks or are disabled.
pub fn set_cli_exe(exe: PathBuf) {
    let _ = CLI_EXE.set(exe);
}

pub(crate) fn cli_exe() -> Option<&'static PathBuf> {
    CLI_EXE.get()
}

/// Open the settings window for the config file at `config_path` and block
/// until it is closed. Must be called on the main thread.
pub fn run(config_path: PathBuf) -> anyhow::Result<()> {
    run_with_page(config_path, None)
}

/// [`run`], opening on the page named `page` ("notch-widgets", "sound",
/// a page title, ...; unknown names open the General page).
pub fn run_with_page(config_path: PathBuf, page: Option<String>) -> anyhow::Result<()> {
    let start = page.as_deref().and_then(|n| {
        let p = model::Page::from_name(n);
        if p.is_none() {
            log::warn!("settings: no page named {n:?}");
        }
        p.map(|p| (p, n.to_string()))
    });
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PowerMac Settings")
            .with_app_id("com.powermac.settings")
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([720.0, 520.0])
            .with_icon(platform::icon())
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "PowerMac Settings",
        options,
        Box::new(move |_cc| {
            platform::become_foreground_app();
            let mut app = app::SettingsApp::new(config_path);
            if let Some((_, name)) = &start {
                app.open_named(name);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("settings window: {e}"))
}

/// Open the FancyZones layout editor for the `fancyzones.json` next to
/// `config_path`, starting on monitor `monitor` (an id from
/// `fancyzones::Monitor::id`; default: the main monitor). Blocks until the
/// window is closed. Must be called on the main thread.
pub fn run_editor(config_path: PathBuf, monitor: Option<String>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FancyZones Layout Editor")
            .with_app_id("com.powermac.fancyzones-editor")
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(platform::icon())
            .with_active(true),
        ..Default::default()
    };
    let fz_path = model::fancyzones_path(&config_path);
    eframe::run_native(
        "FancyZones Layout Editor",
        options,
        Box::new(move |_cc| {
            platform::become_foreground_app();
            Ok(Box::new(editor::EditorApp::new(fz_path, monitor)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("layout editor window: {e}"))
}
