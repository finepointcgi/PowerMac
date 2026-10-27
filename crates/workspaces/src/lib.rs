//! PowerToys **Workspaces** for PowerMac: capture the current desktop (apps,
//! window frames, monitors, states, Spaces, documents) as a named
//! workspace, and restore it later — launching what isn't running, waiting
//! for the windows and putting them back where they were, on the right
//! display.
//!
//! * [`capture`] / [`capture_and_save`] — snapshot ([`capture::CaptureOptions`]).
//! * [`launch`] — restore by name or id, with a progress callback
//!   ([`launch::LaunchEvent`]) and a final [`launch::LaunchReport`].
//! * [`list`], [`store`] — `workspaces.json` in the data directory (atomic
//!   writes).
//! * [`import_powertoys_file`] — PowerToys' `workspaces.json`.
//! * [`run_editor`] — the editor window (egui, PowerMac theme).
//! * [`create_shortcut`] — a desktop shortcut running
//!   `powermac workspace launch <id>`.
//!
//! macOS notes: windows are read and moved through Accessibility (the
//! permission PowerMac already has for FancyZones); no Screen Recording is
//! needed (previews are drawn from rectangles and app icons). Command-line
//! arguments of running apps are not captured (GUI apps are started by
//! LaunchServices, which doesn't pass them; `open --args` only reaches apps
//! that aren't running). Browser URLs need the AppleScript opt-in. Apps are
//! never quit: "other windows" can only be minimized or hidden.

pub mod backend;
pub mod capture;
pub mod config;
mod editor;
pub mod geometry;
pub mod import;
pub mod launch;
pub mod matching;
pub mod model;
pub mod platform;
pub mod shortcut;
pub mod store;

#[cfg(test)]
mod shots;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

pub use capture::CaptureOptions;
pub use config::{Config, Hotkey};
pub use editor::{run_editor, run_editor_with, EditorApp};
pub use geometry::{MissingMonitor, Monitor, Rect};
pub use import::{ImportContext, ImportReport};
pub use launch::{AppStatus, LaunchEvent, LaunchOptions, LaunchReport};
pub use model::{App, AppOptions, ExistingWindows, OtherWindows, WindowState, Workspace};
pub use shortcut::ShortcutKind;

/// Whether this process may read and move other apps' windows
/// (Accessibility on macOS; always true on Windows).
pub fn has_permissions() -> bool {
    platform::has_permissions()
}

/// Snapshot the current desktop as a new (unsaved) workspace.
pub fn capture(name: &str, config: &Config) -> Result<Workspace> {
    let backend = platform::Native::new(&config.capture);
    capture::capture_with(&backend, name, &config.capture)
}

/// [`capture`] and add it to the data directory's workspaces. An empty
/// `name` gets "Workspace N".
pub fn capture_and_save(data_dir: &Path, name: &str, config: &Config) -> Result<Workspace> {
    let list = store::load(data_dir)?;
    let name = if name.trim().is_empty() { store::unique_name(&list, "Workspace") } else { name.trim().to_string() };
    let ws = capture(&name, config)?;
    store::upsert(data_dir, ws.clone())?;
    Ok(ws)
}

/// The saved workspaces.
pub fn list(data_dir: &Path) -> Result<Vec<Workspace>> {
    store::load(data_dir)
}

/// Restore a saved workspace (by id or name; see [`store::find`]) and record
/// its launch time. `progress` is called on this thread for every change.
pub fn launch(data_dir: &Path, name_or_id: &str, config: &Config, progress: &dyn Fn(&LaunchEvent)) -> Result<LaunchReport> {
    let list = store::load(data_dir)?;
    let ws = store::find(&list, name_or_id).cloned().ok_or_else(|| anyhow!("no workspace named \"{name_or_id}\""))?;
    let report = launch_workspace(&ws, config, progress)?;
    // Re-read: the editor may have changed the file meanwhile.
    let mut list = store::load(data_dir)?;
    if let Some(w) = list.iter_mut().find(|w| w.id == ws.id) {
        w.last_launched_time = Some(model::now_unix());
        store::save(data_dir, &list)?;
    }
    Ok(report)
}

/// Restore a workspace that isn't (necessarily) saved.
pub fn launch_workspace(ws: &Workspace, config: &Config, progress: &dyn Fn(&LaunchEvent)) -> Result<LaunchReport> {
    let backend = platform::Native::new(&config.capture);
    launch::run(ws, &backend, &config.launch, progress)
}

/// Import PowerToys' `workspaces.json` into the data directory.
pub fn import_powertoys_file(data_dir: &Path, file: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let mut list = store::load(data_dir)?;
    let mut ctx = ImportContext::current();
    ctx.existing_ids = list.iter().map(|w| w.id.clone()).collect();
    let mut report = import::import_powertoys(&text, &ctx)?;
    for ws in &mut report.workspaces {
        ws.name = store::unique_name(&list, &ws.name);
        list.push(ws.clone());
    }
    store::save(data_dir, &list)?;
    Ok(report)
}

/// Create a desktop shortcut that launches `ws`; returns its path.
pub fn create_shortcut(ws: &Workspace, config: &Config) -> Result<PathBuf> {
    let dir = shortcut::desktop_dir().ok_or_else(|| anyhow!("no desktop folder"))?;
    shortcut::create(ws, &dir, &shortcut::default_exe(), config.shortcut_kind)
}
