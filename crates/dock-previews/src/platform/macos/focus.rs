//! Focusing windows and the close / minimize actions (adapted from the
//! window switcher).
//!
//! Focus follows what AltTab does:
//! 1. unhide the app / un-minimize the window if needed,
//! 2. make the app frontmost *with this window* through SkyLight's
//!    `_SLPSSetFrontProcessWithOptions` + a synthetic "make key window"
//!    event record (this also switches to the window's Space), falling back
//!    to `NSRunningApplication.activate` when the private calls are missing,
//! 3. `AXMain = true` + `AXRaise` so the window ends up on top of its app's
//!    other windows.

use std::ffi::c_void;
use std::sync::OnceLock;

use super::apps;
use super::ax::AxElement;
use super::ffi::*;
use super::windows::AppWindow;

type SetFrontFn = unsafe extern "C" fn(*const ProcessSerialNumber, u32, u32) -> i32;
type PostEventRecordFn = unsafe extern "C" fn(*const ProcessSerialNumber, *const u8) -> i32;

struct SlpsApi {
    set_front: SetFrontFn,
    post_event_record: PostEventRecordFn,
}

fn slps() -> Option<&'static SlpsApi> {
    static API: OnceLock<Option<SlpsApi>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let f = find_symbol(c"_SLPSSetFrontProcessWithOptions");
        let p = find_symbol(c"SLPSPostEventRecordTo");
        if f.is_null() || p.is_null() {
            log::info!("dock-previews: SkyLight focus functions unavailable; using NSRunningApplication");
            return None;
        }
        Some(SlpsApi {
            set_front: std::mem::transmute::<*mut c_void, SetFrontFn>(f),
            post_event_record: std::mem::transmute::<*mut c_void, PostEventRecordFn>(p),
        })
    })
    .as_ref()
}

/// `kCPSUserGenerated`
const SLPS_USER_GENERATED: u32 = 0x200;

/// Make `wid` the key window of `psn`'s app (AltTab's `makeKeyWindow`).
unsafe fn make_key_window(api: &SlpsApi, psn: &ProcessSerialNumber, wid: u32) {
    let mut bytes = [0u8; 0xf8];
    bytes[0x04] = 0xf8;
    bytes[0x3a] = 0x10;
    bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
    for b in &mut bytes[0x20..0x30] {
        *b = 0xff;
    }
    bytes[0x08] = 0x01;
    (api.post_event_record)(psn, bytes.as_ptr());
    bytes[0x08] = 0x02;
    (api.post_event_record)(psn, bytes.as_ptr());
}

fn bring_front(pid: i32, wid: u32) {
    if let Some(api) = slps() {
        let mut psn = ProcessSerialNumber::default();
        if unsafe { GetProcessForPID(pid, &mut psn) } == 0 {
            unsafe {
                (api.set_front)(&psn, wid, SLPS_USER_GENERATED);
                make_key_window(api, &psn, wid);
            }
            return;
        }
    }
    apps::activate(pid);
}

/// Bring `w` to the front (any thread). `el` is its AX element if known.
pub fn focus(w: &AppWindow, el: Option<&AxElement>) {
    let pid = w.pid;
    if w.app_hidden || apps::is_hidden(pid) {
        apps::set_hidden(pid, false);
    }
    if let Some(el) = el {
        if el.bool_attr("AXMinimized").unwrap_or(false) {
            let _ = el.set_bool_attr("AXMinimized", false);
        }
    }
    bring_front(pid, w.wid);
    if let Some(el) = el {
        let _ = el.set_bool_attr("AXMain", true);
        if let Err(e) = el.perform("AXRaise") {
            log::debug!("dock-previews: raise failed: {e}");
        }
    } else {
        apps::activate(pid);
    }
}

/// Window actions from the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
    ToggleMinimize,
    Minimize,
}

/// Perform a window action (any thread).
pub fn act(action: Action, el: Option<&AxElement>) {
    let Some(el) = el else { return };
    match action {
        Action::Close => match el.element_attr("AXCloseButton") {
            Some(b) => {
                let _ = b.perform("AXPress");
            }
            None => log::debug!("dock-previews: window has no close button"),
        },
        Action::ToggleMinimize => {
            let m = el.bool_attr("AXMinimized").unwrap_or(false);
            let _ = el.set_bool_attr("AXMinimized", !m);
        }
        Action::Minimize => {
            let _ = el.set_bool_attr("AXMinimized", true);
        }
    }
}
