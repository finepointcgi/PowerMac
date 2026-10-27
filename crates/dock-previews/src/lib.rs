//! Dock window previews (macOS) — "the Dock doesn't show windows".
//!
//! Hover an app's Dock icon for a moment and a panel with thumbnails of
//! that app's windows appears above it (beside it for a Dock on the left or
//! right), in the same dark HUD style as the window switcher.
//!
//! | In the panel | Action |
//! |---|---|
//! | click a tile | focus that window (un-minimizing / switching Space as needed) |
//! | hover a tile | close (red) and minimize (yellow) buttons appear |
//! | W / M while hovering a tile | close / (un)minimize that window |
//! | middle-click a tile | close that window |
//! | scroll (over the panel or the icon) | cycle through the windows, raising each |
//! | Esc, a click elsewhere, leaving icon and panel | close the panel |
//!
//! Optionally ([`Config::click_to_cycle`]) clicking the Dock icon of the
//! app that is already frontmost minimizes its window (one window) or
//! cycles through its windows (several), like the Windows taskbar.
//!
//! ```no_run
//! let handle = dock_previews::DockPreviews::start(dock_previews::Config::default())?;
//! // ... the main thread must run NSApplication (or otherwise service the
//! // main dispatch queue) for the panel to appear.
//! handle.stop();
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Architecture
//!
//! Pure, tested logic: [`geom`] (Dock geometry, orientation and icon hit
//! testing), [`placement`] (where the panel goes), [`hover`] (hover delay /
//! leave grace state machine), [`layout`] (tiles and their buttons) and
//! [`cycle`] (what a click on the active app's icon does).
//!
//! macOS runtime: a subscriber of the shared event tap (`pm-hook`,
//! [`pm_hook::priority::DOCK_PREVIEWS`]; mouse + Esc/W/M; it only swallows
//! events aimed at the panel, and Dock clicks when click-to-cycle applies),
//! a worker thread that reads the Dock's Accessibility tree, tracks the
//! pointer at ~30 Hz while it is near the Dock, lists the hovered app's
//! windows and captures thumbnails (ScreenCaptureKit), and an `NSPanel` on
//! the main thread. On Windows (which has taskbar thumbnails) and other
//! platforms the module is a no-op.

pub mod cycle;
pub mod geom;
pub mod hover;
pub mod layout;
pub mod placement;
mod platform;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// macOS-only offscreen rendering of the panel (visual tests, a settings
/// preview). Call on the main thread.
#[cfg(target_os = "macos")]
pub use platform::macos::preview;

/// Size of the window thumbnails in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl PreviewSize {
    /// Thumbnail height in points.
    pub fn thumb_height(self) -> f64 {
        match self {
            PreviewSize::Small => 96.0,
            PreviewSize::Medium => 136.0,
            PreviewSize::Large => 188.0,
        }
    }

    pub const ALL: [PreviewSize; 3] = [PreviewSize::Small, PreviewSize::Medium, PreviewSize::Large];

    pub fn label(self) -> &'static str {
        match self {
            PreviewSize::Small => "Small",
            PreviewSize::Medium => "Medium",
            PreviewSize::Large => "Large",
        }
    }
}

/// Dock previews settings (camelCase JSON, every field optional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch.
    pub enabled: bool,
    /// How long the pointer must rest on a Dock icon before the panel
    /// appears. Moving to another icon while the panel is open switches
    /// faster.
    pub hover_delay_ms: u32,
    /// Thumbnail size.
    pub preview_size: PreviewSize,
    /// Also show the panel for apps with a single window (otherwise only
    /// for two or more).
    pub show_single_window: bool,
    /// Clicking the Dock icon of the frontmost app minimizes its window (one
    /// window) or cycles through its windows (several), like the Windows
    /// taskbar. Off by default: it changes what a Dock click does.
    pub click_to_cycle: bool,
}

/// Default for [`Config::hover_delay_ms`].
pub const DEFAULT_HOVER_DELAY_MS: u32 = 300;

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            hover_delay_ms: DEFAULT_HOVER_DELAY_MS,
            preview_size: PreviewSize::Medium,
            show_single_window: true,
            click_to_cycle: false,
        }
    }
}

impl Config {
    /// The hover delay, clamped to 0..=2000 ms.
    pub fn hover_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.hover_delay_ms.min(2000) as u64)
    }
}

/// OS permissions the module uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Reading the Dock, listing/focusing windows, the event tap. Without it
    /// nothing is shown (retried every 2 s).
    pub accessibility: bool,
    /// Window thumbnails; without it tiles show the app icon and titles.
    pub screen_recording: bool,
}

/// Current permission state (never prompts). Both false where unsupported.
pub fn permissions() -> Permissions {
    platform::permissions()
}

/// Whether this platform has a real backend (macOS).
pub const SUPPORTED: bool = cfg!(target_os = "macos");

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Entry point.
pub struct DockPreviews;

impl DockPreviews {
    /// Start the module. Only one instance may run per process. A disabled
    /// config still starts (nothing runs until [`Handle::update`] enables
    /// it). On Windows / other platforms this succeeds and does nothing.
    pub fn start(config: Config) -> Result<Handle> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            bail!("dock previews are already running in this process");
        }
        match platform::Runtime::start(config) {
            Ok(rt) => Ok(Handle { rt: Mutex::new(Some(rt)) }),
            Err(e) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }
}

/// A running instance. Dropping it stops everything.
pub struct Handle {
    rt: Mutex<Option<platform::Runtime>>,
}

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut platform::Runtime) -> R) -> Option<R> {
        self.rt.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(f)
    }

    /// Apply new settings (closes the panel if it's open).
    pub fn update(&self, config: Config) {
        self.with(|rt| rt.update(config));
    }

    /// Current settings.
    pub fn config(&self) -> Config {
        self.with(|rt| rt.config()).unwrap_or_default()
    }

    /// Whether the preview panel is on screen.
    pub fn is_shown(&self) -> bool {
        self.with(|rt| rt.is_shown()).unwrap_or(false)
    }

    /// Whether the event tap is installed (macOS: false until Accessibility
    /// is granted, and while disabled).
    pub fn input_active(&self) -> bool {
        self.with(|rt| rt.is_input_active()).unwrap_or(false)
    }

    /// Stop threads and the tap and close the panel. Idempotent.
    pub fn stop(&self) {
        if let Some(mut rt) = self.rt.lock().unwrap_or_else(|e| e.into_inner()).take() {
            rt.stop();
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_serde() {
        let c = Config::default();
        assert!(c.enabled);
        assert_eq!(c.hover_delay_ms, 300);
        assert_eq!(c.preview_size, PreviewSize::Medium);
        assert!(c.show_single_window);
        assert!(!c.click_to_cycle);
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"enabled": true, "hoverDelayMs": 300, "previewSize": "medium", "showSingleWindow": true, "clickToCycle": false})
        );
        let p: Config = serde_json::from_str(r#"{"previewSize":"large","clickToCycle":true}"#).unwrap();
        assert_eq!(p.preview_size, PreviewSize::Large);
        assert!(p.click_to_cycle && p.enabled && p.hover_delay_ms == 300);
        let e: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(e, Config::default());
        assert_eq!(Config { hover_delay_ms: 99_999, ..c }.hover_delay().as_millis(), 2000);
    }
}
