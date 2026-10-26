//! Display scaling: fix "my 1440p external monitor looks blurry or tiny"
//! (BetterDisplay-style).
//!
//! * **Display modes** — every mode of every display, including HiDPI and
//!   the hidden duplicate low-resolution ones, and switching with a
//!   "revert in 15 s unless confirmed" safety net
//!   ([`Handle::apply_mode_with_timeout`] → [`Token`] → [`Handle::confirm`]).
//! * **HiDPI on non-Retina external displays** (macOS 11+): a virtual
//!   display made with the private `CGVirtualDisplay` API offers HiDPI
//!   modes (e.g. 1920x1080 HiDPI = a 3840x2160 backing store on a 1440p
//!   panel) and the physical display mirrors it, so the panel shows the
//!   downsampled HiDPI image. The virtual display lives as long as the
//!   module; what was changed is persisted so a crash is cleaned up on the
//!   next start. Alternatively [`plist::generate_override`] +
//!   [`plist::install_override_with_admin`] install a display override
//!   plist (admin password; takes effect after reconnect / restart).
//! * **Custom resolutions** — extra logical sizes added to the virtual
//!   display's HiDPI modes.
//! * **Windows** — modes via `EnumDisplaySettingsEx`, switching via
//!   `ChangeDisplaySettingsEx` (tested with `CDS_TEST` first) with the same
//!   revert flow; per-monitor DPI is reported read-only (Windows scales
//!   natively).
//!
//! [`DisplayScaling::start`] spawns a worker thread that owns the displays;
//! [`Handle`] calls are answered by it.

mod engine;
pub mod logic;
mod platform;
pub mod plist;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub use logic::{hidpi_logical_resolutions, select_mode, VirtualPlan, MAX_BACKING};
#[cfg(target_os = "macos")]
pub use platform::virtual_display_self_test;
pub use plist::{generate_override, install_override_with_admin, override_installed, override_path, remove_override_with_admin, OverridePlist};

/// File (next to `config.json`) recording what this module changed, so a
/// crash is undone on the next start.
pub const STATE_FILE: &str = "display-scaling-state.json";

/// Vendor / product ids (`CGDisplayVendorNumber` / `CGDisplayModelNumber`)
/// of this module's virtual HiDPI displays, so other modules can skip them.
pub const VIRTUAL_VENDOR: u32 = 0x50D0;
pub const VIRTUAL_PRODUCT: u32 = 0x4844;

/// Whether a display (by vendor / model number) is one of this module's
/// virtual HiDPI displays.
pub fn is_virtual_display(vendor: u32, model: u32) -> bool {
    vendor == VIRTUAL_VENDOR && model == VIRTUAL_PRODUCT
}

/// Default revert timeout for [`Handle::apply_mode_with_timeout`].
pub const DEFAULT_REVERT_TIMEOUT: Duration = Duration::from_secs(15);

/// A width x height pair (logical points or pixels, depending on context).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// A display mode. `width`/`height` are the logical size (points),
/// `pixel_width`/`pixel_height` the framebuffer; HiDPI modes have pixels at
/// 2x the points. On Windows logical = pixels and `hidpi` is false.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Hz (0 when the display doesn't say, e.g. some built-in panels).
    pub refresh: f64,
    pub hidpi: bool,
}

impl Mode {
    /// "1920 x 1080 (HiDPI) @ 60 Hz".
    pub fn label(&self) -> String {
        let mut s = format!("{} x {}", self.width, self.height);
        if self.hidpi {
            s.push_str(" (HiDPI)");
        }
        if self.refresh > 0.0 {
            s.push_str(&format!(" @ {} Hz", (self.refresh * 100.0).round() / 100.0));
        }
        s
    }

    pub fn resolution(&self) -> Resolution {
        Resolution { width: self.width, height: self.height }
    }

    /// The spec that selects exactly this mode again.
    pub fn spec(&self) -> ModeSpec {
        ModeSpec { width: self.width, height: self.height, hidpi: Some(self.hidpi), refresh: (self.refresh > 0.0).then_some(self.refresh), pixel_width: Some(self.pixel_width), pixel_height: Some(self.pixel_height) }
    }
}

/// A mode as stored in the config: a logical size plus optional details
/// (missing ones are chosen: HiDPI preferred, highest refresh).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModeSpec {
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidpi: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_height: Option<u32>,
}

/// Per-display settings, keyed in [`Config::displays`] by stable display id
/// ([`DisplayModes::id`]: `"<name>#<serial>"`, or `"<name>#v<vendor>m<model>"`
/// without a serial — the same ids the Displays module uses). A key of the
/// form `"v<vendor>m<model>"` (4-digit lowercase hex each) matches every
/// display of that model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DisplayConfig {
    /// macOS: HiDPI through a mirrored virtual display (non-built-in
    /// displays only). Ignored on Windows.
    pub hidpi_enabled: bool,
    /// Logical size to run at while HiDPI is on (e.g. 1920x1080 on a 1440p
    /// panel: bigger text, still sharp). Default: the panel's own size,
    /// rendered at 2x and downsampled (same size, crisper text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_logical_resolution: Option<Resolution>,
    /// Extra logical sizes offered as HiDPI modes on the virtual display.
    pub custom_resolutions: Vec<Resolution>,
    /// Mode applied whenever the display is connected. With HiDPI on it
    /// selects among the virtual display's modes (and wins over
    /// `target_logical_resolution`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_mode: Option<ModeSpec>,
}

/// Module settings. The default does nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// Seconds before an unconfirmed mode change is reverted (5–120).
    pub revert_timeout_secs: u64,
    pub displays: BTreeMap<String, DisplayConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: false, revert_timeout_secs: DEFAULT_REVERT_TIMEOUT.as_secs(), displays: BTreeMap::new() }
    }
}

impl Config {
    pub fn revert_timeout(&self) -> Duration {
        Duration::from_secs(self.revert_timeout_secs.clamp(5, 120))
    }
}

/// A display and its modes, as [`Handle::displays`] reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayModes {
    /// Stable id (config key).
    pub id: String,
    pub name: String,
    pub is_builtin: bool,
    /// EDID vendor / product (what [`plist::generate_override`] takes).
    pub vendor: u32,
    pub model: u32,
    /// Current mode. With HiDPI active, the virtual display's mode (what
    /// the user sees); `physical_current` is the panel's.
    pub current: Option<Mode>,
    /// Modes the user can pick (the virtual display's while HiDPI is active).
    pub modes: Vec<Mode>,
    pub physical_current: Option<Mode>,
    /// The panel's native pixel size.
    pub native: Option<Resolution>,
    /// The display can get HiDPI through a virtual display (macOS, not built-in, API present).
    pub hidpi_capable: bool,
    /// A virtual HiDPI display is mirrored onto this one right now.
    pub hidpi_active: bool,
    /// The display already offers HiDPI modes of its own (Retina/5K etc.).
    pub native_hidpi: bool,
    /// Logical sizes HiDPI would offer (standard ones + custom).
    pub suggested_hidpi: Vec<Resolution>,
    /// Windows: effective DPI (96 = 100 %), read-only.
    pub dpi: Option<u32>,
    /// Windows: scaling percent derived from `dpi`. macOS: backing scale of
    /// the current mode x 100 (200 for HiDPI).
    pub scale_percent: Option<u32>,
}

/// Identifies an unconfirmed mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Token(pub u64);

/// An unconfirmed mode change (for a countdown in the UI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChange {
    pub token: Token,
    pub display_id: String,
    pub previous: Mode,
    pub applied: Mode,
    pub seconds_left: u64,
}

/// `<config dir>/display-scaling-state.json`: `~/Library/Application
/// Support/PowerMac` on macOS, `%APPDATA%\PowerMac` on Windows.
pub fn default_state_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("PowerMac").join(STATE_FILE))
}

/// Whether HiDPI through a virtual display is possible on this system
/// (macOS 11+ with the private `CGVirtualDisplay` classes).
pub fn hidpi_supported() -> bool {
    platform::hidpi_supported()
}

/// Every display and its modes, without starting the module (no changes).
pub fn list_displays() -> Vec<DisplayModes> {
    let mut b = platform::new_backend();
    engine::describe_all(&mut *b, &Config::default(), &BTreeMap::new())
}

pub struct DisplayScaling;

impl DisplayScaling {
    /// Apply `config` (undoing what a crashed previous run left behind
    /// first), then keep it applied as displays come and go. State goes to
    /// [`default_state_path`].
    pub fn start(config: Config) -> Result<Handle> {
        Self::start_with_state_file(config, default_state_path())
    }

    /// [`start`](Self::start) with an explicit state file (`None`: no crash
    /// recovery).
    pub fn start_with_state_file(config: Config, state_path: Option<PathBuf>) -> Result<Handle> {
        Self::start_with(config, state_path, platform::new_backend)
    }

    fn start_with<F>(config: Config, state_path: Option<PathBuf>, make: F) -> Result<Handle>
    where
        F: FnOnce() -> Box<dyn engine::Backend> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("display-scaling".into())
            .spawn(move || {
                let mut w = engine::Worker::new(make(), config, state_path);
                w.startup();
                let _ = ready_tx.send(());
                w.run(rx);
            })
            .context("spawning display-scaling thread")?;
        ready_rx.recv_timeout(Duration::from_secs(30)).map_err(|_| anyhow!("display-scaling worker did not start"))?;
        Ok(Handle { tx, thread: Some(thread) })
    }
}

/// The running module. Dropping it stops it (like [`Handle::stop`]).
pub struct Handle {
    tx: mpsc::Sender<engine::Cmd>,
    thread: Option<JoinHandle<()>>,
}

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

impl Handle {
    fn ask<T>(&self, make: impl FnOnce(mpsc::Sender<T>) -> engine::Cmd) -> Result<T> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(make(tx)).map_err(|_| anyhow!("display-scaling is not running"))?;
        rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| anyhow!("display-scaling did not answer"))
    }

    /// Apply a new config (only displays whose entry changed are touched).
    pub fn update(&self, config: Config) {
        let _ = self.tx.send(engine::Cmd::Update(config));
    }

    /// Every display with its modes (fresh enumeration).
    pub fn displays(&self) -> Vec<DisplayModes> {
        self.ask(engine::Cmd::Displays).unwrap_or_default()
    }

    /// Switch display `id` to `mode` (one of its [`DisplayModes::modes`]);
    /// it is reverted after `timeout` unless [`confirm`](Self::confirm)ed.
    /// With `permanent`, confirming also stores the mode in the system's
    /// display preferences (macOS `kCGConfigurePermanently`, Windows
    /// `CDS_UPDATEREGISTRY`); otherwise it lasts for the login session.
    pub fn apply_mode_with_timeout(&self, id: &str, mode: &Mode, timeout: Duration, permanent: bool) -> Result<Token> {
        let id = id.to_string();
        let mode = *mode;
        self.ask(|r| engine::Cmd::Apply { id, mode, timeout, permanent, reply: r })?
    }

    /// Keep a change made with [`apply_mode_with_timeout`](Self::apply_mode_with_timeout).
    /// Fails when the token is unknown (already reverted / confirmed).
    pub fn confirm(&self, token: Token) -> Result<()> {
        self.ask(|r| engine::Cmd::Confirm(token, r))?
    }

    /// Revert an unconfirmed change now.
    pub fn revert(&self, token: Token) -> Result<()> {
        self.ask(|r| engine::Cmd::Revert(token, r))?
    }

    /// Unconfirmed changes and their remaining time.
    pub fn pending(&self) -> Vec<PendingChange> {
        self.ask(engine::Cmd::Pending).unwrap_or_default()
    }

    /// Stop: revert unconfirmed changes, tear down HiDPI virtual displays
    /// (un-mirroring first) and restore modes changed by `preferredMode`.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(t) = self.thread.take() {
            let (tx, rx) = mpsc::channel();
            if self.tx.send(engine::Cmd::Stop(tx)).is_ok() {
                let _ = rx.recv_timeout(REPLY_TIMEOUT);
            }
            let _ = t.join();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read the crash-recovery state file (for diagnostics).
pub fn read_state(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_camel_case() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert!(!c.enabled && c.displays.is_empty());
        assert_eq!(c.revert_timeout(), Duration::from_secs(15));
        let c: Config = serde_json::from_str(
            r#"{"enabled":true,"revertTimeoutSecs":1,"displays":{"LG#1":{"hidpiEnabled":true,"targetLogicalResolution":{"width":1920,"height":1080},
               "customResolutions":[{"width":2200,"height":1238}],"preferredMode":{"width":1920,"height":1080,"hidpi":true}},"X#2":{}}}"#,
        )
        .unwrap();
        assert_eq!(c.revert_timeout(), Duration::from_secs(5));
        let d = &c.displays["LG#1"];
        assert!(d.hidpi_enabled && d.custom_resolutions.len() == 1 && d.preferred_mode.as_ref().unwrap().hidpi == Some(true));
        assert_eq!(c.displays["X#2"], DisplayConfig::default());
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("\"hidpiEnabled\":true") && j.contains("\"revertTimeoutSecs\":1") && !j.contains("pixelWidth"));
        assert_eq!(serde_json::from_str::<Config>(&j).unwrap(), c);
    }

    #[test]
    fn mode_label_and_spec() {
        let m = Mode { width: 1920, height: 1080, pixel_width: 3840, pixel_height: 2160, refresh: 59.951, hidpi: true };
        assert_eq!(m.label(), "1920 x 1080 (HiDPI) @ 59.95 Hz");
        assert_eq!(select_mode(&[m], &m.spec()), Some(m));
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("pixelWidth"));
    }
}
