//! macOS backend: an active session-level `CGEventTap` on its own run loop.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_ulong, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use pm_keys::*;

use crate::engine::{Decision, Insert, Key, Plan};
use crate::vars::SystemEnv;
use crate::{Job, Shared, INJECTED_TAG};

/// macOS virtual keycodes used here.
mod pm_keys {
    pub const RETURN: u16 = 0x24;
    pub const TAB: u16 = 0x30;
    pub const DELETE: u16 = 0x33;
    pub const ESCAPE: u16 = 0x35;
    pub const KEYPAD_ENTER: u16 = 0x4C;
    pub const ANSI_V: u16 = 0x09;
    pub const COMMAND: u16 = 0x37;
    pub const LEFT_ARROW: u16 = 0x7B;
    pub const RIGHT_ARROW: u16 = 0x7C;
    pub const DOWN_ARROW: u16 = 0x7D;
    pub const UP_ARROW: u16 = 0x7E;
    pub const HOME: u16 = 0x73;
    pub const END: u16 = 0x77;
    pub const PAGE_UP: u16 = 0x74;
    pub const PAGE_DOWN: u16 = 0x79;
    pub const FORWARD_DELETE: u16 = 0x75;
    pub const HELP: u16 = 0x72;
    /// F1-F20
    pub const FKEYS: &[u16] = &[
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71,
        0x6A, 0x40, 0x4F, 0x50, 0x5A,
    ];
}

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventTapCallBack =
    unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFMachPortCreateRunLoopSource(
        alloc: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventTapPostEvent(proxy: CGEventTapProxy, event: CGEventRef);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetType(event: CGEventRef, ty: u32);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: c_ulong, s: *const u16);
    fn CGEventKeyboardGetUnicodeString(
        event: CGEventRef,
        max: c_ulong,
        actual: *mut c_ulong,
        s: *mut u16,
    );
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventSourceSetUserData(source: CGEventSourceRef, data: i64);
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, secs: f64);
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

const kCGHIDEventTap: u32 = 0;
const kCGSessionEventTap: u32 = 1;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;
const kCGEventSourceStatePrivate: i32 = -1;

const kCGEventLeftMouseDown: u32 = 1;
const kCGEventRightMouseDown: u32 = 3;
const kCGEventKeyDown: u32 = 10;
const kCGEventOtherMouseDown: u32 = 25;
const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

const kCGKeyboardEventKeycode: u32 = 9;
const kCGEventTargetUnixProcessID: u32 = 40;
const kCGEventSourceUserData: u32 = 42;

const kCGEventFlagMaskShift: u64 = 0x0002_0000;
const kCGEventFlagMaskControl: u64 = 0x0004_0000;
const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
const kCGEventFlagMaskCommand: u64 = 0x0010_0000;
const kCGEventFlagMaskNumericPad: u64 = 0x0020_0000;
const kCGEventFlagMaskSecondaryFn: u64 = 0x0080_0000;
const kCGEventFlagMaskNonCoalesced: u64 = 0x0000_0100;
const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;

fn mask(ty: u32) -> u64 {
    1u64 << ty
}

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

// ---------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------

fn source() -> CGEventSourceRef {
    static SRC: OnceLock<SendPtr> = OnceLock::new();
    SRC.get_or_init(|| unsafe {
        let s = CGEventSourceCreate(kCGEventSourceStatePrivate);
        if !s.is_null() {
            CGEventSourceSetUserData(s, INJECTED_TAG as i64);
            CGEventSourceSetLocalEventsSuppressionInterval(s, 0.0);
        }
        SendPtr(s)
    })
    .0
}

/// Post after our tap (from the callback) or at the HID level (`proxy` null).
unsafe fn post(proxy: CGEventTapProxy, ev: CGEventRef) {
    if ev.is_null() {
        return;
    }
    CGEventSetIntegerValueField(ev, kCGEventSourceUserData, INJECTED_TAG as i64);
    if proxy.is_null() {
        CGEventPost(kCGHIDEventTap, ev);
    } else {
        CGEventTapPostEvent(proxy, ev);
    }
    CFRelease(ev as CFTypeRef);
}

unsafe fn post_key(proxy: CGEventTapProxy, keycode: u16, flags: u64) {
    let extra = match keycode {
        LEFT_ARROW | RIGHT_ARROW | UP_ARROW | DOWN_ARROW => {
            kCGEventFlagMaskNumericPad | kCGEventFlagMaskSecondaryFn
        }
        _ => 0,
    };
    for down in [true, false] {
        let ev = CGEventCreateKeyboardEvent(source(), keycode, down);
        if !ev.is_null() {
            CGEventSetFlags(ev, flags | extra | kCGEventFlagMaskNonCoalesced);
            post(proxy, ev);
        }
    }
}

fn repeat_key(proxy: CGEventTapProxy, keycode: u16, n: usize) {
    for _ in 0..n {
        unsafe { post_key(proxy, keycode, 0) };
    }
}

/// Type `text` with one unicode key event per character (some apps only
/// read the first character of longer strings); `\n` presses Return, `\t`
/// Tab.
fn post_text(proxy: CGEventTapProxy, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => continue,
            '\r' | '\n' => repeat_key(proxy, RETURN, 1),
            '\t' => repeat_key(proxy, TAB, 1),
            c => {
                let mut units = [0u16; 2];
                let units = c.encode_utf16(&mut units);
                for down in [true, false] {
                    unsafe {
                        let ev = CGEventCreateKeyboardEvent(source(), 0, down);
                        if !ev.is_null() {
                            CGEventKeyboardSetUnicodeString(
                                ev,
                                units.len() as c_ulong,
                                units.as_ptr(),
                            );
                            CGEventSetFlags(ev, kCGEventFlagMaskNonCoalesced);
                            post(proxy, ev);
                        }
                    }
                }
            }
        }
    }
}

/// Perform a plan from the tap callback (a paste goes to the worker).
fn perform(shared: &Shared, proxy: CGEventTapProxy, plan: Plan) {
    repeat_key(proxy, RIGHT_ARROW, plan.right);
    repeat_key(proxy, DELETE, plan.backspaces);
    match plan.insert {
        Insert::Type(t) => {
            post_text(proxy, &t);
            repeat_key(proxy, LEFT_ARROW, plan.left);
        }
        Insert::Paste(text) => shared.submit(Job::Paste {
            text,
            left: plan.left,
        }),
    }
}

/// Cmd+V, then Left `left` times (from the worker thread).
pub fn paste_and_move(left: usize) -> Result<()> {
    if !unsafe { CGPreflightPostEventAccess() } {
        bail!("not permitted to post events (Accessibility)");
    }
    let cmd = kCGEventFlagMaskCommand | NX_DEVICELCMDKEYMASK;
    let p = std::ptr::null_mut();
    unsafe {
        post_flags(COMMAND, true, cmd);
        post_key(p, ANSI_V, cmd);
        post_flags(COMMAND, false, 0);
    }
    if left > 0 {
        // Let the app process the paste first.
        std::thread::sleep(Duration::from_millis(50));
        repeat_key(p, LEFT_ARROW, left);
    }
    Ok(())
}

unsafe fn post_flags(keycode: u16, down: bool, flags: u64) {
    let ev = CGEventCreateKeyboardEvent(source(), keycode, down);
    if !ev.is_null() {
        CGEventSetType(ev, 12); // kCGEventFlagsChanged
        CGEventSetFlags(ev, flags | kCGEventFlagMaskNonCoalesced);
        post(std::ptr::null_mut(), ev);
    }
}

// ---------------------------------------------------------------------------
// App identity
// ---------------------------------------------------------------------------

fn app_ids(pid: i32) -> Vec<String> {
    use objc2_app_kit::NSRunningApplication;
    if pid <= 0 {
        return keyboard_manager::foreground_app();
    }
    objc2::rc::autoreleasepool(|_| {
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        if let Some(b) = app.bundleIdentifier() {
            v.push(b.to_string());
        }
        if let Some(n) = app.localizedName() {
            v.push(n.to_string());
        }
        if let Some(n) = app.executableURL().and_then(|u| u.lastPathComponent()) {
            v.push(n.to_string());
        }
        v
    })
}

// ---------------------------------------------------------------------------
// Tap
// ---------------------------------------------------------------------------

struct TapCtx {
    shared: Arc<Shared>,
    tap: Cell<CFMachPortRef>,
    /// Target pid of the last key event and its app identifiers.
    pid: Cell<i64>,
    app: RefCell<Option<Vec<String>>>,
}

pub struct Hook {
    stop: Arc<AtomicBool>,
    run_loop: SendPtr,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(shared: Arc<Shared>) -> Result<Hook> {
    let events = mask(kCGEventKeyDown)
        | mask(kCGEventLeftMouseDown)
        | mask(kCGEventRightMouseDown)
        | mask(kCGEventOtherMouseDown);
    let _ = source();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let (tx, rx) = mpsc::channel::<std::result::Result<SendPtr, String>>();
    let thread = std::thread::Builder::new()
        .name("te-eventtap".into())
        .spawn(move || unsafe {
            let ctx = Box::into_raw(Box::new(TapCtx {
                shared,
                tap: Cell::new(std::ptr::null_mut()),
                pid: Cell::new(-1),
                app: RefCell::new(None),
            }));
            let tap = CGEventTapCreate(
                kCGSessionEventTap,
                kCGHeadInsertEventTap,
                kCGEventTapOptionDefault,
                events,
                tap_callback,
                ctx as *mut c_void,
            );
            if tap.is_null() {
                drop(Box::from_raw(ctx));
                let _ = tx.send(Err(
                    "could not create the text expander's event tap: grant \
                    Accessibility in System Settings > Privacy & Security"
                        .into(),
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
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(tap);
            CFRelease(src as CFTypeRef);
            CFRelease(tap as CFTypeRef);
            drop(Box::from_raw(ctx));
        })?;
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(rl)) => Ok(Hook {
            stop,
            run_loop: rl,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(anyhow!(e))
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err(anyhow!("event tap thread did not start"))
        }
    }
}

unsafe extern "C" fn tap_callback(
    proxy: CGEventTapProxy,
    ty: u32,
    event: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    let ctx = &*(user as *const TapCtx);
    if ty == kCGEventTapDisabledByTimeout || ty == kCGEventTapDisabledByUserInput {
        log::warn!("text-expander: event tap disabled ({ty:#x}); re-enabling");
        let tap = ctx.tap.get();
        if !tap.is_null() {
            CGEventTapEnable(tap, true);
        }
        return event;
    }
    if event.is_null()
        || CGEventGetIntegerValueField(event, kCGEventSourceUserData) == INJECTED_TAG as i64
    {
        return event;
    }
    let keep = objc2::rc::autoreleasepool(|_| {
        catch_unwind(AssertUnwindSafe(|| handle(ctx, proxy, ty, event))).unwrap_or_else(|_| {
            log::error!("text-expander: event handler panicked");
            true
        })
    });
    if keep {
        event
    } else {
        std::ptr::null_mut()
    }
}

/// Classify a key-down for the engine.
unsafe fn classify(event: CGEventRef) -> Key {
    let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;
    let flags = CGEventGetFlags(event);
    if flags & (kCGEventFlagMaskCommand | kCGEventFlagMaskControl) != 0 {
        return Key::Reset;
    }
    match keycode {
        DELETE if flags & (kCGEventFlagMaskAlternate | kCGEventFlagMaskShift) == 0 => {
            Key::Backspace
        }
        DELETE => Key::Reset, // Option+Backspace deletes a word
        RETURN | KEYPAD_ENTER => Key::Text("\n".into()),
        TAB => Key::Text("\t".into()),
        LEFT_ARROW | RIGHT_ARROW | UP_ARROW | DOWN_ARROW | HOME | END | PAGE_UP | PAGE_DOWN
        | FORWARD_DELETE | ESCAPE | HELP => Key::Reset,
        k if FKEYS.contains(&k) => Key::Reset,
        _ => {
            let mut buf = [0u16; 8];
            let mut n: c_ulong = 0;
            CGEventKeyboardGetUnicodeString(event, buf.len() as c_ulong, &mut n, buf.as_mut_ptr());
            Key::Text(String::from_utf16_lossy(
                &buf[..(n as usize).min(buf.len())],
            ))
        }
    }
}

/// Returns whether to deliver the event.
unsafe fn handle(ctx: &TapCtx, proxy: CGEventTapProxy, ty: u32, event: CGEventRef) -> bool {
    let Ok(mut ex) = ctx.shared.expander.lock() else {
        return true;
    };
    if ty != kCGEventKeyDown {
        ex.reset(); // mouse click: the caret may have moved
        return true;
    }
    if IsSecureEventInputEnabled() != 0 {
        ex.reset();
        return true;
    }
    let pid = CGEventGetIntegerValueField(event, kCGEventTargetUnixProcessID);
    if pid != ctx.pid.get() {
        // focus moved to another app
        ctx.pid.set(pid);
        ctx.app.replace(None);
        ex.reset();
    }
    let mut ids = || -> Vec<String> {
        ctx.app
            .borrow_mut()
            .get_or_insert_with(|| app_ids(pid as i32))
            .clone()
    };
    let key = classify(event);
    log::trace!("text-expander: pid {pid} {key:?}");
    if matches!(key, Key::Text(_)) && ex.excluded(&ids()) {
        ex.reset();
        return true;
    }
    match ex.key(key, &mut ids, &SystemEnv) {
        Decision::Pass => true,
        Decision::Swallow(plan) => {
            drop(ex);
            log::debug!("text-expander: {plan:?}");
            perform(&ctx.shared, proxy, plan);
            false
        }
    }
}
