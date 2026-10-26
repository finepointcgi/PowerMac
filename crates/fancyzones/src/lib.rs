//! # fancyzones — a Rust port of PowerToys FancyZones for macOS and Windows
//!
//! * [`layout`] / [`navigation`] / [`engine`] / [`config`] are platform
//!   independent (zone math identical to FancyZones' `LayoutConfigurator`,
//!   hit-testing from `Layout::ZonesFromPoint`, keyboard navigation from
//!   `WindowKeyboardSnap`, JSON compatible with FancyZones' files).
//! * [`platform`] contains the OS backends (Accessibility + CGEventTap +
//!   AppKit overlay on macOS; WinEvent hooks + low-level keyboard hook +
//!   layered GDI overlay on Windows).
//!
//! ## Integrating into an app binary
//!
//! ```no_run
//! let config = fancyzones::Config::load_or_default().unwrap();
//! let handle = fancyzones::FancyZones::start(config).unwrap();
//! // ... on macOS the MAIN thread must now run the AppKit run loop:
//! #[cfg(target_os = "macos")]
//! fancyzones::platform::run_main_loop(); // or your own NSApplication::run()
//! drop(handle); // stops hooks/threads
//! ```
//!
//! **macOS threading:** `FancyZones::start` may be called from any thread. It
//! spawns an event-tap thread (CFRunLoop) and a worker thread (Accessibility
//! calls). The zone overlay is AppKit UI and is created/updated by blocks
//! posted to the *main dispatch queue*; therefore the process' main thread
//! must run an `NSApplication` run loop (`NSApplication::run`, or
//! [`platform::run_main_loop`]) or at least a `CFRunLoop` that services the
//! main queue. Without it, keyboard snapping and drop-to-zone still work but
//! no overlay is drawn. The process needs the *Accessibility* permission
//! (System Settings → Privacy & Security → Accessibility) for both the event
//! tap and moving windows; `start` returns an error if it is missing.
//!
//! **Windows threading:** `start` spawns one thread that owns the WinEvent
//! hook, the `WH_KEYBOARD_LL` hook and the overlay windows and pumps their
//! message loop. Nothing is required from the caller's thread.

pub mod config;
pub mod engine;
pub mod geometry;
pub mod layout;
pub mod navigation;
pub mod platform;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;

pub use config::{Config, Modifiers, Settings};
pub use engine::{Engine, Monitor, WindowSystem, WorkArea};
pub use geometry::{Point, Rect};
pub use layout::{AppliedLayout, CustomLayout, CustomLayoutInfo, Layout, LayoutType};
pub use navigation::Direction;

/// Options for [`FancyZones::start_with`].
#[derive(Debug, Clone)]
pub struct StartOptions {
    /// Where configuration changes are saved (`None` = don't persist).
    pub config_path: Option<PathBuf>,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self { config_path: Config::default_path() }
    }
}

/// Packed hotkey settings readable from hook callbacks without locking.
pub(crate) mod hotkey_bits {
    use crate::config::{Modifiers, Settings};
    pub const ENABLED: u32 = 1;
    pub const POSITION: u32 = 2;
    fn mods(m: &Modifiers) -> u32 {
        (m.ctrl as u32) | (m.alt as u32) << 1 | (m.shift as u32) << 2 | (m.meta as u32) << 3
    }
    pub fn pack(s: &Settings) -> u32 {
        let mut v = 0;
        if s.override_snap_hotkeys {
            v |= ENABLED;
        }
        if s.move_windows_based_on_position {
            v |= POSITION;
        }
        v | mods(&s.snap_modifiers) << 4 | mods(&s.extend_modifiers) << 8
    }
    /// What a key press with modifiers `m` means, given packed bits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Action {
        Snap,
        Extend,
    }
    pub fn classify(bits: u32, m: Modifiers, horizontal: bool) -> Option<Action> {
        if bits & ENABLED == 0 {
            return None;
        }
        let pos = bits & POSITION != 0;
        let mm = mods(&m);
        if mm == (bits >> 4) & 0xF && mm != 0 && (pos || horizontal) {
            return Some(Action::Snap);
        }
        // Extending is position based in FancyZones; only active in that mode.
        if pos && mm == (bits >> 8) & 0xF && mm != 0 {
            return Some(Action::Extend);
        }
        None
    }
}

pub(crate) struct Shared {
    pub engine: Mutex<Engine>,
    pub config_path: Option<PathBuf>,
    pub hotkeys: AtomicU32,
}

impl Shared {
    pub fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn persist(&self, cfg: &Config) {
        if let Some(p) = &self.config_path {
            if let Err(e) = cfg.save_to(p) {
                log::warn!("fancyzones: saving config failed: {e:#}");
            }
        }
    }
}

/// Entry point.
pub struct FancyZones;

impl FancyZones {
    /// Start FancyZones with the config persisted to [`Config::default_path`].
    pub fn start(config: Config) -> Result<Handle> {
        Self::start_with(config, StartOptions::default())
    }

    pub fn start_with(config: Config, opts: StartOptions) -> Result<Handle> {
        let hotkeys = AtomicU32::new(hotkey_bits::pack(&config.settings));
        let shared = Arc::new(Shared { engine: Mutex::new(Engine::new(config)), config_path: opts.config_path, hotkeys });
        let runtime = platform::Runtime::start(shared.clone())?;
        Ok(Handle { shared, runtime: Some(runtime) })
    }
}

/// Running FancyZones instance. Dropping it stops all hooks and threads.
pub struct Handle {
    shared: Arc<Shared>,
    runtime: Option<platform::Runtime>,
}

impl Handle {
    pub fn stop(&mut self) {
        if let Some(mut rt) = self.runtime.take() {
            rt.stop();
        }
    }

    pub fn is_running(&self) -> bool {
        self.runtime.is_some()
    }

    /// Apply a layout to the monitor with id `monitor_id` (see [`Handle::monitors`]) and persist it.
    pub fn apply_layout(&self, monitor_id: &str, layout: AppliedLayout) -> Result<()> {
        let mut e = self.shared.engine();
        if layout.kind == LayoutType::Custom && layout::find_custom(&e.config.custom_layouts, &layout.uuid).is_none() {
            anyhow::bail!("custom layout {} not found", layout.uuid);
        }
        e.apply_layout(monitor_id, layout);
        self.shared.persist(&e.config);
        Ok(())
    }

    /// Add/replace a custom layout and persist it.
    pub fn upsert_custom_layout(&self, layout: CustomLayout) {
        let mut e = self.shared.engine();
        e.config.upsert_custom_layout(layout);
        self.shared.persist(&e.config);
    }

    /// Keyboard-snap the focused window (same as the hotkey).
    pub fn snap_focused(&self, direction: Direction) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        let r = self.shared.engine().snap_focused(&ws, direction);
        self.persist_history();
        r
    }

    /// Extend the focused window's zones towards `direction`.
    pub fn extend_focused(&self, direction: Direction) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        self.shared.engine().extend_focused(&ws, direction)
    }

    /// Snap the focused window into zone `zone` (0-based) of `monitor_id`
    /// (or of the monitor it is currently on).
    pub fn snap_focused_to_zone(&self, monitor_id: Option<&str>, zone: usize) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        let r = self.shared.engine().snap_focused_to_zone(&ws, monitor_id, zone);
        self.persist_history();
        r
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        platform::NativeWindowSystem::new().monitors()
    }

    /// Monitors with their instantiated layouts (global zone rects).
    pub fn work_areas(&self) -> Result<Vec<WorkArea>> {
        let m = self.monitors()?;
        Ok(self.shared.engine().work_areas(&m))
    }

    pub fn config(&self) -> Config {
        self.shared.engine().config.clone()
    }

    /// Change settings (hotkeys etc. take effect immediately) and persist.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut e = self.shared.engine();
        f(&mut e.config.settings);
        self.shared.hotkeys.store(hotkey_bits::pack(&e.config.settings), Ordering::SeqCst);
        self.shared.persist(&e.config);
    }

    fn persist_history(&self) {
        let e = self.shared.engine();
        self.shared.persist(&e.config);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::hotkey_bits::*;
    use super::*;

    #[test]
    fn hotkey_classification() {
        let mut s = Settings {
            snap_modifiers: Modifiers::new(true, true, false, false),
            extend_modifiers: Modifiers::new(true, true, false, true),
            ..Default::default()
        };
        let b = pack(&s);
        let snap = Modifiers::new(true, true, false, false);
        let ext = Modifiers::new(true, true, false, true);
        assert_eq!(classify(b, snap, true), Some(Action::Snap));
        assert_eq!(classify(b, snap, false), None); // up/down need position mode
        assert_eq!(classify(b, ext, true), None);
        assert_eq!(classify(b, Modifiers::new(true, false, false, false), true), None);
        s.move_windows_based_on_position = true;
        let b = pack(&s);
        assert_eq!(classify(b, snap, false), Some(Action::Snap));
        assert_eq!(classify(b, ext, false), Some(Action::Extend));
        s.override_snap_hotkeys = false;
        assert_eq!(classify(pack(&s), snap, true), None);
    }
}
