//! The Notch Shelf's Quick Controls: Awake on/off and one-shot actions
//! (Pick color, Extract text, Clipboard history, Window switcher, Lock
//! screen, Show desktop).
//!
//! The Sound widget gets PowerMac's audio control
//! ([`crate::audio_glue::notch_sound`]).
//!
//! The shelf calls these on the main thread. The config watcher may hold
//! `RUNNING` while it waits for the main thread (e.g. to register a Carbon
//! hotkey), so nothing here waits for `RUNNING` on the main thread: `get`
//! only tries the lock (falling back to the last known value) and every
//! action runs on its own thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use notch_shelf::{QuickAction, QuickToggle, StartOptions};

use crate::RUNNING;

/// Last known Awake state (for when `RUNNING` is busy).
static AWAKE_ON: AtomicBool = AtomicBool::new(false);

fn awake_active() -> bool {
    if let Ok(g) = RUNNING.try_lock() {
        let on = g.as_ref().and_then(|r| r.awake.as_ref()).is_some_and(|a| a.status().active);
        AWAKE_ON.store(on, Ordering::Relaxed);
    }
    AWAKE_ON.load(Ordering::Relaxed)
}

fn set_awake(on: bool) {
    AWAKE_ON.store(on, Ordering::Relaxed);
    std::thread::spawn(move || {
        let display = crate::current_config().awake.settings.keep_display_on;
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(a) = guard.as_ref().and_then(|r| r.awake.as_ref()) else { return };
        if on {
            let cfg = awake::AwakeConfig { mode: awake::AwakeMode::Indefinite, keep_display_on: display, bound_pid: None };
            match a.set(cfg) {
                Ok(()) => log::info!("awake: on (notch)"),
                Err(e) => log::warn!("awake: {e}"),
            }
        } else {
            a.stop();
            log::info!("awake: off (notch)");
        }
    });
}

/// Run `f` with the running modules on another thread, after `delay` (lets
/// the panel close before a loupe or overlay appears).
fn later(delay_ms: u64, f: impl FnOnce(&crate::Running) + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = guard.as_ref() {
            f(r);
        }
    });
}

fn run_detached(cmd: &str, args: &[&str]) {
    match std::process::Command::new(cmd).args(args).spawn() {
        Ok(mut c) => {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        Err(e) => log::warn!("{cmd}: {e}"),
    }
}

/// Lock the screen (macOS: put the displays to sleep, which locks when
/// "Require password after the display is turned off" is immediate, as it
/// is by default).
pub(crate) fn lock_screen() {
    #[cfg(target_os = "macos")]
    run_detached("/usr/bin/pmset", &["displaysleepnow"]);
    #[cfg(windows)]
    run_detached("rundll32.exe", &["user32.dll,LockWorkStation"]);
}

/// Show the desktop (macOS: Mission Control's "Show Desktop").
pub(crate) fn show_desktop() {
    #[cfg(target_os = "macos")]
    {
        let exe = ["/System/Applications/Mission Control.app/Contents/MacOS/Mission Control",
                   "/Applications/Mission Control.app/Contents/MacOS/Mission Control"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists());
        match exe {
            Some(p) => run_detached(p, &["1"]),
            None => log::warn!("show desktop: Mission Control not found"),
        }
    }
    #[cfg(windows)]
    run_detached("explorer.exe", &["shell:::{3080F90D-D7AD-11D9-BD98-0000F87A1CE6}"]);
}

/// The controls handed to the Notch Shelf.
pub fn notch_controls() -> StartOptions {
    StartOptions {
        toggles: vec![QuickToggle::new("awake", "Awake", "cup.and.saucer", awake_active, set_awake)],
        actions: vec![
            QuickAction::new("pick-color", "Pick color", "eyedropper", || {
                later(250, |r| {
                    if let Some(c) = &r.color {
                        c.pick_now();
                    }
                })
            }),
            QuickAction::new("extract-text", "Extract text", "text.viewfinder", || {
                later(250, |r| {
                    if let Some(o) = &r.ocr {
                        o.capture_now();
                    }
                })
            }),
            QuickAction::new("clipboard", "Clipboard", "doc.on.clipboard", || {
                later(100, |r| match &r.clipboard {
                    Some(c) => {
                        c.open_picker();
                    }
                    None => log::info!("clipboard history is off"),
                })
            }),
            QuickAction::new("switcher", "Windows", "rectangle.stack", || {
                later(200, |r| {
                    if let Some(s) = &r.switcher {
                        s.show();
                    }
                })
            }),
            QuickAction::new("lock", "Lock screen", "lock", || {
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(250));
                    lock_screen();
                });
            }),
            QuickAction::new("desktop", "Show desktop", "menubar.dock.rectangle", || {
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(250));
                    show_desktop();
                });
            }),
        ],
        sound: Some(crate::audio_glue::notch_sound()),
    }
}
