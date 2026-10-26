//! The windows of one app: CGWindowList (every window, any Space,
//! z-order) + Accessibility (titles, subroles, minimized state, element
//! handles) + the private CGS Space functions, adapted from the window
//! switcher's enumeration.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::ax::{self, AxElement};
use super::ffi::*;
use crate::geom::RectF;

/// One window of the hovered app.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppWindow {
    pub wid: u32,
    pub pid: i32,
    pub title: String,
    /// Last known frame (global, top-left origin).
    pub frame: RectF,
    pub minimized: bool,
    pub other_space: bool,
    pub fullscreen: bool,
    pub app_hidden: bool,
}

impl AppWindow {
    /// Width / height (for the tile), or 1.6 when unknown.
    pub fn aspect(&self) -> f64 {
        if self.frame.w > 1.0 && self.frame.h > 1.0 {
            self.frame.w / self.frame.h
        } else {
            1.6
        }
    }
}

/// One CGWindowList entry.
#[derive(Debug, Clone)]
struct CgWindow {
    wid: u32,
    pid: i32,
    bounds: RectF,
    onscreen: bool,
    name: Option<String>,
}

fn cg_windows() -> Vec<CgWindow> {
    let mut out = Vec::new();
    unsafe {
        let arr = CGWindowListCopyWindowInfo(kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements, kCGNullWindowID);
        let Some(_owned) = CfOwned::new(arr) else { return out };
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            if d.is_null() || CFGetTypeID(d) != CFDictionaryGetTypeID() {
                continue;
            }
            if dict_i64(d, "kCGWindowLayer") != Some(0) {
                continue;
            }
            let (Some(wid), Some(pid)) = (dict_i64(d, "kCGWindowNumber"), dict_i64(d, "kCGWindowOwnerPID")) else { continue };
            let mut r = CGRect::default();
            let b = dict_get(d, "kCGWindowBounds");
            if b.is_null() || !CGRectMakeWithDictionaryRepresentation(b, &mut r) {
                continue;
            }
            out.push(CgWindow {
                wid: wid as u32,
                pid: pid as i32,
                bounds: RectF::new(r.origin.x, r.origin.y, r.size.width, r.size.height),
                onscreen: dict_bool(d, "kCGWindowIsOnscreen").unwrap_or(false),
                name: dict_string(d, "kCGWindowName").filter(|s| !s.is_empty()),
            });
        }
    }
    out
}


// ------------------------------------------------------------------ Spaces

type MainConnFn = unsafe extern "C" fn() -> i32;
type CopySpacesForWindowsFn = unsafe extern "C" fn(i32, i32, CFArrayRef) -> CFArrayRef;
type CopyManagedDisplaySpacesFn = unsafe extern "C" fn(i32) -> CFArrayRef;

struct SpacesApi {
    main_conn: MainConnFn,
    spaces_for_windows: CopySpacesForWindowsFn,
    managed_display_spaces: CopyManagedDisplaySpacesFn,
}

fn spaces_api() -> Option<&'static SpacesApi> {
    static API: OnceLock<Option<SpacesApi>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let main = find_symbol(c"CGSMainConnectionID");
        let sfw = find_symbol(c"CGSCopySpacesForWindows");
        let mds = find_symbol(c"CGSCopyManagedDisplaySpaces");
        if main.is_null() || sfw.is_null() || mds.is_null() {
            log::info!("dock-previews: CGS Space functions unavailable");
            return None;
        }
        Some(SpacesApi {
            main_conn: std::mem::transmute::<*mut c_void, MainConnFn>(main),
            spaces_for_windows: std::mem::transmute::<*mut c_void, CopySpacesForWindowsFn>(sfw),
            managed_display_spaces: std::mem::transmute::<*mut c_void, CopyManagedDisplaySpacesFn>(mds),
        })
    })
    .as_ref()
}

unsafe fn number_array(values: &[u32]) -> CfOwned {
    let nums: Vec<CFTypeRef> = values
        .iter()
        .map(|&v| {
            let v = v as i64;
            CFNumberCreate(std::ptr::null(), kCFNumberSInt64Type, &v as *const i64 as *const c_void)
        })
        .collect();
    let arr = CFArrayCreate(std::ptr::null(), nums.as_ptr(), nums.len() as isize, &kCFTypeArrayCallBacks);
    for n in nums {
        CFRelease(n);
    }
    CfOwned(arr)
}

unsafe fn i64_array(arr: CFArrayRef) -> Vec<i64> {
    let mut out = Vec::new();
    if arr.is_null() || CFGetTypeID(arr) != CFArrayGetTypeID() {
        return out;
    }
    for i in 0..CFArrayGetCount(arr) {
        let v = CFArrayGetValueAtIndex(arr, i);
        let mut n: i64 = 0;
        if !v.is_null() && CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue(v, kCFNumberSInt64Type, &mut n as *mut i64 as *mut c_void) != 0 {
            out.push(n);
        }
    }
    out
}

/// The Spaces currently visible (one per display).
fn visible_spaces(api: &SpacesApi) -> HashSet<i64> {
    let mut out = HashSet::new();
    unsafe {
        let arr = (api.managed_display_spaces)((api.main_conn)());
        let Some(_o) = CfOwned::new(arr) else { return out };
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            if d.is_null() || CFGetTypeID(d) != CFDictionaryGetTypeID() {
                continue;
            }
            let cur = dict_get(d, "Current Space");
            if cur.is_null() || CFGetTypeID(cur) != CFDictionaryGetTypeID() {
                continue;
            }
            if let Some(id) = dict_i64(cur, "id64").or_else(|| dict_i64(cur, "ManagedSpaceID")) {
                out.insert(id);
            }
        }
    }
    out
}

/// Of `wids`, the ones that live on a Space that isn't visible. `None` if
/// the private API is unavailable.
fn on_hidden_spaces(wids: &[u32]) -> Option<HashSet<u32>> {
    let api = spaces_api()?;
    let visible = visible_spaces(api);
    if visible.is_empty() {
        return None;
    }
    let mut out = HashSet::new();
    unsafe {
        let cid = (api.main_conn)();
        for &w in wids {
            let arr = number_array(&[w]);
            // mask 7 = all Spaces (current, other, user + fullscreen)
            let spaces = (api.spaces_for_windows)(cid, 7, arr.0);
            let Some(_o) = CfOwned::new(spaces) else { continue };
            let ids = i64_array(spaces);
            if !ids.is_empty() && !ids.iter().any(|s| visible.contains(s)) {
                out.insert(w);
            }
        }
    }
    Some(out)
}

// -------------------------------------------------------------- enumeration

/// AX elements by window id, kept between enumerations.
#[derive(Default)]
pub struct Elements {
    pub by_wid: HashMap<u32, AxElement>,
    failed: HashSet<u32>,
    last_probe: HashMap<i32, Instant>,
}

impl Elements {
    pub fn get(&self, wid: u32) -> Option<AxElement> {
        self.by_wid.get(&wid).cloned()
    }
}

fn is_listed_subrole(s: Option<&str>) -> bool {
    matches!(s, Some("AXStandardWindow") | Some("AXDialog"))
}

const MIN_W: f64 = 60.0;
const MIN_H: f64 = 40.0;

fn build(cg: &CgWindow, w: &AxElement, pid: i32, app_hidden: bool) -> Option<AppWindow> {
    let minimized = w.bool_attr("AXMinimized").unwrap_or(false);
    if !minimized && (cg.bounds.w < MIN_W || cg.bounds.h < MIN_H) {
        return None;
    }
    let title = w.string_attr("AXTitle").filter(|t| !t.is_empty()).or_else(|| cg.name.clone()).unwrap_or_default();
    Some(AppWindow {
        wid: cg.wid,
        pid,
        title,
        frame: cg.bounds,
        minimized,
        other_space: !cg.onscreen && !minimized && !app_hidden,
        fullscreen: w.bool_attr("AXFullScreen").unwrap_or(false),
        app_hidden,
    })
}

/// The windows of `pid` in z-order (front to back; windows that aren't on
/// screen follow), including minimized windows and (when `other_spaces`)
/// windows on other Spaces.
pub fn app_windows(el: &mut Elements, pid: i32, app_hidden: bool, other_spaces: bool) -> Vec<AppWindow> {
    if !ax::is_trusted() {
        return vec![];
    }
    let cg: Vec<CgWindow> = cg_windows().into_iter().filter(|w| w.pid == pid).collect();
    let cg_index: HashMap<u32, usize> = cg.iter().enumerate().map(|(i, w)| (w.wid, i)).collect();
    let mut found: Vec<(usize, AppWindow)> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    if cg.is_empty() {
        return vec![];
    }
    let axapp = AxElement::application(pid);
    for w in axapp.elements_attr("AXWindows") {
        let Some(wid) = w.window_id() else { continue };
        if w.role().as_deref() != Some("AXWindow") || !is_listed_subrole(w.subrole().as_deref()) {
            continue;
        }
        let Some(&ci) = cg_index.get(&wid) else { continue };
        if let Some(info) = build(&cg[ci], &w, pid, app_hidden) {
            seen.insert(wid);
            el.by_wid.insert(wid, w);
            found.push((ci, info));
        }
    }
    if other_spaces && !app_hidden {
        let cands: Vec<u32> = cg
            .iter()
            .filter(|c| !c.onscreen && !seen.contains(&c.wid) && c.bounds.w >= MIN_W && c.bounds.h >= MIN_H)
            .map(|c| c.wid)
            .collect();
        if !cands.is_empty() {
            let hidden_space = on_hidden_spaces(&cands);
            let cands: Vec<u32> = cands.into_iter().filter(|w| hidden_space.as_ref().is_none_or(|h| h.contains(w))).collect();
            let unknown: Vec<u32> = cands.iter().copied().filter(|w| !el.by_wid.contains_key(w) && !el.failed.contains(w)).collect();
            if !unknown.is_empty() {
                let recent = el.last_probe.get(&pid).is_some_and(|t| t.elapsed() < Duration::from_secs(3));
                if !recent {
                    el.last_probe.insert(pid, Instant::now());
                    let got = ax::windows_by_remote_token(pid, &unknown, 1000);
                    for w in &unknown {
                        if !got.iter().any(|(g, _)| g == w) {
                            el.failed.insert(*w);
                        }
                    }
                    for (wid, e) in got {
                        el.by_wid.insert(wid, e);
                    }
                }
            }
            for wid in cands {
                let Some(w) = el.by_wid.get(&wid).cloned() else { continue };
                if !is_listed_subrole(w.subrole().as_deref()) {
                    continue;
                }
                let Some(&ci) = cg_index.get(&wid) else { continue };
                if let Some(mut info) = build(&cg[ci], &w, pid, app_hidden) {
                    info.other_space = !info.minimized;
                    seen.insert(wid);
                    found.push((ci, info));
                }
            }
        }
    }
    // Forget this app's windows that no longer exist (other apps' entries
    // are pruned when they're listed).
    let alive: HashSet<u32> = cg.iter().map(|w| w.wid).collect();
    el.by_wid.retain(|w, e| alive.contains(w) || e.pid() != Some(pid));
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().map(|(_, w)| w).collect()
}

/// Window id of `pid`'s focused window.
pub fn focused_window(pid: i32) -> Option<u32> {
    let app = AxElement::application(pid);
    let w = app.element_attr("AXFocusedWindow").or_else(|| app.element_attr("AXMainWindow"))?;
    w.window_id()
}
