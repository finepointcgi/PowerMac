//! Current Space ("virtual desktop") per display.
//!
//! macOS has no public API for Spaces. Like most window managers we use the
//! private SkyLight/CoreGraphics functions `CGSMainConnectionID`,
//! `CGSManagedDisplayGetCurrentSpace` (per display, keyed by the display
//! UUID) and `CGSGetActiveSpace` (fallback when displays share Spaces or the
//! UUID is unknown). They are resolved with `dlsym` at runtime, so if a
//! future macOS removes them we simply report no Space ids and FancyZones
//! falls back to one layout per monitor.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;

use super::ffi::*;

type MainConnFn = unsafe extern "C" fn() -> i32;
type ActiveSpaceFn = unsafe extern "C" fn(i32) -> u64;
type DisplaySpaceFn = unsafe extern "C" fn(i32, CFStringRef) -> u64;
type DisplayUuidFn = unsafe extern "C" fn(u32) -> CFTypeRef;

struct Api {
    main_conn: MainConnFn,
    active_space: Option<ActiveSpaceFn>,
    display_space: Option<DisplaySpaceFn>,
    display_uuid: Option<DisplayUuidFn>,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let sym = |n: &std::ffi::CStr| -> *mut c_void { find_symbol(n) };
        let main = sym(c"CGSMainConnectionID");
        if main.is_null() {
            log::info!("fancyzones: CGSMainConnectionID unavailable; per-Space layouts disabled");
            return None;
        }
        let opt = |p: *mut c_void| (!p.is_null()).then_some(p);
        Some(Api {
            main_conn: std::mem::transmute::<*mut c_void, MainConnFn>(main),
            active_space: opt(sym(c"CGSGetActiveSpace")).map(|p| std::mem::transmute::<*mut c_void, ActiveSpaceFn>(p)),
            display_space: opt(sym(c"CGSManagedDisplayGetCurrentSpace"))
                .map(|p| std::mem::transmute::<*mut c_void, DisplaySpaceFn>(p)),
            display_uuid: opt(sym(c"CGDisplayCreateUUIDFromDisplayID"))
                .map(|p| std::mem::transmute::<*mut c_void, DisplayUuidFn>(p)),
        })
    })
    .as_ref()
}

/// Space id of a display (`None` if the private API is unavailable).
pub fn current_space(display: u32) -> Option<u64> {
    let api = api()?;
    unsafe {
        let cid = (api.main_conn)();
        if let (Some(uuid_fn), Some(space_fn)) = (api.display_uuid, api.display_space) {
            let uuid = uuid_fn(display);
            if !uuid.is_null() {
                let s = CFUUIDCreateString(std::ptr::null(), uuid);
                CFRelease(uuid);
                if !s.is_null() {
                    let id = space_fn(cid, s);
                    CFRelease(s);
                    if id != 0 {
                        return Some(id);
                    }
                }
            }
        }
        api.active_space.map(|f| f(cid)).filter(|&id| id != 0)
    }
}

/// `monitor id -> "space-<id>"` for the given (monitor id, display id) pairs.
pub fn spaces_for(displays: &[(String, u32)]) -> HashMap<String, String> {
    displays
        .iter()
        .filter_map(|(m, d)| current_space(*d).map(|s| (m.clone(), format!("space-{s}"))))
        .collect()
}
