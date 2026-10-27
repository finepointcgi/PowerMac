//! Keyboard Manager for macOS and Windows: a port of PowerToys Keyboard
//! Manager plus "Paste as plain text" from PowerToys Advanced Paste.
//!
//! - **Key remaps**: one key to another key (CapsLock -> Esc, swap Option and
//!   Command), to a shortcut, to text, or to nothing.
//! - **Shortcut remaps**: a shortcut to another shortcut (Ctrl+Left ->
//!   Option+Left), to a key, to nothing, to text, to a program, or to a URL.
//!   Each can be limited to one app (macOS bundle id or app name, Windows
//!   process name).
//! - **Windows-style keys** (macOS): an opt-in preset (`"windowsStyle"`)
//!   for people used to Windows: Home/End, Ctrl+Arrow word jumps, Ctrl+
//!   Backspace, optional Ctrl+C/V/... → Cmd, and Finder cut/paste, Enter to
//!   open, Delete to trash, Backspace to go up. See [`windows_style`].
//! - **Hyper key** (`"hyperKey"`, off by default): Caps Lock held =
//!   Ctrl+Option+Shift+Cmd (or Control), tapped alone = Escape / nothing /
//!   Caps Lock; or Caps Lock = Escape. See [`hyper`].
//! - **Paste as plain text**: a hotkey (Cmd+Option+Shift+V on macOS,
//!   Win+Ctrl+Alt+V on Windows, as in PowerToys) that strips the clipboard
//!   down to plain text and pastes it, optionally putting the original rich
//!   contents back afterwards.
//!
//! ```no_run
//! use keyboard_manager::{Config, KeyboardManager};
//! let cfg = Config::from_json(r#"{"remapKeys": [{"from": "CapsLock", "to": "Esc"}]}"#)?;
//! let km = KeyboardManager::start(cfg)?;   // installs the event tap / hook
//! // ... later
//! km.update(Config::default())?;           // hot-reload
//! km.paste_plain_text()?;                  // programmatic trigger
//! km.stop();                               // (dropping the handle also stops)
//! # anyhow::Ok(())
//! ```
//!
//! # Config format
//!
//! JSON, camelCase, every field optional (`{}` is the default config: no
//! remaps, paste-as-plain-text on with its default hotkey).
//!
//! ```json
//! {
//!   "enabled": true,
//!   "remapKeys": [
//!     { "from": "CapsLock", "to": "Esc" },
//!     { "from": "LOption", "to": "LCmd" },
//!     { "from": "LCmd",    "to": "LOption" },
//!     { "from": "F13",     "to": "Cmd+Shift+4" },
//!     { "from": "Insert",  "to": "disabled" },
//!     { "from": "F14",     "to": { "text": "Kind regards,\nAda" } }
//!   ],
//!   "remapShortcuts": [
//!     { "from": "Ctrl+Left",  "to": "Option+Left" },
//!     { "from": "Ctrl+Right", "to": "Option+Right" },
//!     { "from": "Ctrl+C", "to": "Cmd+C", "app": "com.microsoft.VSCode" },
//!     { "from": "Cmd+Q",  "to": "disabled" },
//!     { "from": "Ctrl+Alt+T", "to": { "run": { "path": "/System/Applications/Utilities/Terminal.app" } } },
//!     { "from": "Ctrl+Alt+G", "to": { "openUri": "https://github.com" } },
//!     { "from": "Ctrl+Shift+D", "to": { "text": "ada@example.com" } }
//!   ],
//!   "pastePlainText": {
//!     "enabled": true,
//!     "hotkey": "Cmd+Option+Shift+V",
//!     "restoreClipboard": false,
//!     "restoreDelayMs": 500
//!   },
//!   "windowsStyle": { "enabled": true, "ctrlShortcutsToCmd": false },
//!   "hyperKey": { "mode": "hyper", "tapAction": "escape", "tapTimeoutMs": 200 }
//! }
//! ```
//!
//! **Keys** are Windows virtual-key codes, written as a number (`20`), a hex
//! string (`"0x14"`) or a case-insensitive name (`"CapsLock"`, `"A"`,
//! `"F13"`, `"Left"`, `"Numpad5"`, `";"`, `"VolumeUp"`, ... see
//! [`keys::parse_key`]). The same config works on both platforms because
//! macOS keys are translated: **Command is the Windows key** (`Cmd` = `Win`,
//! `LCmd` = `LWin`) and **Option is Alt** (`Option` = `Alt`). `Ctrl`,
//! `Shift`, `Alt`/`Option` and `Win`/`Cmd` mean either side; prefix `L`/`R`
//! for one side (`LCtrl`, `RCmd`, `ROption`).
//!
//! **Shortcuts** are `"Mod+Mod+Key"` strings (or arrays like
//! `["Ctrl", "C"]`): one or more modifiers plus exactly one other key. A
//! shortcut matches only with exactly those modifiers held (Ctrl+C does not
//! fire for Ctrl+Shift+C).
//!
//! **Targets** (`"to"`):
//!
//! | JSON | meaning |
//! |---|---|
//! | `"Esc"`, `27`, `"LCtrl"` | a single key (may be a modifier) |
//! | `"Cmd+C"`, `["Ctrl","C"]`, `{"keys": "Cmd+C"}` | a shortcut |
//! | `"disabled"`, `null`, `{"disabled": true}` | swallow the key/shortcut |
//! | `{"text": "..."}` | type the text (`\n` presses Return) |
//! | `{"run": {"path": "...", "args": ["..."], "workingDir": "..."}}` | start a program (`.app` bundles via `open -a` on macOS) |
//! | `{"openUri": "https://..."}` | open a URL, file or folder |
//! | `"pastePlainText"` | paste the clipboard as plain text |
//!
//! **App scoping**: `"app"` (alias `"targetApp"`) is compared
//! case-insensitively with the frontmost app's bundle id, name and
//! executable name on macOS, and the foreground process's exe name on
//! Windows; `.exe`/`.app` suffixes and directories are ignored. App-specific
//! remaps take precedence over global ones for the same shortcut.
//!
//! A PowerToys `default.json` can be converted with
//! [`Config::from_powertoys_json`].
//!
//! # How it works
//!
//! [`engine::Engine`] is a platform-independent state machine (VK + down/up in,
//! pass / replace-with-actions out) that follows PowerToys'
//! `KeyboardEventHandlers.cpp`. Both backends subscribe to the shared input
//! hook ([`pm_hook`]) at [`pm_hook::priority::KEYBOARD_MANAGER`], ahead of
//! Mouse Without Borders and of every module that matches shortcuts, so
//! those see the remapped keys. On macOS (key down/up, flagsChanged, plus
//! mouse buttons so modifier flags stay consistent) it rewrites events in
//! place when one key becomes another and otherwise replaces them
//! (`Verdict::Replace`: the replacements go to the later subscribers and
//! are posted right after the tap), fixing the modifier flags of every
//! event that passes while a remap changes the modifier state. On Windows
//! it swallows the key and `SendInput`s the replacement. Injected events
//! are tagged ([`INJECTED_TAG`]) and skipped, as are Mouse Without Borders'
//! (`pm_input::INJECTED_TAG`) and the text expander's.
//!
//! macOS needs Accessibility permission for the process (or the terminal
//! that runs it); without it [`KeyboardManager::start`] fails cleanly.

use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};

pub mod clipboard;
pub mod config;
pub mod engine;
pub mod hyper;
pub mod keys;
pub mod launch;
mod powertoys;
pub mod windows_style;

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
        Err(anyhow!("keyboard-manager supports macOS and Windows only"))
    }
    pub fn inject(_: &[engine::Action]) -> Result<()> {
        Ok(())
    }
    pub fn send_paste() -> Result<()> {
        Err(anyhow!("unsupported platform"))
    }
    pub fn foreground_app() -> Vec<String> {
        Vec::new()
    }
    pub fn has_permission(_: bool) -> bool {
        false
    }
    pub fn focused_role() -> Option<String> {
        None
    }
}

pub use config::{Config, KeyRemap, PastePlainText, RunProgram, ShortcutRemap, Target};
pub use engine::{Action, Engine, EngineOptions, Outcome};
pub use hyper::{HyperKeyConfig, HyperMode, TapAction};
pub use keys::{Key, Shortcut};
pub use windows_style::WindowsStylePreset;

/// Tag stamped on every event Keyboard Manager injects (macOS
/// `kCGEventSourceUserData`, Windows `dwExtraInfo`): "PMKM".
pub const INJECTED_TAG: usize = pm_hook::tags::KEYBOARD_MANAGER;
/// Mouse Without Borders' tag; its injected (remote) input is not remapped.
pub const MWB_TAG: usize = pm_input::INJECTED_TAG;
/// Tag of the text expander's injected events ("PMTE"); not remapped
/// either (its unicode text events carry keycode 0, i.e. "A").
pub const TEXT_EXPANDER_TAG: usize = pm_hook::tags::TEXT_EXPANDER;

pub(crate) enum Job {
    Paste,
    Run(RunProgram),
    Open(String),
    /// A Caps Lock flagsChanged arrived while the Hyper key is on: a
    /// keyboard without the hidutil mapping (plugged in later).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    ReapplyHid,
}

/// The macOS Caps Lock → F18 hidutil session (nothing elsewhere).
#[cfg(target_os = "macos")]
type HidSession = hyper::session::Session;
#[cfg(not(target_os = "macos"))]
struct HidSession;

#[cfg(target_os = "macos")]
fn hid_session_start() -> Result<HidSession> {
    HidSession::start()
}
#[cfg(not(target_os = "macos"))]
fn hid_session_start() -> Result<HidSession> {
    Ok(HidSession)
}

/// Start / stop the hidutil mapping to match `cfg`.
fn sync_hid(slot: &Mutex<Option<HidSession>>, cfg: &Config) -> Result<()> {
    let Ok(mut g) = slot.lock() else {
        return Ok(());
    };
    match (cfg.hyper_key.is_on(), g.is_some()) {
        (true, false) => *g = Some(hid_session_start()?),
        (false, true) => *g = None, // Drop restores the previous mapping
        _ => {}
    }
    Ok(())
}

/// State shared between the handle, the hook thread and the worker.
pub(crate) struct Shared {
    pub(crate) engine: Mutex<Engine>,
    pub(crate) config: Mutex<Config>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    hid: Mutex<Option<HidSession>>,
}

impl Shared {
    /// Queue a side action (never blocks the hook).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn submit_job(&self, job: Job) {
        if let Ok(g) = self.jobs.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(job);
            }
        }
    }

    pub(crate) fn submit(&self, a: &Action) {
        let job = match a {
            Action::PastePlainText => Job::Paste,
            Action::Run(r) => Job::Run(r.clone()),
            Action::OpenUri(u) => Job::Open(u.clone()),
            _ => return,
        };
        if let Ok(g) = self.jobs.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(job);
            }
        }
    }
}

/// Whether the OS lets us install the hook (macOS: Accessibility trust).
/// With `prompt`, macOS shows its permission dialog.
pub fn has_permission(prompt: bool) -> bool {
    platform::has_permission(prompt)
}

/// Identifiers of the foreground app as used for app-specific remaps
/// (macOS: bundle id, name, executable; Windows: exe name).
pub fn foreground_app() -> Vec<String> {
    platform::foreground_app()
}

/// Synthesize the platform paste shortcut (Cmd+V on macOS, Ctrl+V on
/// Windows), tagged with [`INJECTED_TAG`] so Keyboard Manager doesn't remap
/// it. macOS needs Accessibility permission.
pub fn send_paste() -> Result<()> {
    platform::send_paste()
}

/// `AXRole` of the focused UI element of the frontmost app (macOS; `None`
/// elsewhere or when unknown). Used by the Windows-style preset's text-field
/// exemption.
pub fn focused_role() -> Option<String> {
    platform::focused_role()
}

/// Entry point.
pub struct KeyboardManager;

impl KeyboardManager {
    /// Install the keyboard hook and start remapping. Fails (without side
    /// effects) if the hook can't be installed, e.g. missing macOS
    /// Accessibility permission.
    pub fn start(config: Config) -> Result<Handle> {
        for w in config.validate() {
            log::warn!("keyboard-manager: {w}");
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let mut engine = Engine::new(&config, EngineOptions::default());
        engine.set_focus_probe(Arc::new(platform::focused_role));
        let shared = Arc::new(Shared {
            engine: Mutex::new(engine),
            config: Mutex::new(config),
            jobs: Mutex::new(Some(tx)),
            hid: Mutex::new(None),
        });
        let hook = platform::start(shared.clone())?;
        // Undo a Caps Lock mapping left behind by a crashed run.
        #[cfg(target_os = "macos")]
        hyper::session::recover();
        {
            let cfg = shared.config.lock().map(|c| c.clone()).unwrap_or_default();
            if let Err(e) = sync_hid(&shared.hid, &cfg) {
                log::warn!("keyboard-manager: hyper key unavailable: {e:#}");
            }
        }
        let s2 = shared.clone();
        let worker = std::thread::Builder::new()
            .name("km-actions".into())
            .spawn(move || {
                for job in rx {
                    let r = match job {
                        Job::Paste => {
                            let p = s2
                                .config
                                .lock()
                                .map(|c| c.paste_plain_text.clone())
                                .unwrap_or_default();
                            paste_plain_text_with(&p)
                        }
                        Job::Run(r) => launch::run_program(&r),
                        Job::Open(u) => launch::open_uri(&u),
                        Job::ReapplyHid => {
                            #[cfg(target_os = "macos")]
                            if let Ok(mut g) = s2.hid.lock() {
                                if let Some(h) = g.as_mut() {
                                    h.reapply();
                                }
                            }
                            Ok(())
                        }
                    };
                    if let Err(e) = r {
                        log::warn!("keyboard-manager: {e:#}");
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

/// A running Keyboard Manager. Dropping it stops remapping.
pub struct Handle {
    shared: Arc<Shared>,
    hook: Option<platform::Hook>,
    worker: Option<JoinHandle<()>>,
}

impl Handle {
    /// Replace the configuration. Keys held right now keep their old
    /// behaviour until released.
    pub fn update(&self, config: Config) -> Result<()> {
        for w in config.validate() {
            log::warn!("keyboard-manager: {w}");
        }
        self.shared
            .engine
            .lock()
            .map_err(|_| anyhow!("engine poisoned"))?
            .set_config(&config);
        let hid = sync_hid(&self.shared.hid, &config);
        *self
            .shared
            .config
            .lock()
            .map_err(|_| anyhow!("config poisoned"))? = config;
        hid.map_err(|e| anyhow!("hyper key: {e:#}"))
    }

    /// The current configuration.
    pub fn config(&self) -> Config {
        self.shared
            .config
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    /// Paste the clipboard as plain text now (works even if the hotkey is
    /// disabled). Blocks until done, including the optional restore delay.
    pub fn paste_plain_text(&self) -> Result<()> {
        let p = self.config().paste_plain_text;
        paste_plain_text_with(&p)
    }

    /// Stop remapping: remove the hook, release any keys we left pressed,
    /// and wait for pending actions.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(hook) = self.hook.take() else { return };
        drop(hook);
        let stuck = self
            .shared
            .engine
            .lock()
            .map(|mut e| e.reset())
            .unwrap_or_default();
        if !stuck.is_empty() {
            log::debug!("keyboard-manager: releasing {stuck:?} on stop");
            if let Err(e) = platform::inject(&stuck) {
                log::warn!("keyboard-manager: releasing keys on stop: {e:#}");
            }
        }
        if let Ok(mut g) = self.shared.jobs.lock() {
            g.take();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        // Put the previous Caps Lock mapping back (macOS).
        if let Ok(mut g) = self.shared.hid.lock() {
            g.take();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The paste-as-plain-text flow: snapshot (optional) -> plain text on the
/// clipboard -> synthesize Cmd+V / Ctrl+V -> restore (optional, only if
/// nobody else changed the clipboard meanwhile).
pub fn paste_plain_text_with(p: &PastePlainText) -> Result<()> {
    let snapshot = if p.restore_clipboard {
        match clipboard::Snapshot::capture() {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("keyboard-manager: clipboard snapshot failed: {e:#}");
                None
            }
        }
    } else {
        None
    };
    if clipboard::convert_to_plain_text()?.is_none() {
        log::info!("keyboard-manager: clipboard has no text; nothing to paste");
        return Ok(());
    }
    platform::send_paste()?;
    if let Some(s) = snapshot {
        let ours = clipboard::change_count();
        std::thread::sleep(Duration::from_millis(p.restore_delay_ms));
        if clipboard::change_count() == ours {
            s.restore()?;
        } else {
            log::info!("keyboard-manager: clipboard changed meanwhile; not restoring");
        }
    }
    Ok(())
}
