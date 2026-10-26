//! Helper windows that run as separate processes of this executable (their
//! event loops can't share the tray app's main loop): the settings window,
//! the FancyZones layout editor, the clipboard history picker and the color
//! picker editor. Each slot holds at most one process; asking for it again
//! while it runs brings it to the front instead of starting a second one.

use std::process::Child;
use std::sync::Mutex;

/// One single-instance child process.
pub struct Slot(Mutex<Option<Child>>);

impl Slot {
    pub const fn new() -> Self {
        Slot(Mutex::new(None))
    }
}

/// `powermac settings`
pub static SETTINGS: Slot = Slot::new();
/// `powermac editor` (FancyZones layout editor, from the tray)
pub static EDITOR: Slot = Slot::new();
/// `powermac clipboard` (clipboard history picker)
pub static CLIPBOARD: Slot = Slot::new();
/// `powermac color-picker --editor`
pub static COLOR_EDITOR: Slot = Slot::new();

/// Start `<this exe> --config <config> <args…>` in `slot`, or bring the
/// process already running there to the front. Callable from any thread.
pub fn open(slot: &Slot, args: &[&str]) {
    open_checked(slot, args);
}

/// [`open`], returning true when the process was already running (it was
/// brought to the front and keeps its own arguments).
pub fn open_checked(slot: &Slot, args: &[&str]) -> bool {
    let what = args.join(" ");
    let mut child = slot.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = child.as_mut() {
        if let Ok(None) = c.try_wait() {
            log::info!("{what}: already open (pid {})", c.id());
            activate_process(c.id());
            return true;
        }
    }
    let Some(path) = crate::CONFIG_PATH.get().cloned().or_else(pm_config::AppConfig::default_path) else {
        return false;
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            log::warn!("{what}: {e}");
            return false;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--config").arg(&path).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.spawn() {
        Ok(c) => {
            log::info!("{what}: started (pid {})", c.id());
            *child = Some(c);
        }
        Err(e) => log::warn!("{what}: could not start: {e}"),
    }
    false
}

#[cfg(target_os = "macos")]
fn activate_process(pid: u32) {
    fn activate(pid: u32) {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) {
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
    }
    if objc2::MainThreadMarker::new().is_some() {
        activate(pid);
    } else {
        dispatch2::DispatchQueue::main().exec_async(move || activate(pid));
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_process(_pid: u32) {}
