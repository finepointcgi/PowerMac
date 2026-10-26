//! PowerMac's settings window (eframe/egui).
//!
//! It runs as its own process (`powermac settings`, started from the tray
//! menu) so its event loop doesn't compete with the tray app's main loop. It
//! only edits files: `config.json` (via [`pm_config::AppConfig`]) and
//! `fancyzones.json` next to it. Edits are saved automatically ~300 ms after
//! the last change, and the running app's config watcher applies them.

mod app;
pub mod model;
mod platform;
mod record;
mod widgets;

use std::path::PathBuf;

/// Open the settings window for the config file at `config_path` and block
/// until it is closed. Must be called on the main thread.
pub fn run(config_path: PathBuf) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PowerMac Settings")
            .with_app_id("com.powermac.settings")
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(platform::icon())
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "PowerMac Settings",
        options,
        Box::new(move |_cc| {
            platform::become_foreground_app();
            Ok(Box::new(app::SettingsApp::new(config_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("settings window: {e}"))
}
