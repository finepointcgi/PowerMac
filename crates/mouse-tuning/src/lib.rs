//! Mouse tuning: "my mouse feels awful on a Mac" fixes in the spirit of
//! LinearMouse and SensibleSideButtons.
//!
//! * **Pointer:** turn pointer acceleration off (linear movement) and set the
//!   pointer speed, separately for mice and trackpads and optionally per
//!   device (vendor/product id). Original values are read first and restored
//!   by [`Handle::stop`] (and on drop).
//! * **Buttons:** side buttons (back/forward) navigate on macOS, and the
//!   middle/side buttons can be remapped to actions (keystrokes, Mission
//!   Control, App Exposé, Launchpad, show desktop, copy/paste, ...).
//!
//! Smooth scrolling for mouse wheels lives in the `scroll-reverser` crate
//! (same subscriber of the shared event tap as the reverser).
//!
//! ```no_run
//! use mouse_tuning::{Config, MouseTuning};
//!
//! let mut cfg = Config::default();
//! cfg.mouse.disable_acceleration = true;
//! let mut handle = MouseTuning::start(cfg.clone())?;
//! cfg.mouse.speed = Some(0.6);
//! handle.update(cfg);
//! handle.stop(); // restores the system's pointer settings
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # macOS
//!
//! Pointer settings go through the HID event system client (see
//! `mac_hid.rs`), per HID service, like LinearMouse. A background thread
//! re-applies them every [`REAPPLY_INTERVAL`] so newly connected devices are
//! covered and values the system pushes back (wake, System Settings) are
//! overridden again; a value that changed behind our back becomes the new
//! "original" to restore. Changes live in memory only: if the process dies
//! without stopping, they last until the device reconnects or the user
//! changes the setting in System Settings.
//!
//! * Acceleration off sets the device's acceleration (`HIDMouseAcceleration`
//!   / `HIDTrackpadAcceleration`, whichever the device's
//!   `HIDPointerAccelerationType` names) to -1.0, the same as
//!   `defaults write -g com.apple.mouse.scaling -1`. Speed is then a linear
//!   multiplier applied through `HIDPointerResolution`
//!   ([`linear_speed_factor`]: 0.25x..4x, 0.5 = the device's own resolution).
//! * With acceleration on, speed sets the acceleration value itself, i.e. the
//!   System Settings "Tracking speed" slider (0..3).
//! * Side buttons: a subscriber of the shared event tap (`pm-hook`,
//!   [`pm_hook::priority::MOUSE_TUNING`], after Mouse Without Borders so a
//!   remote machine gets the real button) on `otherMouseDown/Up` (needs
//!   Accessibility). Back/Forward are sent as Cmd+[ and
//!   Cmd+] (Safari, Finder, Chrome, Firefox, Xcode, Music all honour them),
//!   not as synthesized swipe gestures: the gesture route needs undocumented
//!   CGS gesture fields that change between releases and cannot be verified
//!   without driving other apps. Cmd+[ depends on the keyboard layout having
//!   `[` on the US key position (layouts that move it, such as German, may
//!   need a [`ButtonAction::Keystroke`] mapping instead). Mission Control,
//!   App Exposé, Launchpad and Show Desktop go through the Dock
//!   (`CoreDockSendNotification`, private but long-lived), falling back to
//!   `open -b com.apple.exposelauncher` / `open -a Launchpad`.
//!
//! # Windows
//!
//! * Acceleration ("Enhance pointer precision") via `SPI_GETMOUSE` /
//!   `SPI_SETMOUSE` (thresholds and acceleration 0,0,0 = off) and speed via
//!   `SPI_SETMOUSESPEED` (1..20, [`windows_speed`]); set for the session only
//!   (not persisted to the user profile) and restored on stop. Windows has
//!   one global pointer setting, so only [`Config::mouse`] applies; the
//!   trackpad settings and per-device overrides are ignored there (precision
//!   touchpads have their own settings in Windows).
//! * Side buttons already navigate on Windows, so
//!   [`ButtonsConfig::side_buttons_navigation`] does nothing there; explicit
//!   [`ButtonsConfig::mappings`] work through the shared `WH_MOUSE_LL` hook (`pm-hook`; middle,
//!   X1 = back, X2 = forward).

use serde::{Deserialize, Serialize};

pub mod keys;

#[cfg(target_os = "macos")]
mod mac_hid;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

mod actions;
#[cfg(any(target_os = "macos", test))]
mod applier;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// How often the pointer settings are re-applied (new devices, values pushed
/// back by the system).
pub const REAPPLY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Tag ("MTUN") on events this crate injects itself (`kCGEventSourceUserData`
/// / `dwExtraInfo`). Keystrokes go through `pm-input` and carry its tag.
pub const SELF_INJECTED_TAG: usize = pm_hook::tags::MOUSE_TUNING;

/// `pm-input`'s injection tag ("PMMW"): button events carrying it (Mouse
/// Without Borders) are never remapped.
pub const PM_INPUT_INJECTED_TAG: usize = pm_input::INJECTED_TAG;

/// Mouse button numbers as used in [`ButtonMapping::button`] (macOS
/// `kCGMouseEventButtonNumber`; Windows middle / XBUTTON1 / XBUTTON2 map to
/// 2 / 3 / 4).
pub mod button {
    pub const MIDDLE: u8 = 2;
    /// Side button "back" (button 4 in 1-based terms, XBUTTON1 on Windows).
    pub const BACK: u8 = 3;
    /// Side button "forward" (button 5 in 1-based terms, XBUTTON2 on Windows).
    pub const FORWARD: u8 = 4;
    /// Highest button number handled (macOS reports up to 31).
    pub const MAX: u8 = 31;
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// User settings. Live-updatable through [`Handle::update`]. Every field is
/// optional in JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch. When false nothing is changed (pointer settings are
    /// restored and no button is remapped), but the handle keeps running.
    pub enabled: bool,
    /// Mice (and on Windows: every pointing device).
    pub mouse: PointerSettings,
    /// Trackpads (macOS only).
    pub trackpad: PointerSettings,
    /// Per-device overrides (macOS only), matched by vendor/product id.
    pub devices: Vec<DeviceOverride>,
    pub buttons: ButtonsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            mouse: PointerSettings::default(),
            trackpad: PointerSettings::default(),
            devices: Vec::new(),
            buttons: ButtonsConfig::default(),
        }
    }
}

/// Pointer settings for one class of device. The default changes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PointerSettings {
    /// Linear movement (no acceleration curve). Default false.
    pub disable_acceleration: bool,
    /// Pointer speed, 0.0..=1.0; `None` leaves the system's speed alone.
    /// macOS, acceleration off: linear multiplier, see [`linear_speed_factor`]
    /// (0.5 = the device's native resolution). macOS, acceleration on: the
    /// System Settings tracking speed (0.0..=1.0 maps to its 0..3 range).
    /// Windows: `SPI_SETMOUSESPEED` 1..20 ([`windows_speed`]; 0.5 ≈ 10, the
    /// Windows default).
    pub speed: Option<f64>,
}

impl PointerSettings {
    /// True when these settings change nothing.
    pub fn is_noop(&self) -> bool {
        !self.disable_acceleration && self.speed.is_none()
    }
}

/// Per-device override (macOS). Fields left `None` fall back to the device
/// class settings ([`Config::mouse`] / [`Config::trackpad`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceOverride {
    pub vendor_id: u32,
    pub product_id: u32,
    /// Product name as reported by [`list_devices`]. When non-empty it must
    /// match too (built-in devices report vendor/product 0).
    pub name: String,
    pub disable_acceleration: Option<bool>,
    pub speed: Option<f64>,
}

impl DeviceOverride {
    pub fn matches(&self, d: &DeviceInfo) -> bool {
        self.vendor_id == d.vendor_id
            && self.product_id == d.product_id
            && (self.name.is_empty() || self.name == d.name)
    }
}

/// Mouse-button settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ButtonsConfig {
    /// macOS: side buttons do Back / Forward (unless mapped explicitly).
    /// Default true on macOS, false elsewhere (Windows does it natively).
    pub side_buttons_navigation: bool,
    /// Explicit mappings; they win over `side_buttons_navigation`. Buttons
    /// 0 (left) and 1 (right) are never remapped.
    pub mappings: Vec<ButtonMapping>,
}

#[allow(clippy::derivable_impls)] // not derivable on macOS (cfg-dependent)
impl Default for ButtonsConfig {
    fn default() -> Self {
        Self { side_buttons_navigation: cfg!(target_os = "macos"), mappings: Vec::new() }
    }
}

/// One button → action mapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtonMapping {
    /// See [`button`]: 2 = middle, 3 = back, 4 = forward, 5.. = extra (macOS).
    pub button: u8,
    pub action: ButtonAction,
}

/// What a remapped button does. JSON: `{"type": "missionControl"}`,
/// `{"type": "keystroke", "keys": "Cmd+Shift+T"}`, or any shared automation
/// action ([`keyboard_manager::automation::Action`]):
/// `{"type": "shell", "command": "open -a Notes"}`, `{"type": "text", "text":
/// "..."}`, `{"type": "run", "path": "..."}`, `{"type": "openUri", "uri":
/// "..."}`, `{"type": "pastePlainText"}`, `{"type": "shortcut", "keys": "..."}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ButtonAction {
    /// The button's normal behaviour (pass through).
    #[default]
    Default,
    /// Swallow the button.
    Disabled,
    /// macOS Cmd+[ ; Windows Browser Back key.
    Back,
    /// macOS Cmd+] ; Windows Browser Forward key.
    Forward,
    /// A key combination, see [`keys::parse_combo`] (e.g. `"Cmd+W"`).
    Keystroke { keys: String },
    /// macOS Mission Control; Windows Task View (Win+Tab).
    MissionControl,
    /// macOS App Exposé (the front app's windows); Windows Task View.
    AppExpose,
    /// macOS Launchpad (the Apps view on recent macOS); Windows Start menu.
    Launchpad,
    /// macOS Show Desktop; Windows Win+D.
    ShowDesktop,
    /// Cmd+C / Ctrl+C.
    Copy,
    /// Cmd+V / Ctrl+V.
    Paste,
    /// Cmd+X / Ctrl+X.
    Cut,
    /// Cmd+Z / Ctrl+Z.
    Undo,
    /// A shared automation action (shell command, text, program, URL, ...).
    /// Its JSON is the action's own (`{"type": "shell", ...}`).
    #[serde(untagged)]
    Action(Action),
}

pub use keyboard_manager::automation::Action;

impl ButtonAction {
    /// All variants, for a settings drop-down (`Keystroke` with empty keys).
    pub fn all() -> Vec<ButtonAction> {
        vec![
            ButtonAction::Default,
            ButtonAction::Disabled,
            ButtonAction::Back,
            ButtonAction::Forward,
            ButtonAction::Keystroke { keys: String::new() },
            ButtonAction::MissionControl,
            ButtonAction::AppExpose,
            ButtonAction::Launchpad,
            ButtonAction::ShowDesktop,
            ButtonAction::Copy,
            ButtonAction::Paste,
            ButtonAction::Cut,
            ButtonAction::Undo,
        ]
    }

    /// [`all`](Self::all) plus the shared automation actions (one empty
    /// instance per kind, `Disabled` left out as it duplicates
    /// [`ButtonAction::Disabled`]), for a settings page that can edit them.
    pub fn all_with_automation() -> Vec<ButtonAction> {
        let mut v = Self::all();
        v.extend(
            Action::all()
                .into_iter()
                .filter(|a| *a != Action::Disabled)
                .map(ButtonAction::Action),
        );
        v
    }

    /// Normalize: a shared action that has a native equivalent becomes it
    /// (`{"type":"disabled"}` → [`ButtonAction::Disabled`]).
    pub fn normalized(self) -> ButtonAction {
        match self {
            ButtonAction::Action(Action::Disabled) => ButtonAction::Disabled,
            other => other,
        }
    }

    /// Configuration problems (empty = fine).
    pub fn validate(&self) -> Vec<String> {
        match self {
            ButtonAction::Keystroke { keys } => keys::parse_combo(keys).err().into_iter().collect(),
            ButtonAction::Action(a) => a.validate(),
            _ => Vec::new(),
        }
    }

    /// Short display name.
    pub fn label(&self) -> &'static str {
        match self {
            ButtonAction::Default => "Default",
            ButtonAction::Disabled => "Do nothing",
            ButtonAction::Back => "Back",
            ButtonAction::Forward => "Forward",
            ButtonAction::Keystroke { .. } => "Keyboard shortcut",
            ButtonAction::MissionControl => "Mission Control",
            ButtonAction::AppExpose => "App Exposé",
            ButtonAction::Launchpad => "Launchpad",
            ButtonAction::ShowDesktop => "Show desktop",
            ButtonAction::Copy => "Copy",
            ButtonAction::Paste => "Paste",
            ButtonAction::Cut => "Cut",
            ButtonAction::Undo => "Undo",
            ButtonAction::Action(a) => a.label(),
        }
    }
}

impl Config {
    /// The pointer settings that apply to `device` (class settings, then the
    /// first matching override).
    pub fn effective(&self, device: &DeviceInfo) -> PointerSettings {
        let mut s = match device.kind {
            DeviceKind::Mouse => self.mouse,
            DeviceKind::Trackpad => self.trackpad,
        };
        if let Some(o) = self.devices.iter().find(|o| o.matches(device)) {
            if let Some(d) = o.disable_acceleration {
                s.disable_acceleration = d;
            }
            if o.speed.is_some() {
                s.speed = o.speed;
            }
        }
        s
    }

    /// The action for `button` on this platform (`Default` = pass through).
    pub fn button_action(&self, button: u8) -> ButtonAction {
        if !self.enabled || !(button::MIDDLE..=button::MAX).contains(&button) {
            return ButtonAction::Default;
        }
        if let Some(m) = self.buttons.mappings.iter().find(|m| m.button == button) {
            return m.action.clone().normalized();
        }
        if cfg!(target_os = "macos") && self.buttons.side_buttons_navigation {
            match button {
                button::BACK => return ButtonAction::Back,
                button::FORWARD => return ButtonAction::Forward,
                _ => {}
            }
        }
        ButtonAction::Default
    }

    /// Per-button actions, indexed by button number (0..=[`button::MAX`]).
    pub fn button_table(&self) -> Vec<ButtonAction> {
        (0..=button::MAX).map(|b| self.button_action(b)).collect()
    }

    /// True when some button needs the hook.
    pub fn needs_button_hook(&self) -> bool {
        (button::MIDDLE..=button::MAX).any(|b| self.button_action(b) != ButtonAction::Default)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// 16.16 fixed point, as the HID system stores acceleration and resolution.
pub const FIXED_ONE: i64 = 65_536;

/// macOS: linear speed multiplier for `speed` (0.0..=1.0) with acceleration
/// off: 0.25x at 0, 1x at 0.5, 4x at 1, exponential in between.
pub fn linear_speed_factor(speed: f64) -> f64 {
    let s = if speed.is_finite() { speed.clamp(0.0, 1.0) } else { 0.5 };
    2f64.powf((s - 0.5) * 4.0)
}

/// Windows: `SPI_SETMOUSESPEED` value (1..=20) for `speed` (0.0..=1.0).
pub fn windows_speed(speed: f64) -> u32 {
    let s = if speed.is_finite() { speed.clamp(0.0, 1.0) } else { 0.5 };
    (1.0 + s * 19.0).round() as u32
}

/// macOS property values to set on one service; `None` = leave / restore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MacTargets {
    /// Acceleration (16.16; -65536 = off).
    pub acceleration: Option<i64>,
    /// `HIDPointerResolution` (16.16 dpi).
    pub resolution: Option<i64>,
}

/// What to set for `s` on a device whose original resolution is `orig_resolution`.
pub fn mac_targets(s: &PointerSettings, orig_resolution: Option<i64>) -> MacTargets {
    let speed = s.speed.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
    if s.disable_acceleration {
        MacTargets {
            acceleration: Some(-FIXED_ONE),
            resolution: match (speed, orig_resolution) {
                (Some(v), Some(r)) if r > 0 => {
                    Some(((r as f64 / linear_speed_factor(v)).round() as i64).max(FIXED_ONE))
                }
                _ => None,
            },
        }
    } else {
        MacTargets {
            acceleration: speed.map(|v| (v * 3.0 * FIXED_ONE as f64).round() as i64),
            resolution: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKind {
    Mouse,
    Trackpad,
}

/// A pointing device, for the settings UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// OS id of the HID service (macOS registry id); not stable across reconnects.
    pub id: u64,
    pub name: String,
    pub vendor_id: u32,
    pub product_id: u32,
    pub kind: DeviceKind,
    /// e.g. "USB", "Bluetooth", "FIFO" (built-in); may be empty.
    pub transport: String,
}

impl DeviceInfo {
    /// Same model (one physical device may expose several services).
    pub fn same_model(&self, o: &DeviceInfo) -> bool {
        self.kind == o.kind
            && self.vendor_id == o.vendor_id
            && self.product_id == o.product_id
            && self.name == o.name
    }

    /// A [`DeviceOverride`] that matches this device, with nothing overridden yet.
    pub fn new_override(&self) -> DeviceOverride {
        DeviceOverride {
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            name: self.name.clone(),
            ..Default::default()
        }
    }
}

/// Connected pointing devices (macOS: one per model, from the HID event
/// system; Windows: empty, as per-device settings are not supported there).
pub fn list_devices() -> Vec<DeviceInfo> {
    #[cfg(target_os = "macos")]
    {
        mac_hid::list_devices()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Diagnostics: pointer properties of every pointing HID service (macOS):
/// device, acceleration key, acceleration (16.16; the system-wide value when
/// the device reports none), resolution (16.16, the driver's value). These are
/// what the OS reports, which does not include values set by this crate.
#[doc(hidden)]
pub fn pointer_properties() -> Vec<(DeviceInfo, String, Option<i64>, Option<i64>)> {
    #[cfg(target_os = "macos")]
    {
        let Some(client) = mac_hid::Client::new() else { return Vec::new() };
        client
            .pointers()
            .into_iter()
            .map(|(s, i)| {
                let key = s.accel_key(i.kind);
                let acc = s.get_i64(&key).or_else(|| client.get_i64(&key));
                (i, key, acc, s.get_i64(mac_hid::KEY_RESOLUTION))
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Runtime API
// ---------------------------------------------------------------------------

/// True when button remapping lacks its OS permission (macOS: Accessibility).
/// Pointer settings need no permission. Never prompts; false on Windows.
pub fn needs_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        !macos::is_trusted(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// macOS: show the Accessibility prompt if needed; returns whether trusted.
pub fn request_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_trusted(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub struct MouseTuning;

impl MouseTuning {
    /// Apply `config` and keep it applied. Pointer settings work without any
    /// permission. If the button hook cannot be installed (macOS without
    /// Accessibility) start still succeeds; see [`Handle::buttons_active`].
    /// The hook is (re)tried on every [`Handle::update`].
    pub fn start(config: Config) -> anyhow::Result<Handle> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let inner = platform::Engine::start(config.clone())?;
            Ok(Handle { config, inner: Some(inner) })
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = config;
            Err(anyhow::anyhow!("mouse tuning is not supported on this platform"))
        }
    }
}

/// Running mouse tuning. Dropping it stops it (and restores the settings).
pub struct Handle {
    config: Config,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    inner: Option<platform::Engine>,
}

impl Handle {
    /// Apply new settings (pointer settings within a moment, buttons at once).
    pub fn update(&mut self, config: Config) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(e) = self.inner.as_mut() {
            e.update(config.clone());
        }
        self.config = config;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn is_running(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.inner.is_some()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    /// Whether the button hook is installed (false when no button is
    /// remapped, or on macOS without Accessibility).
    pub fn buttons_active(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.inner.as_ref().is_some_and(|e| e.buttons_active())
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    /// Restore the system pointer settings, remove the hook and join the
    /// threads. Idempotent.
    pub fn stop(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(e) = self.inner.take() {
            e.stop();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
