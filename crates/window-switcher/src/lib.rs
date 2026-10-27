//! Window switcher — a Windows-style Alt+Tab for macOS (and an extra
//! "windows of this app" switcher for Windows): hold a modifier, press Tab
//! to cycle through **windows** (not apps) in most-recently-used order,
//! release to switch.
//!
//! ```no_run
//! let handle = window_switcher::WindowSwitcher::start(window_switcher::Config::default())?;
//! handle.show(); // open it from a tray item (stays open until Return / click / Esc)
//! // ... on macOS the main thread must run NSApplication (or otherwise
//! // service the main dispatch queue) for the overlay to appear.
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Shortcuts
//!
//! | Key (while the switcher is open) | Action |
//! |---|---|
//! | Tab / Shift+Tab (modifier held) | next / previous window |
//! | ← → ↑ ↓, mouse hover | move the selection |
//! | release the modifier, Return, Space, click | switch to the selected window |
//! | Esc (or a click outside) | cancel |
//! | Q / W / M / H / F | quit app / close window / (un)minimize / (un)hide app / full screen |
//!
//! A tap-and-release quicker than [`Config::show_delay_ms`] switches to the
//! previous window without showing the overlay ("fast switch").
//!
//! # Architecture
//!
//! Pure, tested logic: [`config`], [`model`] (window info, MRU order,
//! filtering), [`switcher`] (key state machine), [`layout`] (tile grid) and
//! [`engine`] (glue). Platform runtimes:
//!
//! * macOS — the shared active event tap (`pm-hook`,
//!   [`pm_hook::priority::WINDOW_SWITCHER`]; its health drives the Cmd+Tab
//!   take-over), Accessibility + CGWindowList enumeration
//!   (other Spaces through the private CGS / remote-token APIs AltTab uses),
//!   ScreenCaptureKit thumbnails, `NSPanel` + `NSVisualEffectView` overlay
//!   drawn with Core Animation on the main thread.
//! * Windows — the shared `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks (`pm-hook`,
//!   forwarded to the switcher's GUI thread), `EnumWindows`
//!   (Alt+Tab rules, cloaked windows on other virtual desktops), DWM live
//!   thumbnails (`DwmRegisterThumbnail`) in a topmost tool window.
//!
//! Events injected by Mouse Without Borders (`pm_input::INJECTED_TAG`) are
//! ignored.

pub mod config;
pub mod conflicts;
pub mod engine;
pub mod guard;
pub mod layout;
pub mod model;
pub mod placement;
mod platform;
pub mod switcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Result};

pub use config::{Config, Hotkey, Mods};
pub use model::{Mode, WindowInfo};
pub use switcher::WindowAction;

/// macOS-only helpers to render the overlay offscreen (visual tests, a
/// settings preview). Call on the main thread.
#[cfg(target_os = "macos")]
pub use platform::macos::preview;

/// OS permissions the switcher uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Needed for the keyboard shortcut (active event tap), listing window
    /// titles and focusing windows. Always true on Windows.
    pub accessibility: bool,
    /// Needed for window thumbnails; without it app icons are shown.
    /// Always true on Windows.
    pub screen_recording: bool,
}

/// Whether the system's own app switcher (Cmd+Tab and Cmd+Shift+Tab) is
/// enabled right now. `None` where that doesn't apply (Windows) or the
/// private macOS API is missing.
pub fn system_switcher_enabled() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        platform::macos::cmdtab::system_switcher_enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whether macOS symbolic hotkey `id` is enabled (`None` off macOS). For
/// diagnostics; see [`guard::HOTKEYS_CMD_TAB`].
pub fn system_hotkey_enabled(id: i32) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        platform::macos::cmdtab::hotkey_enabled(id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        None
    }
}

/// The windows the switcher would list for `mode` right now, in its order
/// (front to back). Empty without Accessibility, and on other platforms.
pub fn list_windows(config: &Config, mode: Mode) -> Vec<WindowInfo> {
    #[cfg(target_os = "macos")]
    {
        platform::macos::list_now(config, mode)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (config, mode);
        Vec::new()
    }
}

/// Current permission state (never prompts).
pub fn permissions() -> Permissions {
    platform::permissions()
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Entry point.
pub struct WindowSwitcher;

impl WindowSwitcher {
    /// Start the keyboard hook and window tracking. Only one instance may
    /// run per process. A disabled config still starts (shortcuts are
    /// ignored until [`Handle::update`] enables it).
    ///
    /// On macOS a missing Accessibility permission is not an error: the
    /// event tap is retried every 2 s until it's granted.
    pub fn start(config: Config) -> Result<Handle> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            bail!("the window switcher is already running in this process");
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

/// A running switcher. Dropping it stops everything.
pub struct Handle {
    rt: Mutex<Option<platform::Runtime>>,
}

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut platform::Runtime) -> R) -> Option<R> {
        self.rt.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(f)
    }

    /// Apply new settings (closes the switcher if it's open).
    pub fn update(&self, config: Config) {
        self.with(|rt| rt.update(config));
    }

    /// Current settings.
    pub fn config(&self) -> Config {
        self.with(|rt| rt.config()).unwrap_or_default()
    }

    /// Open the switcher for all windows (e.g. from a tray item). It stays
    /// open until Return / Space / click / Esc since no modifier is held.
    pub fn show(&self) {
        self.with(|rt| rt.show(Mode::AllWindows));
    }

    /// Open the switcher for the frontmost app's windows.
    pub fn show_app_windows(&self) {
        self.with(|rt| rt.show(Mode::CurrentApp));
    }

    /// Close the switcher without switching.
    pub fn hide(&self) {
        self.with(|rt| rt.hide());
    }

    /// Whether the overlay is on screen.
    pub fn is_shown(&self) -> bool {
        self.with(|rt| rt.is_shown()).unwrap_or(false)
    }

    /// Tests / diagnostics: behave as if the keyboard hook had stopped
    /// working (`true`) or works again (`false`). With Cmd+Tab replaced the
    /// system switcher comes back while it's "broken".
    #[doc(hidden)]
    pub fn debug_break_input(&self, broken: bool) {
        self.with(|rt| rt.debug_break_input(broken));
    }

    /// Whether the keyboard hook is installed (macOS: false until
    /// Accessibility is granted).
    pub fn input_active(&self) -> bool {
        self.with(|rt| rt.is_input_active()).unwrap_or(false)
    }

    /// The tracked windows, most recently used first, before filtering.
    pub fn windows(&self) -> Vec<WindowInfo> {
        self.with(|rt| rt.windows()).unwrap_or_default()
    }

    /// Stop hooks and threads and close the overlay. Idempotent.
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
