//! Text Extractor — a port of PowerToys Text Extractor (PowerOCR).
//!
//! Press the hotkey (macOS default **Ctrl+Option+T**, Windows **Win+Shift+T**
//! as in PowerToys), drag a rectangle over any screen (Esc or a right click
//! cancels), and the text in it is recognized and copied to the clipboard,
//! with a small confirmation ("Copied 42 words").
//!
//! * macOS: a dimmed full-screen overlay per display, capture with
//!   ScreenCaptureKit (`SCScreenshotManager`, macOS 14+; on 15.2+
//!   `captureImageInRect:`), falling back to `CGWindowListCreateImage`; OCR
//!   with Vision (`VNRecognizeTextRequest`, accurate level, language
//!   correction, automatic or configured languages). Capturing needs the
//!   **Screen Recording** permission ([`screen_recording_permission`],
//!   [`request_screen_recording_permission`]); OCR of files and clipboard
//!   images doesn't.
//! * Windows: the screen is frozen into a dimmed overlay covering the virtual
//!   desktop, the selection is cut from it, and `Windows.Media.Ocr` reads it.
//!
//! ```no_run
//! let handle = text_extractor::TextExtractor::start(text_extractor::Config::default())?;
//! handle.capture_now();
//! let text = text_extractor::ocr_image("/tmp/screenshot.png")?;
//! # Ok::<(), anyhow::Error>(())
//! ```

mod platform;
pub mod text;

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub use text::Fragment;

/// A global hotkey in PowerToys' JSON shape (`win` = Command on macOS,
/// `alt` = Option; `code` is a Windows VK code, 0 = disabled).
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

    /// Ctrl+Option+T on macOS (Cmd+Shift+T reopens tabs in browsers; the
    /// FancyZones Ctrl+Option shortcuts are arrows, `, [, ] and X only),
    /// Win+Shift+T elsewhere (as PowerToys).
    pub const fn default_for_platform() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(false, true, true, false, 0x54)
        } else {
            Self::new(true, false, false, true, 0x54)
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

/// Text Extractor settings (camelCase JSON, every field optional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub hotkey: Hotkey,
    /// BCP-47 language tags in order of preference (`["en-US", "de-DE"]`).
    /// Empty = automatic (macOS: Vision detects the language; Windows: the
    /// user profile languages).
    pub languages: Vec<String>,
    /// Join all lines into one (line breaks become spaces).
    pub single_line: bool,
    /// Show "Copied N words" after a capture (and errors).
    pub show_toast: bool,
    /// Let the recognizer fix words with its language model (macOS).
    pub language_correction: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: Hotkey::default_for_platform(),
            languages: Vec::new(),
            single_line: false,
            show_toast: true,
            language_correction: true,
        }
    }
}

impl Config {
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

/// Screen Recording permission state (macOS; always granted elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
}

/// Whether this process may capture the screen (no prompt).
pub fn screen_recording_permission() -> Permission {
    if platform::screen_recording_granted() {
        Permission::Granted
    } else {
        Permission::Denied
    }
}

/// Ask for Screen Recording permission. macOS shows its prompt (once per
/// app; afterwards the user has to enable it in System Settings → Privacy &
/// Security → Screen Recording). Returns whether it is granted now.
pub fn request_screen_recording_permission() -> bool {
    platform::request_screen_recording()
}

/// Recognize the text in an image file (PNG, JPEG, HEIC, TIFF, …) with the
/// default settings.
pub fn ocr_image(path: impl AsRef<Path>) -> anyhow::Result<String> {
    ocr_image_with(path, &Config::default())
}

/// [`ocr_image`] with explicit languages / single-line / correction settings.
pub fn ocr_image_with(path: impl AsRef<Path>, config: &Config) -> anyhow::Result<String> {
    let path = path.as_ref();
    anyhow::ensure!(path.exists(), "{} does not exist", path.display());
    let frags = platform::ocr_file(path, config)?;
    Ok(text::assemble(frags, config.single_line))
}

/// Recognize the text in an RGBA image (8 bits per channel, row-major).
pub fn ocr_rgba(
    width: usize,
    height: usize,
    rgba: &[u8],
    config: &Config,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        width > 0 && height > 0 && rgba.len() >= width * height * 4,
        "bad image buffer"
    );
    let frags = platform::ocr_rgba(width, height, rgba, config)?;
    Ok(text::assemble(frags, config.single_line))
}

/// Recognize the text in the image on the clipboard.
pub fn ocr_clipboard(config: &Config) -> anyhow::Result<String> {
    let img = arboard::Clipboard::new()?
        .get_image()
        .map_err(|e| anyhow::anyhow!("no image on the clipboard ({e})"))?;
    ocr_rgba(img.width, img.height, &img.bytes, config)
}

/// Select a region and recognize it, blocking (for the CLI:
/// `powermac ocr --select`). `Ok(None)` if cancelled. Doesn't copy anything.
/// macOS: call it on the main thread of a process that isn't running
/// NSApplication yet (it pumps events itself).
pub fn capture_once(config: &Config) -> anyhow::Result<Option<String>> {
    platform::capture_blocking(config.clone())
}

/// The recognition languages the OS offers (BCP-47 tags).
pub fn supported_languages() -> Vec<String> {
    platform::supported_languages()
}

/// Put `text` on the clipboard.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_text(text.to_string())?;
    Ok(())
}

pub(crate) struct Inner {
    config: Mutex<Config>,
    busy: AtomicBool,
}

impl Inner {
    pub(crate) fn config(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Hotkey / tray action: select, capture, recognize, copy.
    pub(crate) fn activate(self: &Arc<Self>) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        platform::capture_selection(
            self.config(),
            Box::new(move |res| {
                let cfg = me.config();
                me.finish(res, &cfg);
                me.busy.store(false, Ordering::SeqCst);
            }),
        );
    }

    fn finish(&self, res: anyhow::Result<Option<String>>, cfg: &Config) {
        match res {
            Ok(None) => {} // cancelled
            Ok(Some(text)) if text.trim().is_empty() => {
                log::info!("text-extractor: no text found");
                if cfg.show_toast {
                    platform::toast("No text found");
                }
            }
            Ok(Some(text)) => {
                let n = text::word_count(&text);
                match copy_text(&text) {
                    Ok(()) => {
                        log::info!("text-extractor: copied {n} words");
                        if cfg.show_toast {
                            platform::toast(&if n == 1 {
                                "Copied 1 word".to_string()
                            } else {
                                format!("Copied {n} words")
                            });
                        }
                    }
                    Err(e) => {
                        log::warn!("text-extractor: clipboard: {e:#}");
                        if cfg.show_toast {
                            platform::toast("Couldn't copy the text");
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("text-extractor: {e:#}");
                if cfg.show_toast {
                    platform::toast(&format!("{e}"));
                }
            }
        }
    }
}

/// Entry point.
pub struct TextExtractor;

impl TextExtractor {
    /// Register the hotkey.
    pub fn start(config: Config) -> anyhow::Result<Handle> {
        let inner = Arc::new(Inner {
            config: Mutex::new(config.clone()),
            busy: AtomicBool::new(false),
        });
        let hk = inner.clone();
        let reg = platform::HotkeyReg::register(config.hotkey, Arc::new(move || hk.activate()))?;
        log::info!("text-extractor: started (hotkey {})", config.hotkey);
        Ok(Handle {
            inner,
            hotkey: Mutex::new(Some(reg)),
        })
    }
}

/// A running Text Extractor. Dropping it unregisters the hotkey.
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
                Err(e) => log::warn!("text-extractor: hotkey {}: {e:#}", config.hotkey),
            }
        }
    }

    /// Do what the hotkey does (tray item).
    pub fn capture_now(&self) {
        self.inner.activate();
    }

    pub fn stop(self) {}
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.hotkey.lock().unwrap_or_else(|e| e.into_inner()).take();
        if self.inner.busy.load(Ordering::SeqCst) {
            platform::cancel_selection();
        }
    }
}

/// Offscreen renders of the selection overlay and the toast into PNGs in
/// `dir` (macOS, main thread). For checking the look without capturing.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn debug_render(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    platform::debug_render(dir)
}

/// Capture a screen region (global top-left points) and recognize it,
/// without the overlay (diagnostics; needs Screen Recording). Call off the
/// main thread.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn debug_capture_rect(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    config: &Config,
) -> anyhow::Result<String> {
    platform::capture_rect_text(x, y, w, h, config)
}

/// Show the confirmation toast (diagnostics).
#[doc(hidden)]
pub fn debug_toast(text: &str) {
    platform::toast(text);
}

/// Open the real selection overlay for `ms` ms and cancel it
/// programmatically; true if it opened and closed cleanly (main thread).
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn debug_overlay_smoke(ms: u64) -> anyhow::Result<bool> {
    platform::overlay_smoke(ms)
}

/// Whether OCR / capture are implemented on this platform.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));
