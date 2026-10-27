//! Spaces through the private CGS functions (resolved with `dlsym`; when a
//! macOS release removes them, Spaces are simply not recorded / restored).
//!
//! * `CGSCopyManagedDisplaySpaces` lists each display's Spaces in Mission
//!   Control order; a window's Space index is its position among the
//!   display's desktop (type 0) Spaces, 1-based.
//! * `CGSCopySpacesForWindows` gives the Spaces a window is on.
//! * `CGSMoveWindowsToManagedSpace` moves windows of other apps; recent
//!   macOS versions ignore it for other processes' windows unless SIP is
//!   relaxed, so restoring Spaces is opt-in and verified.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::OnceLock;

use super::ffi::*;

type MainConnFn = unsafe extern "C" fn() -> i32;
type CopySpacesForWindowsFn = unsafe extern "C" fn(i32, i32, CFArrayRef) -> CFArrayRef;
type CopyManagedDisplaySpacesFn = unsafe extern "C" fn(i32) -> CFArrayRef;
type MoveWindowsFn = unsafe extern "C" fn(i32, CFArrayRef, u64);
type DisplayUuidFn = unsafe extern "C" fn(u32) -> CFTypeRef;

struct Api {
    main_conn: MainConnFn,
    spaces_for_windows: CopySpacesForWindowsFn,
    managed_display_spaces: CopyManagedDisplaySpacesFn,
    move_windows: Option<MoveWindowsFn>,
    display_uuid: Option<DisplayUuidFn>,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let main = find_symbol(c"CGSMainConnectionID");
        let sfw = find_symbol(c"CGSCopySpacesForWindows");
        let mds = find_symbol(c"CGSCopyManagedDisplaySpaces");
        if main.is_null() || sfw.is_null() || mds.is_null() {
            log::info!("workspaces: CGS Space functions unavailable");
            return None;
        }
        let mv = find_symbol(c"CGSMoveWindowsToManagedSpace");
        let uuid = find_symbol(c"CGDisplayCreateUUIDFromDisplayID");
        Some(Api {
            main_conn: std::mem::transmute::<*mut c_void, MainConnFn>(main),
            spaces_for_windows: std::mem::transmute::<*mut c_void, CopySpacesForWindowsFn>(sfw),
            managed_display_spaces: std::mem::transmute::<*mut c_void, CopyManagedDisplaySpacesFn>(mds),
            move_windows: (!mv.is_null()).then(|| std::mem::transmute::<*mut c_void, MoveWindowsFn>(mv)),
            display_uuid: (!uuid.is_null()).then(|| std::mem::transmute::<*mut c_void, DisplayUuidFn>(uuid)),
        })
    })
    .as_ref()
}

/// One display's Spaces.
#[derive(Debug, Clone, Default)]
pub struct DisplaySpaces {
    /// Display UUID, or "Main" when displays share Spaces.
    pub identifier: String,
    pub current: i64,
    /// Desktop Spaces (type 0), Mission Control order.
    pub desktops: Vec<i64>,
}

pub fn layout() -> Vec<DisplaySpaces> {
    let Some(api) = api() else { return vec![] };
    let mut out = Vec::new();
    unsafe {
        let arr = (api.managed_display_spaces)((api.main_conn)());
        let Some(_o) = CfOwned::new(arr) else { return out };
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            if d.is_null() || CFGetTypeID(d) != CFDictionaryGetTypeID() {
                continue;
            }
            let mut ds = DisplaySpaces { identifier: dict_string(d, "Display Identifier").unwrap_or_default(), ..Default::default() };
            let cur = dict_get(d, "Current Space");
            if !cur.is_null() && CFGetTypeID(cur) == CFDictionaryGetTypeID() {
                ds.current = dict_i64(cur, "id64").or_else(|| dict_i64(cur, "ManagedSpaceID")).unwrap_or(0);
            }
            let spaces = dict_get(d, "Spaces");
            if !spaces.is_null() && CFGetTypeID(spaces) == CFArrayGetTypeID() {
                for j in 0..CFArrayGetCount(spaces) {
                    let s = CFArrayGetValueAtIndex(spaces, j);
                    if s.is_null() || CFGetTypeID(s) != CFDictionaryGetTypeID() {
                        continue;
                    }
                    let ty = dict_i64(s, "type").unwrap_or(0);
                    if ty != 0 {
                        continue;
                    }
                    if let Some(id) = dict_i64(s, "id64").or_else(|| dict_i64(s, "ManagedSpaceID")) {
                        ds.desktops.push(id);
                    }
                }
            }
            out.push(ds);
        }
    }
    out
}

fn spaces_of(api: &Api, wid: u32) -> Vec<i64> {
    unsafe {
        let arr = number_array(&[wid as u64]);
        // mask 7 = current + other + user Spaces
        let spaces = (api.spaces_for_windows)((api.main_conn)(), 7, arr.0);
        let Some(_o) = CfOwned::new(spaces) else { return vec![] };
        i64_array(spaces)
    }
}

/// 1-based desktop index of each window (windows on one Space only).
pub fn space_indexes(wids: &[u32]) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    let Some(api) = api() else { return out };
    let lay = layout();
    let mut index: HashMap<i64, u32> = HashMap::new();
    for d in &lay {
        for (i, s) in d.desktops.iter().enumerate() {
            index.insert(*s, i as u32 + 1);
        }
    }
    for &w in wids {
        let s = spaces_of(api, w);
        if s.len() == 1 {
            if let Some(i) = index.get(&s[0]) {
                out.insert(w, *i);
            }
        }
    }
    out
}

/// Of `wids`, those that live only on Spaces that aren't visible. `None`
/// if the private API is unavailable.
pub fn on_hidden_spaces(wids: &[u32]) -> Option<HashSet<u32>> {
    let api = api()?;
    let visible: HashSet<i64> = layout().iter().map(|d| d.current).filter(|c| *c != 0).collect();
    if visible.is_empty() {
        return None;
    }
    let mut out = HashSet::new();
    for &w in wids {
        let ids = spaces_of(api, w);
        if !ids.is_empty() && !ids.iter().any(|s| visible.contains(s)) {
            out.insert(w);
        }
    }
    Some(out)
}

fn display_uuid(api: &Api, display: u32) -> Option<String> {
    let f = api.display_uuid?;
    unsafe {
        let uuid = f(display);
        if uuid.is_null() {
            return None;
        }
        let s = CFUUIDCreateString(std::ptr::null(), uuid);
        CFRelease(uuid);
        let out = cfstring_to_string(s);
        if !s.is_null() {
            CFRelease(s);
        }
        out
    }
}

/// Move `wid` to desktop `index` (1-based) of `display`. Returns whether
/// the window ended up there.
pub fn move_to_space(wid: u32, display: Option<u32>, index: u32) -> bool {
    let Some(api) = api() else { return false };
    let Some(mv) = api.move_windows else { return false };
    let lay = layout();
    let uuid = display.and_then(|d| display_uuid(api, d));
    let ds = lay
        .iter()
        .find(|d| uuid.as_deref().is_some_and(|u| d.identifier.eq_ignore_ascii_case(u)))
        .or_else(|| (lay.len() == 1).then(|| &lay[0]));
    let Some(target) = ds.and_then(|d| d.desktops.get(index.saturating_sub(1) as usize).copied()) else { return false };
    if spaces_of(api, wid) == vec![target] {
        return true;
    }
    unsafe {
        let arr = number_array(&[wid as u64]);
        mv((api.main_conn)(), arr.0, target as u64);
    }
    spaces_of(api, wid).contains(&target)
}
