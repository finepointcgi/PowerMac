//! The real [`Backend`] for [`crate::guard`]: the Dock's symbolic hotkeys
//! (private `CGSSetSymbolicHotKeyEnabled`, the call AltTab uses) and marker
//! files in `$TMPDIR`.
//!
//! The hotkey state is global to the login session and survives our
//! process, hence the markers, the start-up recovery and an `atexit`
//! handler for exits that skip `Drop`.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};

use super::ffi::*;
use crate::guard::{self, Backend, Marker};

type SetFn = unsafe extern "C" fn(i32, bool) -> i32;
type IsFn = unsafe extern "C" fn(i32) -> bool;

fn set_fn() -> Option<SetFn> {
    static F: OnceLock<Option<SetFn>> = OnceLock::new();
    *F.get_or_init(|| {
        let p = find_symbol(c"CGSSetSymbolicHotKeyEnabled");
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, SetFn>(p) })
    })
}

fn is_fn() -> Option<IsFn> {
    static F: OnceLock<Option<IsFn>> = OnceLock::new();
    *F.get_or_init(|| {
        let p = find_symbol(c"CGSIsSymbolicHotKeyEnabled");
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, IsFn>(p) })
    })
}

/// Whether the system's Cmd+Tab and Cmd+Shift+Tab are on (`None`: the
/// private API is missing).
pub fn system_switcher_enabled() -> Option<bool> {
    let f = is_fn()?;
    Some(guard::HOTKEYS_CMD_TAB.iter().all(|&i| unsafe { f(i) }))
}

/// Whether symbolic hotkey `id` is on.
pub fn hotkey_enabled(id: i32) -> Option<bool> {
    is_fn().map(|f| unsafe { f(id) })
}

fn set_hotkeys(ids: &[i32], enabled: bool) {
    let Some(f) = set_fn() else {
        log::warn!("window-switcher: CGSSetSymbolicHotKeyEnabled is missing; can't change the system switcher");
        return;
    };
    for &id in ids {
        let err = unsafe { f(id, enabled) };
        if err != 0 {
            log::warn!("window-switcher: CGSSetSymbolicHotKeyEnabled({id}, {enabled}) failed: {err}");
        }
    }
}

/// Secure Event Input (a password field has focus) hides keystrokes from
/// every event tap — the system switcher must stay usable then.
pub fn secure_input_on() -> bool {
    type F = unsafe extern "C" fn() -> u8;
    static P: OnceLock<Option<F>> = OnceLock::new();
    let f = *P.get_or_init(|| {
        let p = find_symbol(c"IsSecureEventInputEnabled");
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, F>(p) })
    });
    f.is_some_and(|f| unsafe { f() != 0 })
}

fn marker_dir() -> PathBuf {
    std::env::temp_dir()
}

fn own_marker() -> PathBuf {
    marker_dir().join(guard::marker_file_name(std::process::id()))
}

fn exe_of(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn atexit(cb: extern "C" fn()) -> i32;
}

fn alive(m: &Marker) -> bool {
    if m.pid == 0 || unsafe { kill(m.pid as i32, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() != Some(1) {
        return false; // ESRCH; EPERM (1) means it exists
    }
    // Same pid, same program (a reused pid isn't the owner).
    m.exe.is_empty() || exe_of(m.pid).is_none_or(|e| e == m.exe)
}

fn markers() -> Vec<(PathBuf, Marker)> {
    let Ok(rd) = std::fs::read_dir(marker_dir()) else { return vec![] };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(guard::MARKER_PREFIX) {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            Marker::parse(&name, &text).map(|m| (e.path(), m))
        })
        .collect()
}

/// Hotkeys this process keeps off (for the `atexit` handler).
static OFF: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

pub struct SysBackend;

impl Backend for SysBackend {
    fn set_hotkeys(&mut self, ids: &[i32], enabled: bool) {
        set_hotkeys(ids, enabled);
        let mut off = OFF.lock().unwrap_or_else(|e| e.into_inner());
        for &i in ids {
            if enabled {
                off.remove(&i);
            } else {
                off.insert(i);
            }
        }
        log::info!("window-switcher: system hotkeys {ids:?} {}", if enabled { "restored" } else { "taken over" });
    }

    fn write_marker(&mut self, ids: &BTreeSet<i32>) {
        let path = own_marker();
        if ids.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let pid = std::process::id();
        let m = Marker { pid, exe: exe_of(pid).unwrap_or_default(), hotkeys: ids.clone() };
        if let Err(e) = std::fs::write(&path, m.to_text()) {
            log::warn!("window-switcher: can't write {}: {e}", path.display());
        }
    }

    fn held_by_others(&mut self) -> BTreeSet<i32> {
        let me = std::process::id();
        markers().into_iter().filter(|(_, m)| m.pid != me && alive(m)).flat_map(|(_, m)| m.hotkeys).collect()
    }
}

/// Turn back on what a crashed run left off; delete its markers.
pub fn recover() {
    let plan = guard::plan_recovery(markers(), std::process::id(), alive);
    if !plan.reenable.is_empty() {
        let ids: Vec<i32> = plan.reenable.iter().copied().collect();
        log::warn!("window-switcher: a previous run left the system switcher off; turning {ids:?} back on");
        set_hotkeys(&ids, true);
    }
    for p in plan.stale {
        let _ = std::fs::remove_file(p);
    }
}

extern "C" fn at_exit() {
    let off: Vec<i32> = std::mem::take(&mut *OFF.lock().unwrap_or_else(|e| e.into_inner())).into_iter().collect();
    if off.is_empty() {
        return;
    }
    let others = SysBackend.held_by_others();
    let on: Vec<i32> = off.into_iter().filter(|i| !others.contains(i)).collect();
    set_hotkeys(&on, true);
    let _ = std::fs::remove_file(own_marker());
}

/// Restore the system switcher at `exit()` too (e.g. NSApp terminate).
pub fn install_exit_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        atexit(at_exit);
    });
}
