//! Mouse Utilities — a port of PowerToys' Find My Mouse, Mouse Highlighter
//! and Mouse Pointer Crosshairs for macOS and Windows.
//!
//! ```no_run
//! let handle = mouse_utils::MouseUtils::start(mouse_utils::Config::default())?;
//! handle.toggle_crosshairs();
//! handle.find_mouse();
//! // ... on macOS the main thread must be running NSApplication / the main
//! // dispatch queue for overlays to appear.
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Architecture
//!
//! * [`engine`] is platform-independent: double-Ctrl and shake detection,
//!   hotkey matching, animation timing. Each frame it produces a [`Scene`].
//! * macOS: the shared event tap (`pm-hook`) feeds the engine; overlays (one borderless,
//!   click-through `NSWindow` per screen at `NSScreenSaverWindowLevel`, drawn
//!   with Core Animation layers) live on the **main thread**, driven by a
//!   60 Hz `CFRunLoopTimer` that only runs while something is visible. Work is
//!   posted with the main dispatch queue, so the app must service it (e.g.
//!   `fancyzones::platform::run_main_loop`).
//! * Windows: input from the shared `WH_MOUSE_LL` + `WH_KEYBOARD_LL` hooks
//!   (`pm-hook`); one dedicated thread owns the layered topmost click-through windows (per-pixel alpha via
//!   `UpdateLayeredWindow`, drawn by [`raster`]) and a `SetTimer` animation timer.
//!
//! Events injected by Mouse Without Borders (tagged `pm_input::INJECTED_TAG`)
//! are ignored.

pub mod config;
pub mod engine;
mod platform;
pub mod raster;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use anyhow::{bail, Result};

pub use config::{
    ActivationMethod, Color, Config, CrosshairsConfig, FindMyMouseConfig, HighlighterConfig, Hotkey, Mods, Orientation,
};
pub use engine::{Engine, Scene};

/// How much global input the runtime could hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Mouse + keyboard, hotkeys are swallowed (active tap / LL hooks).
    Full,
    /// Mouse + keyboard observed but hotkeys also reach the focused app
    /// (macOS listen-only tap: Input Monitoring granted, Accessibility not).
    ListenOnly,
    /// Only mouse events (no hotkeys / double Ctrl). Programmatic triggers
    /// and cursor-following still work.
    MouseOnly,
    /// No global input; only the [`Handle`] methods work.
    None,
}

pub(crate) struct State {
    pub engine: Engine,
    /// The renderer's frame timer is (or is about to be) running.
    pub timer_running: bool,
}

/// State shared between the input thread, the renderer and the [`Handle`].
pub(crate) struct Shared {
    state: Mutex<State>,
    kick: OnceLock<Box<dyn Fn() + Send + Sync>>,
    input_mode: Mutex<InputMode>,
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` on the engine, then make sure the renderer is ticking if the
    /// engine now needs frames.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Engine, Instant) -> R) -> R {
        let mut st = self.lock();
        let r = f(&mut st.engine, Instant::now());
        let kick = st.engine.needs_frames() && !st.timer_running;
        if kick {
            st.timer_running = true;
        }
        drop(st);
        if kick {
            if let Some(k) = self.kick.get() {
                k();
            }
        }
        r
    }

    pub(crate) fn set_kick(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.kick.set(Box::new(f));
    }

    pub(crate) fn set_input_mode(&self, m: InputMode) {
        *self.input_mode.lock().unwrap_or_else(|e| e.into_inner()) = m;
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn current_input_mode(&self) -> InputMode {
        *self.input_mode.lock().unwrap_or_else(|e| e.into_inner())
    }
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Entry point.
pub struct MouseUtils;

impl MouseUtils {
    /// Start input capture and the overlay renderer. Only one instance may
    /// run per process.
    ///
    /// Missing input permissions (macOS) are not an error: the runtime falls
    /// back to fewer events (see [`Handle::input_mode`]) and logs a warning.
    pub fn start(config: Config) -> Result<Handle> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            bail!("mouse-utils is already running in this process");
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State { engine: Engine::new(config, platform::double_click_time()), timer_running: false }),
            kick: OnceLock::new(),
            input_mode: Mutex::new(InputMode::None),
        });
        match platform::Runtime::start(shared.clone()) {
            Ok(rt) => {
                // auto_activate features may need frames right away
                shared.with(|_, _| ());
                Ok(Handle { shared, runtime: Some(rt) })
            }
            Err(e) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }
}

/// A running Mouse Utilities instance. Dropping it stops everything.
pub struct Handle {
    shared: Arc<Shared>,
    runtime: Option<platform::Runtime>,
}

impl Handle {
    /// Apply new settings. Disabling a feature hides it immediately.
    pub fn update(&self, config: Config) {
        self.shared.with(|e, _| e.set_config(config));
    }

    /// Current settings.
    pub fn config(&self) -> Config {
        self.shared.lock().engine.config().clone()
    }

    /// Toggle the click highlighter; returns the new state (always false
    /// while `highlighter.enabled` is false).
    pub fn toggle_highlighter(&self) -> bool {
        self.shared.with(|e, _| e.toggle_highlighter())
    }

    /// Toggle the crosshairs; returns the new state (always false while
    /// `crosshairs.enabled` is false).
    pub fn toggle_crosshairs(&self) -> bool {
        self.shared.with(|e, _| e.toggle_crosshairs())
    }

    /// Show the Find My Mouse spotlight now (e.g. from a tray menu item).
    /// No-op while `findMyMouse.enabled` is false.
    pub fn find_mouse(&self) {
        self.shared.with(|e, now| e.find_mouse(now));
    }

    /// Fade the spotlight out (no-op if it isn't shown).
    pub fn hide_spotlight(&self) {
        self.shared.with(|e, now| e.dismiss_spotlight(now));
    }

    pub fn highlighter_on(&self) -> bool {
        self.shared.lock().engine.highlighter_on()
    }

    pub fn crosshairs_on(&self) -> bool {
        self.shared.lock().engine.crosshairs_on()
    }

    pub fn spotlight_active(&self) -> bool {
        self.shared.lock().engine.spotlight_active()
    }

    /// Which global input the runtime could hook.
    pub fn input_mode(&self) -> InputMode {
        *self.shared.input_mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop input capture and close the overlays. Idempotent.
    ///
    /// On macOS, overlay teardown is posted to the main queue unless called
    /// on the main thread.
    pub fn stop(&mut self) {
        if let Some(mut rt) = self.runtime.take() {
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

/// Pump the macOS main run loop for `dur` (main thread only) so overlays
/// are drawn — for demos and tests of binaries that don't run NSApplication.
/// On other platforms this just sleeps.
pub fn run_main_loop_for(dur: std::time::Duration) {
    platform::run_main_loop_for(dur)
}

/// Diagnostics: sample the rendered overlay pixel (premultiplied RGBA) at
/// `cursor + offset` for each offset. macOS only (main thread, software
/// render of the layer tree); `None` elsewhere or where no overlay exists.
#[doc(hidden)]
pub fn debug_probe(offsets: &[(f64, f64)]) -> Vec<Option<[u8; 4]>> {
    platform::debug_probe(offsets)
}
