//! The app's top-level config file: which modules run and their settings.
//! FancyZones keeps its own file (`fancyzones.json`) next to it, and so do
//! PowerRename / New+ (`file-tools.json`, see `file_tools::Config`).
//!
//! Every section has a serde default, so config files written by older
//! versions (without the newer sections) keep loading.

pub mod bridge;
pub mod login;
pub mod mwb_help;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Module<T> {
    pub enabled: bool,
    #[serde(flatten)]
    pub settings: T,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Toggle {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(rename = "mouseWithoutBorders")]
    pub mwb: Module<mwb::Config>,
    pub fancyzones: Toggle,
    pub awake: Module<awake::AwakeConfig>,
    /// Has its own `enabled` switch.
    pub scroll_reverser: scroll_reverser::Config,
    /// Find My Mouse, Mouse Highlighter, Crosshairs (each has its own `enabled`).
    pub mouse_utils: mouse_utils::Config,
    /// Key and shortcut remaps, paste as plain text, Windows-style keys
    /// (has its own `enabled`).
    pub keyboard_manager: keyboard_manager::Config,
    /// Pointer acceleration / speed and mouse-button mappings (has its own `enabled`).
    pub mouse_tuning: mouse_tuning::Config,
    /// Brightness / contrast / volume of displays (has its own `enabled`).
    pub display_control: display_control::Config,
    /// Clipboard history and its picker (has its own `enabled`).
    pub clipboard_history: clipboard_history::Config,
    /// The drop shelf in the notch (has its own `enabled`).
    pub notch_shelf: notch_shelf::Config,
    pub color_picker: Module<color_picker::Config>,
    pub text_extractor: Module<text_extractor::Config>,
    /// Alt+Tab-style window switcher (has its own `enabled`).
    pub window_switcher: window_switcher::Config,
    /// Display modes, HiDPI on non-Retina displays, custom resolutions
    /// (has its own `enabled`; off by default).
    pub display_scaling: display_scaling::Config,
    /// Default-device rules, per-app volume, per-app routing and the mixer
    /// (has its own `enabled`; routes and the mixer are empty / off by default).
    pub audio: audio_control::Config,
    /// Battery charge limit (has its own `enabled`; off by default: nothing
    /// is written to the SMC and no helper is installed until it's turned on
    /// and the helper installed from Settings → Battery).
    pub battery_limit: battery_limit::Config,
    /// Trackpad gestures that run automation actions (nothing bound by
    /// default, and with nothing bound the trackpad isn't read).
    pub gestures: gestures::GesturesConfig,
    /// Window previews on Dock icons (has its own `enabled`).
    pub dock_previews: dock_previews::Config,
    /// Snippets typed from abbreviations (has its own `enabled`; off by default).
    pub text_expander: text_expander::Config,
    /// Hide menu bar icons behind a chevron (has its own `enabled`; off by default).
    pub menu_bar: menubar_manager::Config,
    /// Saved desktops of apps and windows (has its own `enabled`; the
    /// workspaces themselves are in `workspaces.json`).
    pub workspaces: workspaces::Config,
    /// Checking GitHub for a newer version.
    pub updates: UpdatesConfig,
    /// First-run setup (the welcome window).
    pub setup: SetupConfig,
}

/// `updates` section: a daily check of GitHub Releases (nothing is
/// downloaded or installed automatically).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdatesConfig {
    /// Check once a day while PowerMac runs.
    pub check: bool,
    /// Offer beta versions too.
    pub include_prereleases: bool,
    /// When the last automatic or manual check ran (Unix seconds).
    pub last_check: Option<u64>,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        UpdatesConfig { check: true, include_prereleases: false, last_check: None }
    }
}

/// `setup` section: the welcome window (`powermac settings --page welcome`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupConfig {
    /// The welcome window was finished or closed; it isn't opened at
    /// startup any more (unless a permission is missing, see below).
    pub completed: bool,
    /// Open the setup's Permissions step at startup when a permission an
    /// enabled module needs is missing.
    pub remind_missing_permissions: bool,
    /// The version that completed the setup.
    pub completed_version: String,
}

impl Default for SetupConfig {
    fn default() -> Self {
        SetupConfig { completed: false, remind_missing_permissions: true, completed_version: String::new() }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            mwb: Module { enabled: true, settings: mwb::Config::default() },
            fancyzones: Toggle { enabled: true },
            awake: Module { enabled: false, settings: awake::AwakeConfig::default() },
            // Windows has no "natural scrolling" default to undo, so only reverse on macOS.
            scroll_reverser: scroll_reverser::Config { enabled: cfg!(target_os = "macos"), ..Default::default() },
            mouse_utils: mouse_utils::Config::default(),
            keyboard_manager: keyboard_manager::Config::default(),
            mouse_tuning: mouse_tuning::Config::default(),
            display_control: display_control::Config::default(),
            clipboard_history: clipboard_history::Config::default(),
            notch_shelf: notch_shelf::Config::default(),
            color_picker: Module { enabled: color_picker::SUPPORTED, settings: color_picker::Config::default() },
            text_extractor: Module { enabled: text_extractor::SUPPORTED, settings: text_extractor::Config::default() },
            window_switcher: window_switcher::Config::default(),
            display_scaling: display_scaling::Config::default(),
            audio: audio_control::Config::default(),
            battery_limit: battery_limit::Config::default(),
            gestures: gestures::GesturesConfig::default(),
            dock_previews: dock_previews::Config::default(),
            text_expander: text_expander::Config::default(),
            menu_bar: menubar_manager::Config::default(),
            workspaces: workspaces::Config::default(),
            updates: UpdatesConfig::default(),
            setup: SetupConfig::default(),
        }
    }
}

/// The folder that holds `config.json` (and the other PowerMac data files:
/// `clipboard/`, the notch shelf, the color history, ...).
pub fn data_dir_for(config_path: &Path) -> PathBuf {
    config_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// `file-tools.json` (PowerRename / New+ settings) next to `config.json`.
pub fn file_tools_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("file-tools.json")
}

impl AppConfig {
    /// `<config dir>/PowerMac/config.json`
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("PowerMac").join("config.json"))
    }

    /// Defaults with a new MWB key, this machine in the matrix, and a fixed machine id
    /// (MWB persists the id; a random one per start confuses peers).
    pub fn with_new_key() -> Self {
        let mut c = AppConfig::default();
        c.mwb.settings.key = mwb::create_random_key();
        c.mwb.settings.matrix = vec![c.mwb.settings.effective_machine_name()];
        c.mwb.settings.machine_id = Some(rand_id());
        c
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
    }

    /// Atomic write (temp file + rename) so the config watcher never reads half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Written next to `config.json` by the settings window (or anything else) to
/// ask the running app for a Mouse Without Borders discovery sweep now.
pub const MWB_DISCOVER_REQUEST_FILE: &str = "mwb-discover.json";
/// Written next to `config.json` by the running app: Mouse Without Borders
/// connection status for the settings window.
pub const MWB_STATUS_FILE: &str = "mwb-status.json";
/// The app rewrites the status file at least this often while it runs.
pub const MWB_STATUS_REFRESH_SECS: u64 = 5;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A request for a discovery sweep (see [`MWB_DISCOVER_REQUEST_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverRequest {
    /// Milliseconds since the Unix epoch; old requests are ignored.
    pub at: u64,
}

impl DiscoverRequest {
    pub fn now() -> Self {
        DiscoverRequest { at: now_ms() }
    }

    pub fn is_fresh(&self, max_age_ms: u64) -> bool {
        now_ms().saturating_sub(self.at) <= max_age_ms
    }

    /// Write a request next to `config_path`.
    pub fn write_for(config_path: &Path) -> Result<()> {
        let path = config_path.with_file_name(MWB_DISCOVER_REQUEST_FILE);
        std::fs::write(&path, serde_json::to_string(&Self::now())?).with_context(|| format!("writing {}", path.display()))
    }
}

/// One machine in [`MwbStatus`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MwbMachine {
    pub name: String,
    /// IP address (or `ip:port` when it uses another port).
    pub address: String,
    pub connected: bool,
    /// Found automatically this session.
    pub discovered: bool,
}

/// Mouse Without Borders status as the running app last saw it (see [`MWB_STATUS_FILE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MwbStatus {
    /// When it was written (milliseconds since the Unix epoch).
    pub at: u64,
    /// The engine is running.
    pub running: bool,
    pub machine_name: String,
    pub searching: bool,
    pub auto_discover: bool,
    /// When the last search started (milliseconds since the Unix epoch).
    pub last_search: Option<u64>,
    pub machines: Vec<MwbMachine>,
    /// Addresses that answered with a different security key.
    pub other_key: Vec<String>,
}

impl MwbStatus {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name(MWB_STATUS_FILE)
    }

    pub fn stamp(mut self) -> Self {
        self.at = now_ms();
        self
    }

    /// Whether the app wrote it recently enough to be running.
    pub fn is_fresh(&self) -> bool {
        now_ms().saturating_sub(self.at) <= MWB_STATUS_REFRESH_SECS * 3 * 1000
    }

    /// The status next to `config_path`, if any.
    pub fn load_for(config_path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(Self::path_for(config_path)).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Atomic write next to `config_path`.
    pub fn save_for(&self, config_path: &Path) -> Result<()> {
        let path = Self::path_for(config_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// Written next to `config.json` by the running app: the displays Display
/// Control sees, with their current values (for the settings window, which
/// never talks to the monitors itself).
pub const DISPLAY_STATUS_FILE: &str = "display-status.json";
/// Written next to `config.json` by the settings window: values to set on a
/// display (the running app applies them).
pub const DISPLAY_REQUEST_FILE: &str = "display-set.json";

/// Display Control status as the running app last saw it (see [`DISPLAY_STATUS_FILE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DisplayStatus {
    /// When it was written (milliseconds since the Unix epoch).
    pub at: u64,
    pub running: bool,
    /// The brightness / volume key hook is installed.
    pub keys_active: bool,
    pub displays: Vec<display_control::DisplayInfo>,
}

impl DisplayStatus {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name(DISPLAY_STATUS_FILE)
    }

    pub fn stamp(mut self) -> Self {
        self.at = now_ms();
        self
    }

    /// Written within the last 10 s (the app rewrites it every 2 s).
    pub fn is_fresh(&self) -> bool {
        now_ms().saturating_sub(self.at) <= 10_000
    }

    pub fn load_for(config_path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(Self::path_for(config_path)).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save_for(&self, config_path: &Path) -> Result<()> {
        write_atomic(&Self::path_for(config_path), &serde_json::to_string_pretty(self)?)
    }
}

/// One change requested by the settings window (see [`DISPLAY_REQUEST_FILE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DisplayRequest {
    /// Milliseconds since the Unix epoch; old requests are ignored.
    pub at: u64,
    /// `DisplayInfo::id`.
    pub id: String,
    pub brightness: Option<u8>,
    pub contrast: Option<u8>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
}

impl DisplayRequest {
    pub fn new(id: &str) -> Self {
        DisplayRequest { at: now_ms(), id: id.to_string(), ..Default::default() }
    }

    pub fn is_fresh(&self, max_age_ms: u64) -> bool {
        now_ms().saturating_sub(self.at) <= max_age_ms
    }

    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name(DISPLAY_REQUEST_FILE)
    }

    /// Write the request next to `config_path` (atomically).
    pub fn write_for(&self, config_path: &Path) -> Result<()> {
        write_atomic(&Self::path_for(config_path), &serde_json::to_string(self)?)
    }

    pub fn load_for(config_path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(Self::path_for(config_path)).ok()?;
        serde_json::from_str(&s).ok()
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load, change and save the config file at `path`.
pub fn edit_at(path: &Path, f: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let mut c = AppConfig::load_or_default(path)?;
    f(&mut c);
    c.save(path)
}

fn rand_id() -> u32 {
    let b: [u8; 4] = mwb::crypto::random_bytes();
    // Avoid the reserved ids NONE (0) and ALL (255).
    u32::from_le_bytes(b).max(256)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The battery charge limit section (dropped in 1.0.x, restored since)
    /// loads from configs written by either version.
    #[test]
    fn battery_limit_section_loads() {
        let dir = std::env::temp_dir().join(format!("pm-config-battery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let json = r#"{"windowSwitcher":{"showDelayMs":150},"batteryLimit":{"enabled":true,"limitPercent":80,"sailingPercent":3}}"#;
        std::fs::write(&path, json).unwrap();
        let c = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(c.window_switcher.show_delay_ms, 150);
        assert!(c.battery_limit.enabled);
        assert_eq!((c.battery_limit.limit_percent, c.battery_limit.sailing_percent), (80, 3));
        // Fields added later keep their defaults.
        assert!(c.battery_limit.limit_via_discharge);
        assert_eq!(c.battery_limit.heat_protection_celsius, 35);
        c.save(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["batteryLimit"]["limitViaDischarge"], true);
        assert_eq!(v["batteryLimit"]["limitPercent"], 80);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Routes, the mixer, hotkeys and gestures round-trip with camelCase
    /// keys, and partial sections keep their defaults.
    #[test]
    fn automation_audio_and_gesture_sections() {
        let mut c = AppConfig::default();
        assert!(c.audio.app_routes.is_empty() && !c.audio.mixer.enabled);
        assert!(c.gestures.enabled && c.gestures.bindings.is_empty() && c.gestures.swallow_clicks);
        assert!(c.keyboard_manager.hotkeys.is_empty());
        c.audio.app_routes.insert(
            "com.spotify.client".into(),
            audio_control::AppRoute { name: "Spotify".into(), keep_default: true, ..audio_control::AppRoute::to(audio_control::DeviceMatch::name("AirPods")) },
        );
        c.audio.mixer.enabled = true;
        c.audio.mixer.sources.push(audio_control::MixSource { id: "s1".into(), name: "Music".into(), ..Default::default() });
        c.audio.mixer.outputs.push(audio_control::MixOutput { id: "o1".into(), name: "Speakers".into(), ..Default::default() });
        c.audio.mixer.set_send("s1", "o1", -6.0, false);
        c.keyboard_manager.hotkeys.push(keyboard_manager::Hotkey {
            from: "Ctrl+Alt+B".parse().unwrap(),
            action: keyboard_manager::automation::Action::Shell(keyboard_manager::ShellAction { command: "make".into(), ..Default::default() }),
            app: None,
            enabled: true,
        });
        c.gestures.bindings.push(gestures::GestureBinding::new(
            "3-finger-tap".parse().unwrap(),
            keyboard_manager::automation::Action::Shortcut { keys: "Cmd+W".into() },
        ));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["audio"]["appRoutes"]["com.spotify.client"]["keepDefault"], true);
        assert_eq!(v["audio"]["mixer"]["sends"][0]["gainDb"], -6.0);
        assert_eq!(v["keyboardManager"]["hotkeys"][0]["action"]["type"], "shell");
        assert_eq!(v["gestures"]["bindings"][0]["gesture"], "3-finger-tap");
        assert_eq!(v["gestures"]["swallowClicks"], true);
        let back: AppConfig = serde_json::from_value(v).unwrap();
        assert_eq!(back.audio, c.audio);
        assert_eq!(back.keyboard_manager.hotkeys, c.keyboard_manager.hotkeys);
        assert_eq!(back.gestures, c.gestures);
        let p: AppConfig = serde_json::from_str(r#"{"gestures": {"bindings": [{"gesture": "4-finger-swipe-up", "action": {"type": "openUri", "uri": "https://example.com"}}]}}"#).unwrap();
        assert!(p.gestures.enabled && p.gestures.swallow_clicks && p.gestures.bindings.len() == 1);
    }

    #[test]
    fn roundtrip_and_partial_files() {
        let c = AppConfig::with_new_key();
        let json = serde_json::to_string(&c).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mwb.settings.key, c.mwb.settings.key);
        assert!(mwb::crypto::is_key_valid(&back.mwb.settings.key));

        let partial: AppConfig = serde_json::from_str(
            r#"{"mouseWithoutBorders":{"enabled":true,"key":"abcdefghjkmnpqrs","matrix":["mac","pc"]},
                "awake":{"enabled":true,"mode":{"type":"indefinite"},"keepDisplayOn":true}}"#,
        )
        .unwrap();
        assert_eq!(partial.mwb.settings.matrix, ["mac", "pc"]);
        assert_eq!(partial.mwb.settings.port, mwb::DEFAULT_PORT);
        assert!(partial.awake.enabled && partial.awake.settings.keep_display_on);
        assert!(partial.fancyzones.enabled);
        // Discovery is on by default, also for configs written before it existed.
        assert!(partial.mwb.settings.auto_discover);
        assert_eq!(partial.mwb.settings.discovery_interval_secs, 60);
        assert_eq!(partial.mwb.settings.discovery_interval_connected_secs, 300);
    }

    /// A config.json written before the newer modules existed (only the
    /// original sections, some of them partial) still loads, and every new
    /// section takes its defaults.
    #[test]
    fn old_config_files_keep_loading() {
        let old = r#"{
            "mouseWithoutBorders": {"enabled": true, "key": "abcdefghjkmnpqrs", "matrix": ["mac", "pc"]},
            "fancyzones": {"enabled": true},
            "awake": {"enabled": false, "mode": {"type": "indefinite"}},
            "scrollReverser": {"enabled": true, "reverseMouseVertical": true},
            "mouseUtils": {},
            "keyboardManager": {"enabled": true, "remapKeys": [{"from": "CapsLock", "to": "Esc"}]}
        }"#;
        let c: AppConfig = serde_json::from_str(old).unwrap();
        let d = AppConfig::default();
        assert_eq!(c.mwb.settings.matrix, ["mac", "pc"]);
        assert!(c.scroll_reverser.enabled && !c.scroll_reverser.smooth_scrolling);
        assert_eq!(c.scroll_reverser.smooth_step_px, 60);
        assert_eq!(c.keyboard_manager.remap_keys.len(), 1);
        assert!(!c.keyboard_manager.windows_style.enabled);
        assert_eq!(c.keyboard_manager.windows_style, keyboard_manager::WindowsStylePreset::default());
        assert_eq!(c.mouse_tuning, d.mouse_tuning);
        assert_eq!(c.display_control, d.display_control);
        assert_eq!(c.clipboard_history, d.clipboard_history);
        assert!(!c.clipboard_history.enabled);
        assert_eq!(c.notch_shelf, d.notch_shelf);
        assert_eq!(c.color_picker.enabled, d.color_picker.enabled);
        assert_eq!(c.color_picker.settings, d.color_picker.settings);
        assert_eq!(c.text_extractor.settings, d.text_extractor.settings);
        assert_eq!(c.window_switcher, d.window_switcher);
        assert_eq!(c.display_scaling, d.display_scaling);
        assert!(!c.display_scaling.enabled && !c.text_expander.enabled && !c.battery_limit.enabled);
        assert_eq!(c.audio, d.audio);
        assert_eq!(c.battery_limit, d.battery_limit);
        assert_eq!(c.gestures, d.gestures);
        assert_eq!(c.dock_previews, d.dock_previews);
        assert_eq!(c.text_expander, d.text_expander);
        assert_eq!(c.menu_bar, d.menu_bar);
        assert!(!c.menu_bar.enabled);
        assert_eq!(c.keyboard_manager.hyper_key, keyboard_manager::HyperKeyConfig::default());
        assert_eq!(c.workspaces, workspaces::Config::default());
        assert!(c.updates.check && !c.updates.include_prereleases && c.updates.last_check.is_none());
        assert!(!c.setup.completed && c.setup.remind_missing_permissions);

        // New sections are written with camelCase keys and read back.
        let mut c2 = c.clone();
        c2.color_picker.enabled = false;
        c2.color_picker.settings.copied_format = "RGB".into();
        c2.clipboard_history.paused = true;
        c2.window_switcher.show_delay_ms = 200;
        c2.keyboard_manager.windows_style.enabled = true;
        c2.keyboard_manager.windows_style.finder_cut_paste = false;
        c2.keyboard_manager.hyper_key.mode = keyboard_manager::HyperMode::Hyper;
        c2.audio.app_volumes.insert("com.spotify.client".into(), audio_control::AppLevel { volume: 0.5, muted: false });
        c2.text_expander.snippets.push(text_expander::Snippet::new(";sig", "Best,\nAda"));
        let v = serde_json::to_value(&c2).unwrap();
        for k in ["mouseTuning", "displayControl", "clipboardHistory", "notchShelf", "colorPicker", "textExtractor", "windowSwitcher",
                  "displayScaling", "audio", "batteryLimit", "gestures", "dockPreviews", "textExpander", "menuBar", "workspaces", "updates", "setup"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["colorPicker"]["enabled"], false);
        assert_eq!(v["colorPicker"]["copiedFormat"], "RGB");
        assert_eq!(v["keyboardManager"]["windowsStyle"]["finderCutPaste"], false);
        let back: AppConfig = serde_json::from_value(v.clone()).unwrap();
        assert!(!back.color_picker.enabled && back.clipboard_history.paused);
        assert_eq!(back.window_switcher.show_delay_ms, 200);
        assert_eq!(v["keyboardManager"]["hyperKey"]["mode"], "hyper");
        assert_eq!(v["audio"]["appVolumes"]["com.spotify.client"]["volume"], 0.5);
        assert_eq!(back.keyboard_manager.hyper_key.mode, keyboard_manager::HyperMode::Hyper);
        assert_eq!(back.text_expander.snippets.last().unwrap().trigger, ";sig");
        assert!(back.keyboard_manager.windows_style.enabled && !back.keyboard_manager.windows_style.finder_cut_paste);
        assert_eq!(v["updates"]["includePrereleases"], false);
        assert_eq!(v["setup"]["remindMissingPermissions"], true);
        assert_eq!(v["workspaces"]["launch"]["timeoutSecs"], 15);
        // A partial new section keeps the other fields' defaults.
        let p: AppConfig = serde_json::from_str(r#"{"colorPicker": {"copiedFormat": "HSL"}, "textExtractor": {"enabled": true}}"#).unwrap();
        assert_eq!(p.color_picker.settings.copied_format, "HSL");
        assert_eq!(p.color_picker.settings.hotkey, color_picker::Hotkey::default_for_platform());
        assert!(p.text_extractor.enabled);
        let p: AppConfig = serde_json::from_str(
            r#"{"updates": {"lastCheck": 1800000000}, "setup": {"completed": true},
                "workspaces": {"hotkey": {"ctrl": true, "alt": true, "code": 87}}}"#,
        )
        .unwrap();
        assert!(p.updates.check && p.updates.last_check == Some(1_800_000_000));
        assert!(p.setup.completed && p.setup.remind_missing_permissions);
        assert_eq!(p.workspaces.hotkey, workspaces::Hotkey::new(false, true, true, false, 87));
        assert!(p.workspaces.enabled);
    }

    /// Display Control skips Display Scaling's virtual displays by these numbers.
    #[test]
    fn virtual_display_ids_agree() {
        assert_eq!(display_control::SCALING_VIRTUAL_VENDOR, display_scaling::VIRTUAL_VENDOR);
        assert_eq!(display_control::SCALING_VIRTUAL_PRODUCT, display_scaling::VIRTUAL_PRODUCT);
        assert!(display_control::is_scaling_virtual_display(display_scaling::VIRTUAL_VENDOR, display_scaling::VIRTUAL_PRODUCT));
    }

    #[test]
    fn display_status_files() {
        let dir = std::env::temp_dir().join(format!("pm-config-display-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        assert!(DisplayStatus::load_for(&cfg).is_none());
        let st = DisplayStatus { running: true, keys_active: true, ..Default::default() }.stamp();
        st.save_for(&cfg).unwrap();
        assert_eq!(DisplayStatus::load_for(&cfg).unwrap(), st);
        assert!(st.is_fresh());
        let r = DisplayRequest { brightness: Some(40), ..DisplayRequest::new("LG#1") };
        r.write_for(&cfg).unwrap();
        let back = DisplayRequest::load_for(&cfg).unwrap();
        assert_eq!(back, r);
        assert!(back.is_fresh(5_000));
        assert_eq!(data_dir_for(&cfg), dir);
        assert_eq!(file_tools_path_for(&cfg), dir.join("file-tools.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_and_request_files() {
        let dir = std::env::temp_dir().join(format!("pm-config-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        assert!(MwbStatus::load_for(&cfg).is_none());
        let st = MwbStatus {
            running: true,
            machines: vec![MwbMachine { name: "pc".into(), address: "192.168.1.20".into(), connected: true, discovered: true }],
            ..Default::default()
        }
        .stamp();
        st.save_for(&cfg).unwrap();
        let back = MwbStatus::load_for(&cfg).unwrap();
        assert_eq!(back, st);
        assert!(back.is_fresh());
        assert!(!MwbStatus { at: 1, ..Default::default() }.is_fresh());
        DiscoverRequest::write_for(&cfg).unwrap();
        let r: DiscoverRequest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MWB_DISCOVER_REQUEST_FILE)).unwrap()).unwrap();
        assert!(r.is_fresh(5_000));
        assert!(!DiscoverRequest { at: 1 }.is_fresh(5_000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
