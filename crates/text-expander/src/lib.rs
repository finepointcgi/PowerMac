//! Text expansion (like TextExpander / espanso): type an abbreviation
//! anywhere (`;addr`, `:date`, `btw`) and it is replaced with its snippet.
//!
//! - A snippet expands as soon as its trigger's last character is typed,
//!   or, with `wordBoundary`, when a space / Return / Tab / punctuation
//!   follows a trigger that starts a word (the separator is kept).
//! - Replacements can contain variables ([`vars`]): `{date:%d.%m.%Y}`,
//!   `{time}`, `{clipboard}`, `{cursor}`, `{uuid}`, `{random:6}`.
//! - Backspace right after an expansion undoes it (puts the typed trigger
//!   back), unless `undoWithBackspace` is off.
//! - Snippets can be limited to apps; password managers (and optionally
//!   terminals) are excluded, and nothing is read while macOS secure input
//!   is on (password fields).
//! - espanso match files can be imported ([`espanso::import`]).
//!
//! ```no_run
//! use text_expander::{Config, TextExpander};
//! let mut cfg = Config::default();
//! cfg.enabled = true;
//! let h = TextExpander::start(cfg)?;
//! // ...
//! h.stop();
//! # anyhow::Ok(())
//! ```
//!
//! # Config (`"textExpander"` in the app config)
//!
//! ```json
//! {
//!   "enabled": true,
//!   "snippets": [
//!     { "trigger": ";date", "replacement": "{date:%Y-%m-%d}" },
//!     { "trigger": ";shrug", "replacement": "¯\\_(ツ)_/¯" },
//!     { "trigger": "btw", "replacement": "by the way", "wordBoundary": true, "caseSensitive": false },
//!     { "trigger": ";sig", "replacement": "Best,\nAda", "apps": ["com.apple.mail", "outlook.exe"] },
//!     { "trigger": ";div", "replacement": "<div>{cursor}</div>" }
//!   ],
//!   "undoWithBackspace": true,
//!   "pasteThreshold": 200,
//!   "clipboardRestoreDelayMs": 400,
//!   "excludedApps": ["com.1password.1password", "KeePass.exe"],
//!   "excludeTerminals": false
//! }
//! ```
//!
//! # How it works
//!
//! [`engine::Expander`] keeps a rolling buffer of the characters typed
//! (reset by mouse clicks, arrows and other navigation keys, Cmd/Ctrl
//! shortcuts and focus changes; Backspace removes the last one). When a
//! trigger completes, the completing key is swallowed, the rest of the
//! trigger is deleted with Backspaces, and the replacement is typed with
//! unicode key events — or, when longer than `pasteThreshold`, pasted
//! through the clipboard, which is saved and restored afterwards. A
//! `{cursor}` is reached with Left-arrow presses.
//!
//! Input comes from the shared hook ([`pm_hook`]) at
//! [`pm_hook::priority::TEXT_EXPANDER`], after Keyboard Manager (so remapped
//! keys are what gets typed into the buffer).
//! **macOS:** the shared active `CGEventTap` (Accessibility permission); the
//! typed text comes from `CGEventKeyboardGetUnicodeString`, so any keyboard
//! layout works. Injected events are tagged with [`INJECTED_TAG`] ("PMTE"),
//! which this subscriber and Keyboard Manager skip.
//! **Windows:** the shared `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks, `ToUnicodeEx`
//! (without touching the dead-key state) and `SendInput` with `KEYEVENTF_UNICODE`.

use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};

pub mod config;
pub mod engine;
pub mod espanso;
pub mod vars;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::*;
    pub struct Hook;
    pub fn start(_: Arc<Shared>) -> Result<Hook> {
        Err(anyhow!("text-expander supports macOS and Windows only"))
    }
    pub fn paste_and_move(_: usize) -> Result<()> {
        Ok(())
    }
}

pub use config::{Config, Snippet};
pub use engine::{Decision, Expander, Insert, Key, Plan};

/// Tag on every event the text expander injects ("PMTE"; macOS
/// `kCGEventSourceUserData`, Windows `dwExtraInfo`).
pub const INJECTED_TAG: usize = keyboard_manager::TEXT_EXPANDER_TAG;

/// The replacement of `snippet` as it would be inserted now (variables
/// evaluated, `{cursor}` removed), for the settings UI.
pub fn expand_preview(snippet: &Snippet) -> String {
    vars::preview(&snippet.replacement)
}

/// Whether the OS lets us watch and post keys (macOS: Accessibility).
pub fn has_permission() -> bool {
    keyboard_manager::has_permission(false)
}

pub(crate) enum Job {
    /// Paste `text` through the clipboard, then press Left `left` times.
    Paste { text: String, left: usize },
}

/// State shared by the handle, the hook thread and the worker.
pub(crate) struct Shared {
    pub(crate) expander: Mutex<Expander>,
    pub(crate) config: Mutex<Config>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
}

impl Shared {
    pub(crate) fn submit(&self, job: Job) {
        if let Ok(g) = self.jobs.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(job);
            }
        }
    }
}

/// Entry point.
pub struct TextExpander;

impl TextExpander {
    /// Install the keyboard hook and start expanding. Fails without side
    /// effects when the hook can't be installed (macOS: no Accessibility).
    pub fn start(config: Config) -> Result<Handle> {
        for w in config.validate() {
            log::warn!("text-expander: {w}");
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let shared = Arc::new(Shared {
            expander: Mutex::new(Expander::new(&config)),
            config: Mutex::new(config),
            jobs: Mutex::new(Some(tx)),
        });
        let hook = platform::start(shared.clone())?;
        let s2 = shared.clone();
        let worker = std::thread::Builder::new()
            .name("te-paste".into())
            .spawn(move || {
                for job in rx {
                    match job {
                        Job::Paste { text, left } => {
                            let delay = s2
                                .config
                                .lock()
                                .map(|c| c.clipboard_restore_delay_ms)
                                .unwrap_or(400);
                            if let Err(e) = paste_text(&text, left, delay) {
                                log::warn!("text-expander: paste: {e:#}");
                            }
                        }
                    }
                }
            })?;
        Ok(Handle {
            shared,
            hook: Some(hook),
            worker: Some(worker),
        })
    }
}

/// Save the clipboard, put `text` on it, paste, move the caret, and put
/// the previous contents back (unless something else changed the clipboard
/// meanwhile).
fn paste_text(text: &str, left: usize, restore_delay_ms: u64) -> Result<()> {
    use keyboard_manager::clipboard;
    let snapshot = clipboard::Snapshot::capture()
        .map_err(|e| log::warn!("text-expander: clipboard snapshot: {e:#}"))
        .ok();
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
        cb.set_text(text.to_string())
            .map_err(|e| anyhow!("clipboard: {e}"))?;
    }
    let ours = clipboard::change_count();
    // Give the pasteboard server a moment before the app reads it.
    std::thread::sleep(Duration::from_millis(30));
    platform::paste_and_move(left)?;
    if let Some(s) = snapshot {
        std::thread::sleep(Duration::from_millis(restore_delay_ms));
        if clipboard::change_count() == ours {
            s.restore()?;
        } else {
            log::info!("text-expander: clipboard changed meanwhile; not restoring");
        }
    }
    Ok(())
}

/// A running text expander. Dropping it stops it.
pub struct Handle {
    shared: Arc<Shared>,
    hook: Option<platform::Hook>,
    worker: Option<JoinHandle<()>>,
}

impl Handle {
    /// Replace the configuration (snippets take effect immediately).
    pub fn update(&self, config: Config) -> Result<()> {
        for w in config.validate() {
            log::warn!("text-expander: {w}");
        }
        self.shared
            .expander
            .lock()
            .map_err(|_| anyhow!("expander poisoned"))?
            .set_config(&config);
        *self
            .shared
            .config
            .lock()
            .map_err(|_| anyhow!("config poisoned"))? = config;
        Ok(())
    }

    /// The current configuration.
    pub fn config(&self) -> Config {
        self.shared
            .config
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    /// Stop: remove the hook and finish a pending paste (restoring the
    /// clipboard).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(hook) = self.hook.take() else { return };
        drop(hook);
        if let Ok(mut g) = self.shared.jobs.lock() {
            g.take();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
