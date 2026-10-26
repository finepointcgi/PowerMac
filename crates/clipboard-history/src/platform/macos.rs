//! macOS: `NSPasteboard` reading/writing, frontmost app, activation, the
//! pointer position and the global hotkey (an active `CGEventTap`).

#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSPasteboard, NSPasteboardItem, NSPasteboardWriting,
    NSRunningApplication, NSWorkspace,
};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

use crate::content::{png_to_tiff, to_png, Content, SourceApp};

const T_TEXT: &str = "public.utf8-plain-text";
const T_HTML: &str = "public.html";
const T_RTF: &str = "public.rtf";
const T_PNG: &str = "public.png";
const T_TIFF: &str = "public.tiff";
const T_FILE_URL: &str = "public.file-url";
/// nspasteboard.org: bundle id of the app that wrote the pasteboard.
const T_SOURCE: &str = "org.nspasteboard.source";

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn change_count() -> i64 {
    autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as i64)
}

/// Every type of every item on the pasteboard, plus the source app (from
/// `org.nspasteboard.source` when present, else the frontmost app).
pub fn read_meta() -> (Vec<String>, Option<SourceApp>) {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        let mut types: Vec<String> = Vec::new();
        let mut marked_source = None;
        if let Some(items) = pb.pasteboardItems() {
            for item in items.iter() {
                for t in item.types().iter() {
                    let t = t.to_string();
                    if t == T_SOURCE && marked_source.is_none() {
                        marked_source = item.stringForType(&ns(T_SOURCE)).map(|s| s.to_string());
                    }
                    if !types.contains(&t) {
                        types.push(t);
                    }
                }
            }
        }
        let front = frontmost().map(|f| f.app);
        let source = match marked_source.filter(|s| !s.is_empty()) {
            Some(id) => {
                let name = front
                    .as_ref()
                    .filter(|f| f.id.as_deref() == Some(id.as_str()))
                    .and_then(|f| f.name.clone())
                    .or_else(|| app_name_for_bundle(&id));
                Some(SourceApp { name, id: Some(id) })
            }
            None => front,
        };
        (types, source)
    })
}

fn app_name_for_bundle(id: &str) -> Option<String> {
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns(id));
    let app = apps.iter().next()?;
    app.localizedName().map(|n| n.to_string())
}

/// Read the formats the history keeps.
pub fn read_content(types: &[String]) -> Content {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        let has = |t: &str| types.iter().any(|x| x == t);
        let mut c = Content::default();
        if let Some(items) = pb.pasteboardItems() {
            for item in items.iter() {
                if let Some(u) = item.stringForType(&ns(T_FILE_URL)) {
                    if let Some(p) = NSURL::URLWithString(&u).and_then(|u| u.path()) {
                        c.files.push(p.to_string());
                    }
                }
            }
        }
        if has(T_TEXT) {
            c.text = pb.stringForType(&ns(T_TEXT)).map(|s| s.to_string());
        }
        if has(T_HTML) {
            c.html = pb
                .dataForType(&ns(T_HTML))
                .map(|d| String::from_utf8_lossy(&d.to_vec()).into_owned());
        }
        if has(T_RTF) {
            c.rtf = pb.dataForType(&ns(T_RTF)).map(|d| d.to_vec());
        }
        if c.files.is_empty() {
            let img = if has(T_PNG) {
                pb.dataForType(&ns(T_PNG))
            } else if has(T_TIFF) {
                pb.dataForType(&ns(T_TIFF))
            } else {
                None
            };
            if let Some(d) = img {
                c.png = to_png(&d.to_vec()).map(|(p, _, _)| p);
            }
        }
        c
    })
}

/// Put `c` on the pasteboard (`plain`: text only). Returns the new change
/// count.
pub fn write(c: &Content, plain: bool) -> Result<i64> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let main = NSPasteboardItem::new();
        let mut any = false;
        if let Some(t) = &c.text {
            any |= main.setString_forType(&ns(t), &ns(T_TEXT));
        }
        if !plain {
            if let Some(h) = &c.html {
                any |= main.setData_forType(&NSData::with_bytes(h.as_bytes()), &ns(T_HTML));
            }
            if let Some(r) = &c.rtf {
                any |= main.setData_forType(&NSData::with_bytes(r), &ns(T_RTF));
            }
            if let Some(p) = &c.png {
                any |= main.setData_forType(&NSData::with_bytes(p), &ns(T_PNG));
                if let Some(t) = png_to_tiff(p) {
                    main.setData_forType(&NSData::with_bytes(&t), &ns(T_TIFF));
                }
            }
        }
        let mut objs: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
        if !plain && !c.files.is_empty() {
            // One item per file (what Finder does); the first also carries the text.
            for (i, f) in c.files.iter().enumerate() {
                let item = if i == 0 {
                    main.clone()
                } else {
                    NSPasteboardItem::new()
                };
                let url = NSURL::fileURLWithPath(&ns(f));
                if let Some(s) = url.absoluteString() {
                    item.setString_forType(&s, &ns(T_FILE_URL));
                }
                objs.push(ProtocolObject::from_retained(item));
            }
            any = true;
        } else if plain && c.text.is_none() && !c.files.is_empty() {
            any |= main.setString_forType(&ns(&c.files.join("\n")), &ns(T_TEXT));
            objs.push(ProtocolObject::from_retained(main));
        } else {
            objs.push(ProtocolObject::from_retained(main));
        }
        if !any {
            bail!("nothing to put on the clipboard");
        }
        if !pb.writeObjects(&NSArray::from_retained_slice(&objs)) {
            bail!("NSPasteboard writeObjects failed");
        }
        Ok(pb.changeCount() as i64)
    })
}

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetCount(a: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> *const c_void;
    fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(n: *const c_void, ty: isize, out: *mut c_void) -> u8;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, ret: u8) -> i32;
    fn CFRunLoopStop(rl: *mut c_void);
    fn CFMachPortCreateRunLoopSource(
        a: *const c_void,
        port: *mut c_void,
        order: isize,
    ) -> *mut c_void;
    fn CFMachPortInvalidate(port: *mut c_void);
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGWindowLayer: CFStringRef;
    static kCGWindowOwnerPID: CFStringRef;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(e: *mut c_void) -> CGPoint;
    fn CGGetDisplaysWithPoint(p: CGPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(d: u32) -> CGRect;
    fn CGMainDisplayID() -> u32;
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        cb: unsafe extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void,
        user: *mut c_void,
    ) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, on: bool);
    fn CGEventGetIntegerValueField(e: *mut c_void, f: u32) -> i64;
    fn CGEventGetFlags(e: *mut c_void) -> u64;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementGetPid(el: CFTypeRef, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
}

/// Pid of the app that has keyboard focus, from Accessibility (always
/// current, on any thread; needs Accessibility trust).
fn focused_app_pid() -> Option<i32> {
    unsafe {
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(sys, 0.25);
        let attr = ns("AXFocusedApplication");
        let mut app: CFTypeRef = std::ptr::null();
        let err =
            AXUIElementCopyAttributeValue(sys, Retained::as_ptr(&attr) as CFStringRef, &mut app);
        CFRelease(sys);
        if err != 0 || app.is_null() {
            return None;
        }
        let mut pid = 0;
        let ok = AXUIElementGetPid(app, &mut pid) == 0;
        CFRelease(app);
        (ok && pid > 0).then_some(pid)
    }
}

const kCGWindowListOptionOnScreenOnly: u32 = 1;
const kCGWindowListExcludeDesktopElements: u32 = 16;

unsafe fn dict_i64(d: *const c_void, key: CFStringRef) -> Option<i64> {
    let n = CFDictionaryGetValue(d, key);
    if n.is_null() {
        return None;
    }
    let mut v: i64 = 0;
    (CFNumberGetValue(n, 4 /* SInt64 */, &mut v as *mut i64 as *mut c_void) != 0).then_some(v)
}

/// Owner of the front-most normal window (works on any thread).
fn frontmost_window_pid() -> Option<i32> {
    unsafe {
        let arr = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            0,
        );
        if arr.is_null() {
            return None;
        }
        let mut pid = None;
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            if dict_i64(d, kCGWindowLayer) == Some(0) {
                pid = dict_i64(d, kCGWindowOwnerPID).map(|p| p as i32);
                break;
            }
        }
        CFRelease(arr);
        pid
    }
}

/// The app in front: pid plus names.
#[derive(Debug, Clone)]
pub struct FrontApp {
    pub pid: i32,
    /// Unused on macOS (Windows: the foreground HWND).
    pub window: isize,
    pub app: SourceApp,
}

/// The frontmost app: the one with keyboard focus according to
/// Accessibility, else `NSWorkspace` (only current while the main run loop
/// runs), else the owner of the front-most window.
pub fn frontmost() -> Option<FrontApp> {
    autoreleasepool(|_| {
        let app = focused_app_pid()
            .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
            .or_else(|| NSWorkspace::sharedWorkspace().frontmostApplication())
            .or_else(|| {
                NSRunningApplication::runningApplicationWithProcessIdentifier(
                    frontmost_window_pid()?
                )
            })?;
        Some(FrontApp {
            pid: app.processIdentifier(),
            window: 0,
            app: SourceApp {
                name: app.localizedName().map(|s| s.to_string()),
                id: app.bundleIdentifier().map(|s| s.to_string()),
            },
        })
    })
}

/// Bring the app with `pid` to the front.
pub fn activate(pid: i32, _window: isize) -> bool {
    autoreleasepool(|_| {
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return false;
        };
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps)
    })
}

/// Whether `pid` is the frontmost app.
pub fn is_frontmost(pid: i32) -> bool {
    frontmost().is_some_and(|f| f.pid == pid)
}

/// Pointer position and the bounds of its screen, in global points
/// (top-left origin, like window positions).
pub fn pointer_and_screen() -> Option<([f32; 2], [f32; 4])> {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return None;
        }
        let p = CGEventGetLocation(e);
        CFRelease(e as CFTypeRef);
        let mut d = 0u32;
        let mut n = 0u32;
        let display = if CGGetDisplaysWithPoint(p, 1, &mut d, &mut n) == 0 && n > 0 {
            d
        } else {
            CGMainDisplayID()
        };
        let b = CGDisplayBounds(display);
        Some((
            [p.x as f32, p.y as f32],
            [
                b.origin.x as f32,
                b.origin.y as f32,
                b.size.x as f32,
                b.size.y as f32,
            ],
        ))
    }
}

// ---------------------------------------------------------------------------
// Hotkey (active event tap)
// ---------------------------------------------------------------------------

const kCGSessionEventTap: u32 = 1;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;
const kCGEventKeyDown: u32 = 10;
const kCGEventKeyUp: u32 = 11;
const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;
const kCGKeyboardEventAutorepeat: u32 = 8;
const kCGKeyboardEventKeycode: u32 = 9;
const kCGEventSourceUserData: u32 = 42;

const NX_DEVICELCTLKEYMASK: u64 = 0x01;
const NX_DEVICELSHIFTKEYMASK: u64 = 0x02;
const NX_DEVICERSHIFTKEYMASK: u64 = 0x04;
const NX_DEVICELCMDKEYMASK: u64 = 0x08;
const NX_DEVICERCMDKEYMASK: u64 = 0x10;
const NX_DEVICELALTKEYMASK: u64 = 0x20;
const NX_DEVICERALTKEYMASK: u64 = 0x40;
const NX_DEVICERCTLKEYMASK: u64 = 0x2000;
const kCGEventFlagMaskShift: u64 = 0x0002_0000;
const kCGEventFlagMaskControl: u64 = 0x0004_0000;
const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

struct TapCtx {
    hotkey: keyboard_manager::Shortcut,
    fire: mpsc::Sender<()>,
    tap: std::cell::Cell<*mut c_void>,
    /// Keycode whose key-up we swallow (the hotkey's).
    swallow_up: std::cell::Cell<Option<i64>>,
}

/// Sided modifier VKs held according to event flags.
fn held_from_flags(f: u64) -> Vec<u32> {
    use pm_input::vk::*;
    let fams = [
        (
            kCGEventFlagMaskCommand,
            NX_DEVICELCMDKEYMASK,
            NX_DEVICERCMDKEYMASK,
            VK_LWIN,
            VK_RWIN,
        ),
        (
            kCGEventFlagMaskControl,
            NX_DEVICELCTLKEYMASK,
            NX_DEVICERCTLKEYMASK,
            VK_LCONTROL,
            VK_RCONTROL,
        ),
        (
            kCGEventFlagMaskAlternate,
            NX_DEVICELALTKEYMASK,
            NX_DEVICERALTKEYMASK,
            VK_LMENU,
            VK_RMENU,
        ),
        (
            kCGEventFlagMaskShift,
            NX_DEVICELSHIFTKEYMASK,
            NX_DEVICERSHIFTKEYMASK,
            VK_LSHIFT,
            VK_RSHIFT,
        ),
    ];
    let mut v = Vec::new();
    for (generic, l, r, vl, vr) in fams {
        if f & generic == 0 {
            continue;
        }
        let (hl, hr) = (f & l != 0, f & r != 0);
        if hl {
            v.push(vl);
        }
        if hr {
            v.push(vr);
        }
        if !hl && !hr {
            v.push(vl);
        }
    }
    v
}

unsafe extern "C" fn tap_cb(
    _proxy: *mut c_void,
    ty: u32,
    e: *mut c_void,
    user: *mut c_void,
) -> *mut c_void {
    let ctx = &*(user as *const TapCtx);
    if ty == kCGEventTapDisabledByTimeout || ty == kCGEventTapDisabledByUserInput {
        let t = ctx.tap.get();
        if !t.is_null() {
            CGEventTapEnable(t, true);
        }
        return e;
    }
    if e.is_null() || (ty != kCGEventKeyDown && ty != kCGEventKeyUp) {
        return e;
    }
    // Mouse Without Borders' remote input isn't ours to act on.
    if CGEventGetIntegerValueField(e, kCGEventSourceUserData) == keyboard_manager::MWB_TAG as i64 {
        return e;
    }
    let kc = CGEventGetIntegerValueField(e, kCGKeyboardEventKeycode);
    if ty == kCGEventKeyUp {
        if ctx.swallow_up.get() == Some(kc) {
            ctx.swallow_up.set(None);
            return std::ptr::null_mut();
        }
        return e;
    }
    let Some((vk, _)) = pm_input::vk::mac_to_vk(kc as u16) else {
        return e;
    };
    if vk != ctx.hotkey.key {
        return e;
    }
    let held = held_from_flags(CGEventGetFlags(e));
    if !ctx.hotkey.mods.matches_exactly(|k| held.contains(&k)) {
        return e;
    }
    ctx.swallow_up.set(Some(kc));
    if CGEventGetIntegerValueField(e, kCGKeyboardEventAutorepeat) == 0 {
        let _ = ctx.fire.send(());
    }
    std::ptr::null_mut()
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

/// A registered global hotkey; dropping it unregisters.
pub struct Hotkey {
    stop: Arc<AtomicBool>,
    run_loop: SendPtr,
    thread: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Hotkey {
    /// Swallow `hotkey` system-wide and call `on_fire` (on a worker thread)
    /// when it is pressed. Needs Accessibility permission.
    pub fn start(
        hotkey: keyboard_manager::Shortcut,
        on_fire: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Hotkey> {
        let (fire_tx, fire_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("cliphist-hotkey".into())
            .spawn(move || {
                for () in fire_rx {
                    on_fire();
                }
            })?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let (tx, rx) = mpsc::channel::<std::result::Result<SendPtr, String>>();
        let thread = std::thread::Builder::new()
            .name("cliphist-tap".into())
            .spawn(move || unsafe {
                let ctx = Box::into_raw(Box::new(TapCtx {
                    hotkey,
                    fire: fire_tx,
                    tap: std::cell::Cell::new(std::ptr::null_mut()),
                    swallow_up: std::cell::Cell::new(None),
                }));
                let mask = (1u64 << kCGEventKeyDown) | (1u64 << kCGEventKeyUp);
                let tap = CGEventTapCreate(
                    kCGSessionEventTap,
                    kCGHeadInsertEventTap,
                    kCGEventTapOptionDefault,
                    mask,
                    tap_cb,
                    ctx as *mut c_void,
                );
                if tap.is_null() {
                    drop(Box::from_raw(ctx));
                    let _ = tx.send(Err(
                        "could not create the hotkey event tap (Accessibility permission?)".into(),
                    ));
                    return;
                }
                (*ctx).tap.set(tap);
                let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                let rl = CFRunLoopGetCurrent();
                CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
                let _ = tx.send(Ok(SendPtr(rl)));
                while !stop2.load(Ordering::SeqCst) {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, 0);
                }
                CGEventTapEnable(tap, false);
                CFMachPortInvalidate(tap);
                CFRelease(src as CFTypeRef);
                CFRelease(tap as CFTypeRef);
                drop(Box::from_raw(ctx)); // closes the fire channel
            })?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(rl)) => Ok(Hotkey {
                stop,
                run_loop: rl,
                thread: Some(thread),
                worker: Some(worker),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                let _ = worker.join();
                Err(anyhow!(e))
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(anyhow!("hotkey thread did not start"))
            }
        }
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Clipboard watching on macOS is polling `changeCount` (there is no
/// notification); see [`crate::watch`].
pub const POLL: Duration = Duration::from_millis(300);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_to_mods() {
        use pm_input::vk::*;
        let f = kCGEventFlagMaskCommand
            | NX_DEVICELCMDKEYMASK
            | kCGEventFlagMaskShift
            | NX_DEVICERSHIFTKEYMASK;
        assert_eq!(held_from_flags(f), vec![VK_LWIN, VK_RSHIFT]);
        // Generic bit without device bits (synthetic events): left side.
        assert_eq!(held_from_flags(kCGEventFlagMaskControl), vec![VK_LCONTROL]);
        let hk: keyboard_manager::Shortcut = "Cmd+Shift+V".parse().unwrap();
        assert!(hk.mods.matches_exactly(|k| held_from_flags(f).contains(&k)));
        assert!(!hk
            .mods
            .matches_exactly(|k| held_from_flags(f | kCGEventFlagMaskAlternate).contains(&k)));
    }

    #[test]
    fn frontmost_and_pointer_do_not_crash() {
        let _ = frontmost();
        let _ = pointer_and_screen();
        let _ = change_count();
    }
}
