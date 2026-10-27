//! Status and request files between the running app and the settings
//! window, which is a separate process and never drives Display Scaling,
//! audio taps or the battery helper itself (same pattern as
//! `display-status.json` / `display-set.json`).
//!
//! The app rewrites each status file every couple of seconds while the
//! module runs; the settings window writes a request file and the app,
//! which polls it every 150 ms, applies requests that are fresh and new
//! (by their `at` stamp).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Display Scaling: displays, modes, pending changes (written by the app).
pub const SCALING_STATUS_FILE: &str = "display-scaling-status.json";
/// Display Scaling: apply / confirm / revert (written by the settings window).
pub const SCALING_REQUEST_FILE: &str = "display-scaling-request.json";
/// Sound: devices, apps, permission (written by the app).
pub const AUDIO_STATUS_FILE: &str = "audio-status.json";
/// Sound: default device, device and app levels (written by the settings window).
pub const AUDIO_REQUEST_FILE: &str = "audio-request.json";

/// Menu bar: items and their sections (written by the app).
pub const MENUBAR_STATUS_FILE: &str = "menubar-status.json";
/// Menu bar: list / move items (written by the settings window).
pub const MENUBAR_REQUEST_FILE: &str = "menubar-request.json";

/// Notch Shelf widgets: latest widget results, reminder lists, permission
/// states (written by the app while the shelf runs).
pub const NOTCH_STATUS_FILE: &str = "notch-status.json";
/// Notch Shelf widgets: refresh a widget, show a Home page (written by the
/// settings window).
pub const NOTCH_REQUEST_FILE: &str = "notch-request.json";
/// Settings window: switch to a page (written by the app's tray menu when
/// the window is already open).
pub const SETTINGS_REQUEST_FILE: &str = "settings-request.json";

/// The app itself: permissions as the app sees them, input-hook health and
/// one status line per module (written by the app every 2 s).
pub const APP_STATUS_FILE: &str = "app-status.json";
/// The app itself: restart, launch / capture a workspace, check for
/// updates (written by the settings window).
pub const APP_REQUEST_FILE: &str = "app-request.json";
/// The last update check (`pm_update::UpdateState`; written by whoever ran
/// it, the app's daily check or the settings window's "Check now").
pub const UPDATE_STATUS_FILE: &str = "update-status.json";

/// A status older than this means the app (or the module) isn't running.
pub const STATUS_FRESH_MS: u64 = 10_000;
/// Requests older than this are ignored.
pub const REQUEST_FRESH_MS: u64 = 5_000;

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A payload with the time it was written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamped<T> {
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub data: T,
}

impl<T> Stamped<T> {
    pub fn is_fresh(&self, max_age_ms: u64) -> bool {
        now_ms().saturating_sub(self.at) <= max_age_ms
    }
}

/// `file` next to `config_path`.
pub fn path_for(config_path: &Path, file: &str) -> PathBuf {
    config_path.with_file_name(file)
}

/// Write `data` (stamped now) into `file` next to `config_path`, atomically.
/// Returns the stamp.
pub fn write<T: Serialize>(config_path: &Path, file: &str, data: &T) -> Result<u64> {
    let mut at = now_ms();
    // Two requests in the same millisecond must still differ.
    if let Some(prev) = read_raw_at(config_path, file) {
        if prev >= at {
            at = prev + 1;
        }
    }
    let json = serde_json::to_string_pretty(&Stamped { at, data })?;
    crate::write_atomic(&path_for(config_path, file), &json)?;
    Ok(at)
}

fn read_raw_at(config_path: &Path, file: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct At {
        at: u64,
    }
    let s = std::fs::read_to_string(path_for(config_path, file)).ok()?;
    serde_json::from_str::<At>(&s).ok().map(|a| a.at)
}

/// Read `file` next to `config_path`.
pub fn read<T: DeserializeOwned>(config_path: &Path, file: &str) -> Option<Stamped<T>> {
    let s = std::fs::read_to_string(path_for(config_path, file)).ok()?;
    serde_json::from_str(&s).ok()
}

/// Read `file` if it was written within `max_age_ms`.
pub fn read_fresh<T: DeserializeOwned>(config_path: &Path, file: &str, max_age_ms: u64) -> Option<T> {
    read::<T>(config_path, file).filter(|s| s.is_fresh(max_age_ms)).map(|s| s.data)
}

/// Polls a request file: yields each fresh request once.
#[derive(Debug, Default)]
pub struct RequestReader {
    last_at: Option<u64>,
    mtime: Option<std::time::SystemTime>,
}

impl RequestReader {
    /// Start after whatever is in the file now (old requests are never replayed).
    pub fn new(config_path: &Path, file: &str) -> Self {
        let p = path_for(config_path, file);
        RequestReader { last_at: read_raw_at(config_path, file), mtime: std::fs::metadata(p).and_then(|m| m.modified()).ok() }
    }

    /// The request written since the last call, if any and fresh.
    pub fn poll<T: DeserializeOwned>(&mut self, config_path: &Path, file: &str) -> Option<T> {
        let m = std::fs::metadata(path_for(config_path, file)).and_then(|m| m.modified()).ok();
        if m == self.mtime {
            return None;
        }
        self.mtime = m;
        let r = read::<T>(config_path, file)?;
        if Some(r.at) == self.last_at {
            return None;
        }
        self.last_at = Some(r.at);
        r.is_fresh(REQUEST_FRESH_MS).then_some(r.data)
    }
}

// ---------------------------------------------------------------------------
// Display Scaling

/// What the running Display Scaling module reports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScalingStatus {
    pub running: bool,
    /// HiDPI through a virtual display is possible on this system.
    pub hidpi_supported: bool,
    pub displays: Vec<display_scaling::DisplayModes>,
    /// Unconfirmed mode changes (with their countdown).
    pub pending: Vec<display_scaling::PendingChange>,
    /// The last request failed with this.
    pub error: Option<String>,
}

/// A change the settings window asks the app to make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ScalingRequest {
    /// Switch a display to one of its modes; reverted unless confirmed.
    Apply { display_id: String, mode: display_scaling::Mode, permanent: bool },
    /// Keep a pending change (and remember it as the display's preferred mode).
    Confirm { token: display_scaling::Token },
    /// Undo a pending change now.
    Revert { token: display_scaling::Token },
}

// ---------------------------------------------------------------------------
// Sound

/// What the running audio module reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioStatus {
    pub running: bool,
    pub outputs: Vec<audio_control::AudioDevice>,
    pub inputs: Vec<audio_control::AudioDevice>,
    /// Apps playing audio (only while per-app volume is on).
    pub apps: Vec<audio_control::AudioApp>,
    pub per_app_supported: bool,
    pub capture_permission: audio_control::CapturePermission,
    pub error: Option<String>,
}

impl Default for AudioStatus {
    fn default() -> Self {
        AudioStatus {
            running: false,
            outputs: Vec::new(),
            inputs: Vec::new(),
            apps: Vec::new(),
            per_app_supported: false,
            capture_permission: audio_control::CapturePermission::NotApplicable,
            error: None,
        }
    }
}

/// A change the settings window asks the app to make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AudioRequest {
    SetDefault { kind: audio_control::DeviceKind, uid: String },
    DeviceVolume { uid: String, volume: f32 },
    DeviceMute { uid: String, muted: bool },
    AppVolume { bundle_id: String, volume: f32 },
    AppMute { bundle_id: String, muted: bool },
    /// Show the macOS audio-capture permission prompt (from the app, which
    /// owns the permission).
    RequestPermission,
}

// ---------------------------------------------------------------------------
// Menu bar

/// What the running menu bar manager reports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MenuBarStatus {
    pub running: bool,
    /// Accessibility (listing items on macOS 26+, moving them).
    pub accessibility: bool,
    pub screen_recording: bool,
    /// The items were listed (only while the settings window asks for them).
    pub listed: bool,
    pub items: Vec<menubar_manager::MenuBarItem>,
    /// Items that should be visible but are behind the notch.
    pub hidden_by_notch: Vec<menubar_manager::MenuBarItem>,
    /// macOS 26+: separators are as long as still fits.
    pub fitted_mode: bool,
    pub layout_issue: Option<String>,
    /// The last request's error.
    pub error: Option<String>,
}

/// A change the settings window asks the app to make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MenuBarRequest {
    /// List the items for the next few seconds (the settings page is open).
    List,
    /// Move an item into a section (synthesized Cmd-drag).
    Move { item_id: String, section: menubar_manager::Section },
    ShowHidden,
    Hide,
}

// ---------------------------------------------------------------------------
// Notch Shelf widgets

/// What the running Notch Shelf reports about its widgets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotchStatus {
    pub running: bool,
    /// Permission states ("granted", "denied", "notDetermined",
    /// "missingUsageKey", "unavailable", "unknown").
    pub calendar: String,
    pub camera: String,
    pub reminders: String,
    /// Reminder list names (empty until the Reminders widget fetched once).
    pub reminder_lists: Vec<String>,
    /// Latest results of the Script / xbar / Web widgets, by instance id.
    pub widgets: std::collections::BTreeMap<String, WidgetPreview>,
}

/// A widget's latest result, reduced to text for the settings window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WidgetPreview {
    /// Empty when the run succeeded.
    pub error: String,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub value: Option<String>,
    pub unit: String,
    /// 0..1 for a gauge.
    pub progress: Option<f64>,
    pub symbol: Option<String>,
    /// Item titles (lists, xbar menus), at most 12.
    pub items: Vec<String>,
}

impl WidgetPreview {
    pub fn from_content(c: &notch_shelf::WidgetContent) -> Self {
        WidgetPreview {
            error: String::new(),
            title: c.title.clone(),
            subtitle: c.subtitle.clone(),
            body: c.body.clone(),
            value: c.value.clone(),
            unit: c.unit.clone(),
            progress: c.progress,
            symbol: c.symbol.clone(),
            items: c.items.iter().filter(|i| !i.separator && !i.title.trim().is_empty()).take(12).map(|i| i.title.clone()).collect(),
        }
    }

    pub fn from_result(r: &Result<notch_shelf::WidgetContent, String>) -> Self {
        match r {
            Ok(c) => Self::from_content(c),
            Err(e) => WidgetPreview { error: if e.trim().is_empty() { "Failed".into() } else { e.clone() }, ..Default::default() },
        }
    }
}

/// A request from the settings window to the running Notch Shelf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum NotchRequest {
    /// Re-run one widget instance now.
    RefreshWidget { id: String },
    /// Open the notch on Home page `page` (index into the config's pages).
    ShowPage { page: usize },
}

/// A request from the app (tray menu) to an open settings window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SettingsRequest {
    /// Switch to a page ("notch-widgets", "sound", ...).
    ShowPage { page: String },
}

// ---------------------------------------------------------------------------
// The app itself

/// Permissions as the running app sees them. macOS caches some of them per
/// process (Screen Recording, Input Monitoring): a grant made while the app
/// runs only shows up here after a restart, which is how the settings
/// window knows to offer "Restart PowerMac".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppPermissions {
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub screen_recording: bool,
}

/// A slow input-hook handler (see `pm_hook::stats`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SlowHandler {
    pub name: String,
    /// Calls slower than `pm_hook::SLOW_HANDLER` (10 ms).
    pub slow: u64,
    pub calls: u64,
    pub max_ms: u64,
}

/// Health of the shared input hook (`pm_hook::status`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HookHealth {
    /// "idle", "starting", "active", "listenOnly", "disabled", "unavailable".
    pub state: String,
    /// Details (why it is unavailable, whether keys arrive listen-only).
    pub detail: String,
    /// The tap is installed and enabled right now.
    pub healthy: bool,
    /// macOS Secure Event Input (a password field): no keys reach the tap.
    pub secure_input: bool,
    /// How often macOS disabled the tap and it was turned back on.
    pub reenabled: u64,
    pub subscribers: Vec<String>,
    pub slow_handlers: Vec<SlowHandler>,
}

/// Whether a module runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModuleState {
    Running,
    /// On, but a permission it needs is missing (it starts or works fully
    /// once it is granted, some modules only after a restart).
    WaitingForPermission,
    /// On, but it didn't start (see the log).
    Failed,
    #[default]
    Off,
}

/// One module's line in Settings → General → Permissions & Health.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleStatus {
    /// Settings page name ("fancyzones", "window-switcher", ...).
    pub page: String,
    pub name: String,
    pub state: ModuleState,
    /// "needs Accessibility", "2 displays", ...
    pub detail: String,
}

/// What the running app reports about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppStatus {
    pub version: String,
    pub pid: u32,
    /// Unix seconds.
    pub started_at: u64,
    /// The executable (inside PowerMac.app when started from the bundle).
    pub executable: String,
    pub permissions: AppPermissions,
    pub hook: HookHealth,
    pub modules: Vec<ModuleStatus>,
}

/// A request from the settings window to the running app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AppRequest {
    /// Quit cleanly and start again (`open -n` of the bundle), so new
    /// permissions take effect; the settings window reopens on `reopen_page`.
    Restart { reopen_page: Option<String> },
    /// Restore a saved workspace (by id or name).
    LaunchWorkspace { id: String },
    /// Snapshot the desktop as a new workspace and open the editor.
    CaptureWorkspace { name: String },
    OpenWorkspacesEditor,
    /// Check GitHub for a newer version now.
    CheckUpdates,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamped_files_and_requests() {
        let dir = std::env::temp_dir().join(format!("pm-config-bridge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        assert!(read::<AudioStatus>(&cfg, AUDIO_STATUS_FILE).is_none());
        let st = AudioStatus { running: true, per_app_supported: true, ..Default::default() };
        write(&cfg, AUDIO_STATUS_FILE, &st).unwrap();
        assert_eq!(read_fresh::<AudioStatus>(&cfg, AUDIO_STATUS_FILE, STATUS_FRESH_MS), Some(st));
        let j = std::fs::read_to_string(dir.join(AUDIO_STATUS_FILE)).unwrap();
        assert!(j.contains("\"perAppSupported\": true") && j.contains("\"capturePermission\""), "{j}");

        // Requests: each one is seen once; what was there before isn't replayed.
        write(&cfg, AUDIO_REQUEST_FILE, &AudioRequest::RequestPermission).unwrap();
        let mut r = RequestReader::new(&cfg, AUDIO_REQUEST_FILE);
        assert_eq!(r.poll::<AudioRequest>(&cfg, AUDIO_REQUEST_FILE), None);
        let req = AudioRequest::AppVolume { bundle_id: "com.spotify.client".into(), volume: 0.5 };
        let a = write(&cfg, AUDIO_REQUEST_FILE, &req).unwrap();
        let b = write(&cfg, AUDIO_REQUEST_FILE, &req).unwrap();
        assert!(b > a);
        let j = std::fs::read_to_string(dir.join(AUDIO_REQUEST_FILE)).unwrap();
        assert!(j.contains("\"action\": \"appVolume\"") && j.contains("\"bundleId\""), "{j}");
        // mtime granularity: force a change the reader notices.
        r.mtime = None;
        assert_eq!(r.poll::<AudioRequest>(&cfg, AUDIO_REQUEST_FILE), Some(req));
        r.mtime = None;
        assert_eq!(r.poll::<AudioRequest>(&cfg, AUDIO_REQUEST_FILE), None);

        let s = ScalingRequest::Confirm { token: display_scaling::Token(7) };
        write(&cfg, SCALING_REQUEST_FILE, &s).unwrap();
        assert_eq!(read::<ScalingRequest>(&cfg, SCALING_REQUEST_FILE).unwrap().data, s);
        let j = std::fs::read_to_string(dir.join(SCALING_REQUEST_FILE)).unwrap();
        assert!(j.contains("\"action\": \"confirm\""), "{j}");
        let s = ScalingRequest::Apply {
            display_id: "LG#1".into(),
            mode: display_scaling::Mode { width: 1920, height: 1080, pixel_width: 3840, pixel_height: 2160, refresh: 60.0, hidpi: true },
            permanent: false,
        };
        write(&cfg, SCALING_REQUEST_FILE, &s).unwrap();
        assert_eq!(read::<ScalingRequest>(&cfg, SCALING_REQUEST_FILE).unwrap().data, s);
        assert!(std::fs::read_to_string(dir.join(SCALING_REQUEST_FILE)).unwrap().contains("\"displayId\""));

        let m = MenuBarRequest::Move { item_id: "com.apple.menuextra.battery#0".into(), section: menubar_manager::Section::AlwaysHidden };
        write(&cfg, MENUBAR_REQUEST_FILE, &m).unwrap();
        assert_eq!(read::<MenuBarRequest>(&cfg, MENUBAR_REQUEST_FILE).unwrap().data, m);
        assert!(std::fs::read_to_string(dir.join(MENUBAR_REQUEST_FILE)).unwrap().contains("\"alwaysHidden\""));

        let n = NotchRequest::ShowPage { page: 2 };
        write(&cfg, NOTCH_REQUEST_FILE, &n).unwrap();
        assert_eq!(read::<NotchRequest>(&cfg, NOTCH_REQUEST_FILE).unwrap().data, n);
        assert!(std::fs::read_to_string(dir.join(NOTCH_REQUEST_FILE)).unwrap().contains("\"showPage\""));
        let sr = SettingsRequest::ShowPage { page: "notch-widgets".into() };
        write(&cfg, SETTINGS_REQUEST_FILE, &sr).unwrap();
        assert_eq!(read::<SettingsRequest>(&cfg, SETTINGS_REQUEST_FILE).unwrap().data, sr);
        let content = notch_shelf::WidgetContent { title: "Disk".into(), value: Some("42".into()), unit: "%".into(), progress: Some(0.42), ..Default::default() };
        let mut ns = NotchStatus { running: true, reminders: "granted".into(), ..Default::default() };
        ns.widgets.insert("a".into(), WidgetPreview::from_result(&Ok(content)));
        ns.widgets.insert("b".into(), WidgetPreview::from_result(&Err("Timed out".into())));
        write(&cfg, NOTCH_STATUS_FILE, &ns).unwrap();
        let back: NotchStatus = read_fresh(&cfg, NOTCH_STATUS_FILE, STATUS_FRESH_MS).unwrap();
        assert_eq!(back, ns);
        assert_eq!((back.widgets["a"].value.as_deref(), back.widgets["b"].error.as_str()), (Some("42"), "Timed out"));

        let req = AppRequest::Restart { reopen_page: Some("welcome".into()) };
        write(&cfg, APP_REQUEST_FILE, &req).unwrap();
        assert_eq!(read::<AppRequest>(&cfg, APP_REQUEST_FILE).unwrap().data, req);
        let j = std::fs::read_to_string(dir.join(APP_REQUEST_FILE)).unwrap();
        assert!(j.contains("\"action\": \"restart\"") && j.contains("\"reopenPage\""), "{j}");
        let req = AppRequest::LaunchWorkspace { id: "{A}".into() };
        write(&cfg, APP_REQUEST_FILE, &req).unwrap();
        assert_eq!(read::<AppRequest>(&cfg, APP_REQUEST_FILE).unwrap().data, req);
        let st = AppStatus {
            version: "1.0.0".into(),
            permissions: AppPermissions { accessibility: true, ..Default::default() },
            hook: HookHealth { state: "active".into(), healthy: true, ..Default::default() },
            modules: vec![ModuleStatus { page: "fancyzones".into(), name: "FancyZones".into(), state: ModuleState::WaitingForPermission, detail: "needs Accessibility".into() }],
            ..Default::default()
        };
        write(&cfg, APP_STATUS_FILE, &st).unwrap();
        assert_eq!(read_fresh::<AppStatus>(&cfg, APP_STATUS_FILE, STATUS_FRESH_MS), Some(st));
        assert!(std::fs::read_to_string(dir.join(APP_STATUS_FILE)).unwrap().contains("\"waitingForPermission\""));

        let old = Stamped { at: 1, data: MenuBarRequest::List };
        assert!(!old.is_fresh(REQUEST_FRESH_MS));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
