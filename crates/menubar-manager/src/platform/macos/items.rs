//! Other apps' status items (any thread): `CGWindowListCopyWindowInfo`
//! (status windows at `kCGStatusWindowLevel`, macOS 15 and older) or the
//! Accessibility API (`AXExtrasMenuBar`, macOS 26+ where status items are no
//! longer windows), plus the synthesized Cmd-drag that moves an item.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_app_kit::NSRunningApplication;

use super::ffi::*;
use crate::{Rect, RawItem};

const SYSTEM_OWNERS: &[&str] = &["Control Center", "ControlCenter", "SystemUIServer", "MenuBarAgent", "TextInputMenuAgent", "Spotlight", "Siri", "Window Server"];

fn is_system(owner: &str, bundle: Option<&str>) -> bool {
    SYSTEM_OWNERS.contains(&owner) || bundle.is_some_and(|b| b.starts_with("com.apple."))
}

fn app_info(pid: i32) -> (Option<String>, Option<String>) {
    match NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        Some(a) => (a.localizedName().map(|s| s.to_string()), a.bundleIdentifier().map(|s| s.to_string())),
        None => (None, None),
    }
}

pub fn own_pid() -> i32 {
    std::process::id() as i32
}

/// Every other process's status items, unsorted.
pub fn list_other_items() -> Vec<RawItem> {
    let v = list_cg();
    if !v.is_empty() {
        return v;
    }
    if unsafe { AXIsProcessTrusted() } {
        return list_ax();
    }
    vec![]
}

/// Status item windows (macOS 15 and older).
fn list_cg() -> Vec<RawItem> {
    let me = own_pid();
    let Some(arr) = Owned::new(unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionAll, 0) }) else { return vec![] };
    let n = unsafe { CFArrayGetCount(arr.0) };
    let mut out = vec![];
    let mut names: HashMap<i32, (Option<String>, Option<String>)> = HashMap::new();
    for i in 0..n {
        let d = unsafe { CFArrayGetValueAtIndex(arr.0, i) };
        if d.is_null() || dict_i64(d, unsafe { kCGWindowLayer }) != Some(kCGStatusWindowLevel) {
            continue;
        }
        let pid = dict_i64(d, unsafe { kCGWindowOwnerPID }).unwrap_or(0) as i32;
        if pid == me || pid == 0 {
            continue;
        }
        let mut r = CGRect::default();
        let b = dict_get(d, unsafe { kCGWindowBounds });
        if b.is_null() || !unsafe { CGRectMakeWithDictionaryRepresentation(b, &mut r) } {
            continue;
        }
        // Status items are menu-bar tall and not very wide.
        if r.size.height > 60.0 || r.size.height < 8.0 || r.size.width > 800.0 {
            continue;
        }
        let owner = cf_to_string(dict_get(d, unsafe { kCGWindowOwnerName })).unwrap_or_default();
        let title = cf_to_string(dict_get(d, unsafe { kCGWindowName })).filter(|s| !s.is_empty());
        let wid = dict_i64(d, unsafe { kCGWindowNumber }).map(|w| w as u32);
        let (_, bundle) = names.entry(pid).or_insert_with(|| app_info(pid)).clone();
        out.push(RawItem {
            system: is_system(&owner, bundle.as_deref()),
            owner,
            bundle_id: bundle,
            pid,
            label: title.clone(),
            title,
            index: 0,
            frame: Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height),
            window_id: wid,
            own: false,
        });
    }
    number_by_owner(&mut out);
    out
}

fn number_by_owner(v: &mut [RawItem]) {
    v.sort_by(|a, b| a.frame.x.total_cmp(&b.frame.x));
    let mut seen: HashMap<i32, usize> = HashMap::new();
    for it in v.iter_mut() {
        let n = seen.entry(it.pid).or_insert(0);
        it.index = *n;
        *n += 1;
    }
}

fn running_pids() -> Vec<i32> {
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: Retained<AnyObject> = msg_send![&*ws, runningApplications];
        let n: usize = msg_send![&*apps, count];
        (0..n)
            .map(|i| {
                let a: *mut AnyObject = msg_send![&*apps, objectAtIndex: i];
                let pid: i32 = msg_send![a, processIdentifier];
                pid
            })
            .collect()
    }
}

/// `AXExtrasMenuBar` of every app (macOS 26+; needs Accessibility). Asks
/// the apps in parallel: there are hundreds of processes and each answer
/// takes a few milliseconds.
fn list_ax() -> Vec<RawItem> {
    let me = own_pid();
    let pids: Vec<i32> = running_pids().into_iter().filter(|&p| p != me && p > 0).collect();
    let chunks = 12usize;
    let per = pids.len().div_ceil(chunks).max(1);
    let mut out: Vec<RawItem> = std::thread::scope(|s| {
        let handles: Vec<_> = pids.chunks(per).map(|c| s.spawn(move || c.iter().flat_map(|&p| ax_items_of(p)).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    out.sort_by(|a, b| a.frame.x.total_cmp(&b.frame.x));
    out
}

fn ax_items_of(pid: i32) -> Vec<RawItem> {
    let t0 = std::time::Instant::now();
    let Some(app) = ax::El::app(pid, 0.25) else { return vec![] };
    let bar = app.attr("AXExtrasMenuBar");
    if t0.elapsed() > Duration::from_millis(100) {
        log::debug!("menubar-manager: AX of pid {pid} took {:?}", t0.elapsed());
    }
    let Some(bar) = bar else { return vec![] };
    let kids = ax::El(bar).children();
    if kids.is_empty() {
        return vec![];
    }
    let (name, bundle) = app_info(pid);
    let owner = name.unwrap_or_else(|| format!("pid {pid}"));
    let mut out = vec![];
    for (index, k) in kids.iter().enumerate() {
        let Some(f) = k.frame() else { continue };
        let ident = k.string("AXIdentifier");
        let title = k.string("AXTitle").or_else(|| k.string("AXDescription")).or_else(|| k.string("AXHelp"));
        out.push(RawItem {
            system: is_system(&owner, bundle.as_deref()),
            owner: owner.clone(),
            bundle_id: bundle.clone(),
            pid,
            label: ident.or_else(|| title.clone()),
            title,
            index,
            frame: Rect::new(f.origin.x, f.origin.y, f.size.width, f.size.height),
            window_id: None,
            own: false,
        });
    }
    out
}

/// Find another app's item element again (for pressing it).
pub fn find_ax_item(pid: i32, id: &str, frame_hint: Option<Rect>) -> Option<ax::El> {
    let app = ax::El::app(pid, 0.5)?;
    let bar = ax::El(app.attr("AXExtrasMenuBar")?);
    let (name, bundle) = app_info(pid);
    let owner = name.unwrap_or_else(|| format!("pid {pid}"));
    let mut best = None;
    for (index, k) in bar.children().into_iter().enumerate() {
        let title = k.string("AXTitle").or_else(|| k.string("AXDescription")).or_else(|| k.string("AXHelp"));
        let label = k.string("AXIdentifier").or(title);
        if crate::logic::item_id(&owner, bundle.as_deref(), label.as_deref(), index) == id {
            return Some(k);
        }
        if let (Some(h), Some(f)) = (frame_hint, k.frame()) {
            if (f.origin.x - h.x).abs() < 2.0 && (f.size.width - h.w).abs() < 2.0 {
                best = Some(k);
            }
        }
    }
    best
}

/// What the menu bar really shows at a point (AX hit test through the
/// window server; on macOS 26+ this reflects the menu bar's own layout,
/// unlike the status items' window frames). Must not run on the main
/// thread when the point may be on one of this process's items.
#[derive(Clone, Debug, PartialEq)]
pub struct HitInfo {
    pub pid: i32,
    pub role: String,
    pub title: Option<String>,
    /// A status item (child of an `AXExtrasMenuBar`), not an app menu.
    pub status_item: bool,
}

pub fn hit(x: f64, y: f64) -> Option<HitInfo> {
    unsafe {
        let sys = Owned::new(AXUIElementCreateSystemWide())?;
        AXUIElementSetMessagingTimeout(sys.0, 0.5);
        let mut el: AXUIElementRef = std::ptr::null();
        if AXUIElementCopyElementAtPosition(sys.0, x as f32, y as f32, &mut el) != 0 || el.is_null() {
            return None;
        }
        let el = ax::El(Owned(el));
        AXUIElementSetMessagingTimeout(el.ptr(), 0.5);
        let mut pid = 0;
        if AXUIElementGetPid(el.ptr(), &mut pid) != 0 {
            return None;
        }
        let role = el.string("AXRole").unwrap_or_default();
        let status_item = role == "AXMenuBarItem"
            && (el.attr("AXParent").map(ax::El).and_then(|p| p.string("AXRole")).is_some_and(|r| r == "AXExtrasMenuBar") || is_extra(&el));
        let title = el.string("AXTitle").or_else(|| el.string("AXDescription"));
        Some(HitInfo { pid, role, title, status_item })
    }
}

/// Is this menu bar item one of its app's extras (status items)?
fn is_extra(el: &ax::El) -> bool {
    let mut pid = 0;
    if unsafe { AXUIElementGetPid(el.ptr(), &mut pid) } != 0 {
        return false;
    }
    let Some(app) = ax::El::app(pid, 0.3) else { return false };
    let Some(bar) = app.attr("AXExtrasMenuBar").map(ax::El) else { return false };
    let Some(f) = el.frame() else { return false };
    bar.children().iter().filter_map(|k| k.frame()).any(|k| (k.origin.x - f.origin.x).abs() < 1.0 && (k.size.width - f.size.width).abs() < 1.0)
}

/// The process owning the menu bar element at a point.
pub fn pid_at(x: f64, y: f64) -> Option<i32> {
    hit(x, y).map(|h| h.pid)
}

/// Cmd-drag along `path` (global top-left points). The first point must be
/// on an item of `pid`: checked with an AX hit test so nothing else is ever
/// dragged. Restores the pointer afterwards.
pub fn cmd_drag(path: &[(f64, f64)], pid: i32) -> Result<()> {
    let (Some(&start), Some(&end)) = (path.first(), path.last()) else { bail!("empty drag path") };
    if !unsafe { AXIsProcessTrusted() } {
        bail!("needs Accessibility");
    }
    match pid_at(start.0, start.1) {
        Some(p) if p == pid => {}
        other => bail!("the item isn't at ({:.0}, {:.0}) (found pid {other:?}); not dragging", start.0, start.1),
    }
    let orig = cursor();
    let pt = |p: (f64, f64)| CGPoint { x: p.0, y: p.1 };
    let sleep = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    let cmd = kCGEventFlagMaskCommand;
    post_mouse(kCGEventMouseMoved, pt(start), cmd, 0);
    sleep(30);
    post_mouse(kCGEventLeftMouseDown, pt(start), cmd, 1);
    sleep(120);
    for &p in &path[1..] {
        post_mouse(kCGEventLeftMouseDragged, pt(p), cmd, 1);
        sleep(16);
    }
    post_mouse(kCGEventLeftMouseDragged, pt(end), cmd, 1);
    sleep(80);
    post_mouse(kCGEventLeftMouseUp, pt(end), cmd, 1);
    sleep(40);
    unsafe {
        CGWarpMouseCursorPosition(orig);
        CGAssociateMouseAndMouseCursorPosition(1);
    }
    post_mouse(kCGEventMouseMoved, orig, 0, 0);
    Ok(())
}

/// A plain click at a point, pointer restored afterwards.
pub fn click_at(x: f64, y: f64) {
    let orig = cursor();
    let p = CGPoint { x, y };
    post_mouse(kCGEventMouseMoved, p, 0, 0);
    std::thread::sleep(Duration::from_millis(20));
    post_mouse(kCGEventLeftMouseDown, p, 0, 1);
    std::thread::sleep(Duration::from_millis(40));
    post_mouse(kCGEventLeftMouseUp, p, 0, 1);
    std::thread::sleep(Duration::from_millis(20));
    unsafe {
        CGWarpMouseCursorPosition(orig);
    }
}

/// Whether a menu (pop-up menu window) is open anywhere.
pub fn menu_open() -> bool {
    let Some(arr) = Owned::new(unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, 0) }) else { return false };
    let n = unsafe { CFArrayGetCount(arr.0) };
    (0..n).any(|i| {
        let d = unsafe { CFArrayGetValueAtIndex(arr.0, i) };
        !d.is_null() && dict_i64(d, unsafe { kCGWindowLayer }) == Some(kCGPopUpMenuWindowLevel)
    })
}

/// Right edge of the frontmost app's menus (AX), to tell the empty part of
/// the menu bar from the app menus.
pub fn app_menus_right(pid: i32) -> Option<f64> {
    let app = ax::El::app(pid, 0.15)?;
    let bar = ax::El(app.attr("AXMenuBar")?);
    bar.children().iter().filter_map(|k| k.frame()).map(|f| f.origin.x + f.size.width).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
}

/// Hit-test along the menu bar and return the status items it really
/// shows: (left x, pid, title).
pub fn scan_visible(screen: crate::Rect, menu_bar_h: f64) -> Vec<(f64, i32, Option<String>)> {
    let y = screen.y + (menu_bar_h / 2.0).max(4.0);
    let mut out: Vec<(f64, i32, Option<String>)> = vec![];
    let mut last: Option<(i32, Option<String>)> = None;
    let mut x = screen.x + 2.0;
    while x < screen.right() - 1.0 {
        // A hit test can fail while an app is busy: try twice.
        let h = hit(x, y).or_else(|| hit(x, y)).filter(|h| h.status_item).map(|h| (h.pid, h.title));
        if h != last {
            if let Some((pid, title)) = &h {
                out.push((x, *pid, title.clone()));
            }
            last = h;
        }
        x += 4.0;
    }
    out
}
