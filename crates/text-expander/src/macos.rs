//! macOS backend: a subscriber of the shared event tap (`pm-hook`) at
//! [`pm_hook::priority::TEXT_EXPANDER`] — after Keyboard Manager, so it sees
//! the remapped keys, as the session-level tap it used to have did. Its
//! backspaces and text replace the completing key (`Verdict::Replace`): the
//! lower-priority subscribers see them, then they are posted after the tap.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_ulong, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{bail, Result};
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
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
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
const kCGEventSourceStatePrivate: i32 = -1;

const kCGEventKeyDown: u32 = 10;

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

/// Where posted events go: from the tap handler they replace the current
/// event; otherwise they are posted at the HID level.
enum Sink<'a> {
    Hid,
    Tap(&'a mut Vec<pm_hook::Emit>),
}

unsafe fn post(sink: &mut Sink<'_>, ev: CGEventRef) {
    if ev.is_null() {
        return;
    }
    CGEventSetIntegerValueField(ev, kCGEventSourceUserData, INJECTED_TAG as i64);
    match sink {
        Sink::Hid => {
            CGEventPost(kCGHIDEventTap, ev);
            CFRelease(ev as CFTypeRef);
        }
        Sink::Tap(out) => {
            if let Some(e) = pm_hook::Synthetic::from_cg(ev) {
                out.push(pm_hook::Emit::Event(e));
            }
        }
    }
}

unsafe fn post_key(sink: &mut Sink<'_>, keycode: u16, flags: u64) {
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
            post(sink, ev);
        }
    }
}

fn repeat_key(sink: &mut Sink<'_>, keycode: u16, n: usize) {
    for _ in 0..n {
        unsafe { post_key(sink, keycode, 0) };
    }
}

/// Type `text` with one unicode key event per character (some apps only
/// read the first character of longer strings); `\n` presses Return, `\t`
/// Tab.
fn post_text(sink: &mut Sink<'_>, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => continue,
            '\r' | '\n' => repeat_key(sink, RETURN, 1),
            '\t' => repeat_key(sink, TAB, 1),
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
                            post(sink, ev);
                        }
                    }
                }
            }
        }
    }
}

/// Perform a plan from the tap callback (a paste goes to the worker).
fn perform(shared: &Shared, sink: &mut Sink<'_>, plan: Plan) {
    repeat_key(sink, RIGHT_ARROW, plan.right);
    repeat_key(sink, DELETE, plan.backspaces);
    match plan.insert {
        Insert::Type(t) => {
            post_text(sink, &t);
            repeat_key(sink, LEFT_ARROW, plan.left);
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
    let p = &mut Sink::Hid;
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
        post(&mut Sink::Hid, ev);
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
// Tap (a pm-hook subscriber)
// ---------------------------------------------------------------------------

struct TapCtx {
    shared: Arc<Shared>,
    /// Target pid of the last key event and its app identifiers.
    pid: Cell<i64>,
    app: RefCell<Option<Vec<String>>>,
}

/// The subscription; dropping it unsubscribes.
pub struct Hook {
    _guard: pm_hook::Guard,
}

pub fn start(shared: Arc<Shared>) -> Result<Hook> {
    use pm_hook::{EventMask, EventType, Subscription, Verdict};
    let mask = EventMask::of(&[EventType::KeyDown, EventType::LeftMouseDown, EventType::RightMouseDown, EventType::OtherMouseDown]);
    let _ = source();
    let ctx = TapCtx { shared, pid: Cell::new(-1), app: RefCell::new(None) };
    let guard = pm_hook::subscribe(
        Subscription::new("text-expander", pm_hook::priority::TEXT_EXPANDER, mask, move |ev| {
            let (ty, event) = (ev.cg_type(), ev.cg_event());
            let mut emits = Vec::new();
            let keep = catch_unwind(AssertUnwindSafe(|| unsafe { handle(&ctx, &mut Sink::Tap(&mut emits), ty, event) }))
                .unwrap_or_else(|_| {
                    log::error!("text-expander: event handler panicked");
                    true
                });
            match (keep, emits.is_empty()) {
                (true, true) => Verdict::Pass,
                (false, true) => Verdict::Swallow,
                (keep, false) => {
                    if keep {
                        emits.push(pm_hook::Emit::Original);
                    }
                    Verdict::Replace(emits)
                }
            }
        })
        .ignore_tags(&[INJECTED_TAG]),
    );
    if !pm_hook::status().is_active() {
        drop(guard);
        bail!("could not create the text expander's event tap: grant Accessibility in System Settings > Privacy & Security");
    }
    Ok(Hook { _guard: guard })
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
unsafe fn handle(ctx: &TapCtx, sink: &mut Sink<'_>, ty: u32, event: CGEventRef) -> bool {
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
            perform(&ctx.shared, sink, plan);
            false
        }
    }
}
