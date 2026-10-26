//! Color Picker — a port of PowerToys Color Picker for macOS and Windows.
//!
//! * A global hotkey (macOS default **Ctrl+Option+C**, Windows **Win+Shift+C**)
//!   starts a pick. On macOS this is AppKit's `NSColorSampler` (the system
//!   magnifier loupe; no Screen Recording permission needed); on Windows a
//!   low-level mouse hook plus a magnifier window that follows the cursor.
//! * The picked color goes to the history (`color-picker-history.json` in the
//!   data directory, newest first, at most [`Config::history_limit`]) and is
//!   copied to the clipboard in [`Config::copied_format`]; depending on
//!   [`ActivationAction`] the editor opens too.
//! * The editor ([`run_editor`]) is an egui window meant to run in its own
//!   process (`powermac color-picker --editor`): history, a big swatch, lighter
//!   and darker variations, hue/saturation/brightness adjustment and every
//!   visible format with a copy button.
//!
//! Formats follow PowerToys' `ColorFormatHelper` exactly; see [`formats`].
//!
//! ```no_run
//! # use std::path::PathBuf;
//! let handle = color_picker::ColorPicker::start(
//!     color_picker::Config::default(),
//!     PathBuf::from("/tmp/powermac"),
//!     || { /* spawn `powermac color-picker --editor` */ },
//! )?;
//! handle.pick_now();
//! // macOS: the main thread must run NSApplication (the tray app does).
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod editor;
pub mod formats;
pub mod history;
mod platform;

#[cfg(test)]
mod formats_tests;
#[cfg(test)]
mod shots;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub use editor::run_editor;
pub use formats::{format_color, Rgb, BUILTIN_FORMATS};

/// A global hotkey in PowerToys' JSON shape
/// (`{"win":true,"ctrl":false,"alt":false,"shift":true,"code":67}`).
///
/// `win` is Command on macOS, `alt` is Option; `code` is a Windows virtual-key
/// code (letters are their ASCII capital). `code == 0` disables the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkey {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub code: u32,
}

impl Hotkey {
    pub const fn new(win: bool, ctrl: bool, alt: bool, shift: bool, code: u32) -> Self {
        Self {
            win,
            ctrl,
            alt,
            shift,
            code,
        }
    }

    pub fn is_set(&self) -> bool {
        self.code != 0
    }

    /// The default: Ctrl+Option+C on macOS (Cmd+Shift+C is taken by browsers'
    /// "inspect element" and Finder), Win+Shift+C elsewhere (as PowerToys).
    pub const fn default_for_platform() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(false, true, true, false, 0x43)
        } else {
            Self::new(true, false, false, true, 0x43)
        }
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mac = cfg!(target_os = "macos");
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push(if mac { "Option" } else { "Alt" }.into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push(if mac { "Cmd" } else { "Win" }.into());
        }
        parts.push(match self.code {
            0 => "(none)".to_string(),
            c @ (0x30..=0x39 | 0x41..=0x5A) => char::from_u32(c).unwrap().to_string(),
            c @ 0x70..=0x87 => format!("F{}", c - 0x6F),
            c => format!("VK 0x{c:02X}"),
        });
        write!(f, "{}", parts.join("+"))
    }
}

/// What the hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ActivationAction {
    /// Pick a color, copy it, and open the editor (PowerToys' default:
    /// "pick color then editor").
    #[default]
    PickThenEditor,
    /// Pick a color and copy it; no window.
    PickThenCopy,
    /// Open the editor directly.
    OpenEditor,
}

/// A user-defined format (PowerToys template syntax, e.g. `#%ReX%GrX%BlX`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CustomFormat {
    pub name: String,
    pub format: String,
}

/// Color Picker settings (camelCase JSON, every field optional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub hotkey: Hotkey,
    pub activation: ActivationAction,
    /// Name of the format copied to the clipboard after a pick.
    pub copied_format: String,
    /// Names of the formats shown in the editor, in order (built-in or custom).
    pub visible_formats: Vec<String>,
    pub custom_formats: Vec<CustomFormat>,
    /// Show the color's name ("Light blue") in the editor.
    pub show_color_name: bool,
    /// How many colors the history keeps.
    pub history_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: Hotkey::default_for_platform(),
            activation: ActivationAction::default(),
            copied_format: "HEX".into(),
            visible_formats: vec!["HEX".into(), "RGB".into(), "HSL".into()],
            custom_formats: Vec::new(),
            show_color_name: false,
            history_limit: 20,
        }
    }
}

impl Config {
    /// The template of format `name`: a custom format of that name wins over
    /// a built-in one.
    pub fn template(&self, name: &str) -> Option<String> {
        self.custom_formats
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.format.clone())
            .or_else(|| formats::builtin_format(name).map(str::to_string))
    }

    /// Every known format: built-ins, then custom ones (name, template).
    pub fn all_formats(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = BUILTIN_FORMATS
            .iter()
            .map(|(n, _)| (n.to_string(), self.template(n).unwrap_or_default()))
            .collect();
        for c in &self.custom_formats {
            if !v.iter().any(|(n, _)| *n == c.name) {
                v.push((c.name.clone(), c.format.clone()));
            }
        }
        v
    }

    /// The visible formats that exist, in order (name, template). Falls back
    /// to HEX when none is visible.
    pub fn visible(&self) -> Vec<(String, String)> {
        let v: Vec<_> = self
            .visible_formats
            .iter()
            .filter_map(|n| self.template(n).map(|t| (n.clone(), t)))
            .collect();
        if v.is_empty() {
            vec![("HEX".into(), formats::builtin_format("HEX").unwrap().into())]
        } else {
            v
        }
    }

    /// `c` in the copied format (HEX if that format doesn't exist).
    pub fn copied_text(&self, c: Rgb) -> String {
        let t = self
            .template(&self.copied_format)
            .unwrap_or_else(|| formats::builtin_format("HEX").unwrap().into());
        format_color(c, &t)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

type Callback = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Inner {
    config: Mutex<Config>,
    data_dir: PathBuf,
    on_open_editor: Callback,
    picking: AtomicBool,
}

impl Inner {
    fn config(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The hotkey / tray action.
    pub(crate) fn activate(self: &Arc<Self>) {
        let cfg = self.config();
        if cfg.activation == ActivationAction::OpenEditor {
            self.open_editor();
            return;
        }
        if self.picking.swap(true, Ordering::SeqCst) {
            return; // a pick is already on screen
        }
        let me = self.clone();
        platform::pick_async(Box::new(move |res| {
            me.picking.store(false, Ordering::SeqCst);
            let Some(c) = res else { return };
            let cfg = me.config();
            me.picked(c, &cfg);
            if cfg.activation == ActivationAction::PickThenEditor {
                me.open_editor();
            }
        }));
    }

    fn picked(&self, c: Rgb, cfg: &Config) {
        if let Err(e) = history::add(&self.data_dir, c, cfg.history_limit) {
            log::warn!("color-picker: saving history: {e:#}");
        }
        let text = cfg.copied_text(c);
        if let Err(e) = copy_text(&text) {
            log::warn!("color-picker: clipboard: {e:#}");
        }
        log::info!("color-picker: picked {c} (copied {text:?})");
    }

    fn open_editor(&self) {
        let cb = self.on_open_editor.clone();
        // Never block the main thread on spawning a process.
        std::thread::spawn(move || cb());
    }
}

/// Put `text` on the clipboard.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_text(text.to_string())?;
    Ok(())
}

/// Entry point.
pub struct ColorPicker;

impl ColorPicker {
    /// Register the hotkey. `on_open_editor` is called (on a worker thread)
    /// when the editor should open; the app typically spawns
    /// `powermac color-picker --editor`, which calls [`run_editor`].
    pub fn start(
        config: Config,
        data_dir: impl Into<PathBuf>,
        on_open_editor: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Handle> {
        let inner = Arc::new(Inner {
            config: Mutex::new(config.clone()),
            data_dir: data_dir.into(),
            on_open_editor: Arc::new(on_open_editor),
            picking: AtomicBool::new(false),
        });
        let hk = inner.clone();
        let hotkey = platform::HotkeyReg::register(config.hotkey, Arc::new(move || hk.activate()))?;
        log::info!("color-picker: started (hotkey {})", config.hotkey);
        Ok(Handle {
            inner,
            hotkey: Mutex::new(Some(hotkey)),
        })
    }
}

/// A running Color Picker. Dropping it unregisters the hotkey.
pub struct Handle {
    inner: Arc<Inner>,
    hotkey: Mutex<Option<platform::HotkeyReg>>,
}

impl Handle {
    /// Apply new settings (re-registers the hotkey if it changed).
    pub fn update(&self, config: Config) {
        let old = std::mem::replace(
            &mut *self.inner.config.lock().unwrap_or_else(|e| e.into_inner()),
            config.clone(),
        );
        if old.hotkey != config.hotkey {
            let mut slot = self.hotkey.lock().unwrap_or_else(|e| e.into_inner());
            slot.take();
            let hk = self.inner.clone();
            match platform::HotkeyReg::register(config.hotkey, Arc::new(move || hk.activate())) {
                Ok(r) => *slot = Some(r),
                Err(e) => log::warn!("color-picker: hotkey {}: {e:#}", config.hotkey),
            }
        }
    }

    /// Do what the hotkey does (tray item).
    pub fn pick_now(&self) {
        self.inner.activate();
    }

    /// The history file's directory.
    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    pub fn stop(self) {}
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.hotkey.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Pick one color and return it (blocking), for the CLI. `None` if cancelled.
///
/// macOS: call it on the main thread of a process that isn't already
/// running NSApplication (it pumps events itself); from another thread it
/// relies on the main thread's run loop.
pub fn pick_once() -> Option<Rgb> {
    platform::pick_blocking()
}

/// Whether picking is supported on this platform.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));
