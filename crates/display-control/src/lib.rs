//! Display control: brightness, contrast and volume of external monitors
//! over DDC/CI, native brightness of built-in / Apple displays, and
//! software (gamma) dimming for everything else — PowerToys "PowerDisplay"
//! / MonitorControl for PowerMac.
//!
//! * **macOS (Apple Silicon)**: DDC/CI through the private `IOAVService`
//!   I2C API, each `DCPAVServiceProxy` matched to its CoreGraphics display
//!   via `IODisplayLocation` / EDID UUID (like MonitorControl). Built-in and
//!   Apple-made displays (Studio Display, Pro Display XDR, LG UltraFine)
//!   use the private DisplayServices framework. Intel Macs: DDC is not
//!   implemented (DisplayServices and gamma dimming still work).
//! * **Windows**: Dxva2 (`GetPhysicalMonitorsFromHMONITOR`,
//!   `Get/SetVCPFeature`) for external monitors, the `\\.\LCD` brightness
//!   IOCTLs for laptop panels, `SetDeviceGammaRamp` for dimming.
//!
//! [`DisplayControl::start`] spawns a worker thread that owns the hardware;
//! [`Handle`] setters return immediately (values are cached and writes are
//! coalesced and rate-limited). With [`Config::take_over_brightness_keys`] /
//! [`Config::take_over_volume_keys`] the media keys act on the external
//! display under the pointer (macOS: needs Accessibility), with a small OSD.

pub mod config;
pub mod ddc;
mod engine;
pub mod identity;
pub mod keys;
mod platform;
pub mod values;

use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use config::{Config, DisplayOverride, Hotkey, Hotkeys, Mods};
#[cfg(target_os = "macos")]
pub use platform::{debug_noop_writes, debug_report, debug_show_osd, run_main_loop_for};
pub use ddc::Feature;

/// A rectangle in global display coordinates (points, top-left origin on
/// macOS; virtual-screen pixels on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64 && y >= self.y as f64 && x < (self.x + self.width) as f64 && y < (self.y + self.height) as f64
    }
}

/// How a display's brightness is controlled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrightnessMethod {
    /// Not controllable.
    None,
    /// DDC/CI VCP 0x10.
    Ddc,
    /// DisplayServices (macOS built-in / Apple displays) or the Windows
    /// panel brightness IOCTL.
    Native,
    /// Gamma-table dimming.
    Software,
}

/// What the OSD shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OsdKind {
    Brightness,
    Contrast,
    Volume,
    Muted,
}

/// A display and its current (cached) values. Percentages are 0–100;
/// `max_*` are the raw DDC maxima (after per-display overrides).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    /// Stable id (`"<name>#<serial>"`), the key for [`Config::displays`].
    pub id: String,
    pub name: String,
    pub is_builtin: bool,
    /// From the per-display override.
    pub enabled: bool,
    pub bounds: Rect,
    /// The monitor answered DDC/CI reads.
    pub supports_ddc: bool,
    pub brightness_method: BrightnessMethod,
    pub brightness: Option<u8>,
    pub contrast: Option<u8>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    /// Raw VCP 0x60 value (MCCS: 0x0F DisplayPort-1, 0x10 DisplayPort-2,
    /// 0x11 HDMI-1, 0x12 HDMI-2, 0x1B USB-C, …). Read-only.
    pub input_source: Option<u16>,
    pub max_brightness: Option<u16>,
    pub max_contrast: Option<u16>,
    pub max_volume: Option<u16>,
}

impl DisplayInfo {
    /// Human name of an MCCS input source value.
    pub fn input_source_name(&self) -> Option<&'static str> {
        self.input_source.map(input_source_name)
    }
}

/// Human name of an MCCS VCP 0x60 value (low byte).
pub fn input_source_name(v: u16) -> &'static str {
    match v & 0xFF {
        0x01 => "VGA-1",
        0x02 => "VGA-2",
        0x03 => "DVI-1",
        0x04 => "DVI-2",
        0x0F => "DisplayPort-1",
        0x10 => "DisplayPort-2",
        0x11 => "HDMI-1",
        0x12 => "HDMI-2",
        0x1B => "USB-C",
        _ => "Other",
    }
}

/// Entry point.
pub struct DisplayControl;

impl DisplayControl {
    /// Start the module: enumerate displays (waits up to ~2 s for that),
    /// read their values in the background, and install the key hook if the
    /// config asks for it.
    pub fn start(config: Config) -> Result<Handle> {
        let (inner, worker) = engine::start(config, Box::new(platform::new_backend), Duration::from_secs(2))?;
        let h = Handle { inner, worker: Mutex::new(Some(worker)), hook: Mutex::new(None) };
        h.sync_hook();
        Ok(h)
    }
}

/// A running display-control module. Dropping it stops it.
pub struct Handle {
    inner: Arc<engine::Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
    hook: Mutex<Option<platform::KeyHook>>,
}

fn wants_hook(c: &Config) -> bool {
    let h = &c.hotkeys;
    c.enabled
        && (c.take_over_brightness_keys
            || c.take_over_volume_keys
            || [h.brightness_up, h.brightness_down, h.contrast_up, h.contrast_down, h.volume_up, h.volume_down, h.mute].iter().any(|k| k.is_set()))
}

impl Handle {
    /// Apply a new config (takes effect immediately).
    pub fn update(&self, config: Config) {
        self.inner.update_config(config);
        self.sync_hook();
    }

    fn sync_hook(&self) {
        let want = wants_hook(&self.inner.config());
        let mut hook = self.hook.lock().unwrap_or_else(|e| e.into_inner());
        if want && hook.is_none() {
            match platform::KeyHook::start(self.inner.clone()) {
                Ok(h) => *hook = Some(h),
                Err(e) => log::warn!("display-control: key takeover unavailable: {e:#}"),
            }
        } else if !want {
            if let Some(mut h) = hook.take() {
                h.stop();
            }
        }
    }

    /// Whether the key hook is installed (macOS: it needs Accessibility).
    pub fn keys_active(&self) -> bool {
        self.hook.lock().map(|h| h.is_some()).unwrap_or(false)
    }

    /// Stop the worker and key hook, restore gamma tables.
    pub fn stop(self) {
        drop(self)
    }

    fn shutdown(&self) {
        if let Some(mut h) = self.hook.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.stop();
        }
        platform::close_osd();
        self.inner.stop_worker();
        if let Some(t) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }

    /// Displays and their cached values.
    pub fn displays(&self) -> Vec<DisplayInfo> {
        self.inner.displays()
    }

    pub fn display(&self, id: &str) -> Option<DisplayInfo> {
        self.displays().into_iter().find(|d| d.id == id)
    }

    pub fn set_brightness(&self, id: &str, pct: u8) -> Result<()> {
        self.inner.set_pct(id, Feature::Brightness, pct)
    }

    pub fn set_contrast(&self, id: &str, pct: u8) -> Result<()> {
        self.inner.set_pct(id, Feature::Contrast, pct)
    }

    pub fn set_volume(&self, id: &str, pct: u8) -> Result<()> {
        self.inner.set_pct(id, Feature::Volume, pct)
    }

    /// Mute via VCP 0x8D, or by setting the volume to 0 (and restoring it)
    /// on monitors without 0x8D.
    pub fn set_mute(&self, id: &str, muted: bool) -> Result<()> {
        self.inner.set_mute(id, muted)
    }

    /// Re-enumerate displays and re-read every value (asynchronous; slow
    /// monitors take a second or two).
    pub fn refresh(&self) {
        self.inner.request_refresh(None, None);
    }

    /// Like [`Handle::refresh`] but waits (up to `timeout`) until done.
    /// Returns false on timeout.
    pub fn refresh_blocking(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        self.inner.request_refresh(None, Some(tx));
        rx.recv_timeout(timeout).is_ok()
    }

    /// Wait until queued writes have been sent (up to `timeout`).
    pub fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        self.inner.request_refresh(Some("\u{0}flush".into()), Some(tx));
        rx.recv_timeout(timeout).is_ok()
    }

    /// Whether the first enumeration finished.
    pub fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_is_shareable() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Handle>();
        send_sync::<DisplayInfo>();
    }

    #[test]
    fn rect_contains() {
        let r = Rect { x: -100, y: 0, width: 100, height: 50 };
        assert!(r.contains(-100.0, 0.0));
        assert!(r.contains(-0.5, 49.9));
        assert!(!r.contains(0.0, 10.0));
        assert!(!r.contains(-50.0, 50.0));
    }

    #[test]
    fn hook_wanted() {
        let mut c = Config { take_over_brightness_keys: false, ..Default::default() };
        assert!(!wants_hook(&c));
        c.hotkeys.mute = Hotkey::new(false, true, true, false, 0x4D);
        assert!(wants_hook(&c));
        c.enabled = false;
        assert!(!wants_hook(&c));
        c = Config { take_over_brightness_keys: false, take_over_volume_keys: true, ..Default::default() };
        assert!(wants_hook(&c));
    }

    #[test]
    fn info_serializes_camel_case() {
        let d = DisplayInfo {
            id: "a#1".into(),
            name: "a".into(),
            is_builtin: false,
            enabled: true,
            bounds: Rect::default(),
            supports_ddc: true,
            brightness_method: BrightnessMethod::Ddc,
            brightness: Some(5),
            contrast: None,
            volume: None,
            muted: None,
            input_source: Some(0x0F),
            max_brightness: Some(100),
            max_contrast: None,
            max_volume: None,
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"isBuiltin\":false") && s.contains("\"brightnessMethod\":\"ddc\"") && s.contains("\"maxBrightness\":100"), "{s}");
        assert_eq!(d.input_source_name(), Some("DisplayPort-1"));
        assert_eq!(input_source_name(0x0111), "HDMI-1");
    }
}
