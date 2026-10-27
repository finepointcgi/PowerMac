//! macOS backend: a subscriber of the shared event tap (`pm-hook`) at
//! [`pm_hook::priority::KEYBOARD_MANAGER`], ahead of every module that
//! matches shortcuts.
//!
//! - Key down/up and flagsChanged events go through the [`Engine`]. A 1:1
//!   remap (CapsLock -> Esc, Option -> Command) rewrites the event in place
//!   (keycode + flags); anything else swallows the event and replaces it
//!   (`Verdict::Replace`): the replacements reach the lower-priority
//!   subscribers and are then posted right after the tap, so ordering with
//!   later hardware events is preserved.
//! - While remaps make the OS-visible modifiers differ from the physical
//!   ones, the modifier flags of every passing keyboard and mouse-button
//!   event are rewritten to match (apps read modifiers from event flags).
//! - Events we post carry [`INJECTED_TAG`] in `kCGEventSourceUserData` (from a
//!   private event source, so physical modifier state doesn't leak into
//!   them) and are skipped by the tap, as are Mouse Without Borders' events.
//! - CapsLock only produces one flagsChanged per toggle, so it reaches the
//!   engine as a press+release pair. When it is remapped the caps-lock state
//!   it toggled is switched back off via `IOHIDSetModifierLockState`.

#![allow(non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms)]

use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::{c_char, c_ulong, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use pm_input::vk::{self, mac};

use crate::engine::{Action, Engine, Outcome};
use crate::keys::is_modifier;
use crate::{Shared, INJECTED_TAG, MWB_TAG};

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;

    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num: isize,
        key_cb: *const c_void,
        value_cb: *const c_void,
    ) -> CFDictionaryRef;
    fn CFDictionaryGetValue(d: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFArrayGetCount(a: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> *const c_void;
    fn CFNumberGetValue(n: *const c_void, ty: isize, out: *mut c_void) -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGWindowLayer: CFStringRef;
    static kCGWindowOwnerPID: CFStringRef;

    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetType(event: CGEventRef, ty: u32);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: c_ulong, s: *const u16);
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventSourceSetUserData(source: CGEventSourceRef, data: i64);
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, secs: f64);
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
    fn IOServiceGetMatchingService(main_port: u32, matching: CFDictionaryRef) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, ty: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(obj: u32) -> i32;
    fn IOHIDGetModifierLockState(handle: u32, selector: i32, state: *mut bool) -> i32;
    fn IOHIDSetModifierLockState(handle: u32, selector: i32, state: bool) -> i32;
}

extern "C" {
    static mach_task_self_: u32;
}

const kCGHIDEventTap: u32 = 0;
const kCGEventSourceStatePrivate: i32 = -1;

const kCGEventLeftMouseDown: u32 = 1;
const kCGEventRightMouseDown: u32 = 3;
const kCGEventKeyDown: u32 = 10;
const kCGEventKeyUp: u32 = 11;
const kCGEventFlagsChanged: u32 = 12;
const kCGEventScrollWheel: u32 = 22;
const kCGEventOtherMouseDown: u32 = 25;

const kCGKeyboardEventKeycode: u32 = 9;
const kCGEventSourceUnixProcessID: u32 = 41;
const kCGEventSourceUserData: u32 = 42;

const kCGEventFlagMaskAlphaShift: u64 = 0x0001_0000;
const kCGEventFlagMaskShift: u64 = 0x0002_0000;
const kCGEventFlagMaskControl: u64 = 0x0004_0000;
const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
const kCGEventFlagMaskCommand: u64 = 0x0010_0000;
const kCGEventFlagMaskNumericPad: u64 = 0x0020_0000;
const kCGEventFlagMaskSecondaryFn: u64 = 0x0080_0000;
const kCGEventFlagMaskNonCoalesced: u64 = 0x0000_0100;
const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

const kCGWindowListOptionOnScreenOnly: u32 = 1;
const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
const kCFNumberSInt64Type: isize = 4;
const kIOHIDParamConnectType: u32 = 1;
const kIOHIDCapsLockState: i32 = 1;

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

// ---------------------------------------------------------------------------
// Modifier flags
// ---------------------------------------------------------------------------

/// (sided VK, generic flag, device flag)
const MOD_BITS: [(u32, u64, u64); 8] = [
    (vk::VK_LSHIFT, kCGEventFlagMaskShift, NX_DEVICELSHIFTKEYMASK),
    (vk::VK_RSHIFT, kCGEventFlagMaskShift, NX_DEVICERSHIFTKEYMASK),
    (
        vk::VK_LCONTROL,
        kCGEventFlagMaskControl,
        NX_DEVICELCTLKEYMASK,
    ),
    (
        vk::VK_RCONTROL,
        kCGEventFlagMaskControl,
        NX_DEVICERCTLKEYMASK,
    ),
    (
        vk::VK_LMENU,
        kCGEventFlagMaskAlternate,
        NX_DEVICELALTKEYMASK,
    ),
    (
        vk::VK_RMENU,
        kCGEventFlagMaskAlternate,
        NX_DEVICERALTKEYMASK,
    ),
    (vk::VK_LWIN, kCGEventFlagMaskCommand, NX_DEVICELCMDKEYMASK),
    (vk::VK_RWIN, kCGEventFlagMaskCommand, NX_DEVICERCMDKEYMASK),
];

const GENERIC_MODS: u64 = kCGEventFlagMaskShift
    | kCGEventFlagMaskControl
    | kCGEventFlagMaskAlternate
    | kCGEventFlagMaskCommand;
const DEVICE_MODS: u64 = NX_DEVICELCTLKEYMASK
    | NX_DEVICERCTLKEYMASK
    | NX_DEVICELSHIFTKEYMASK
    | NX_DEVICERSHIFTKEYMASK
    | NX_DEVICELALTKEYMASK
    | NX_DEVICERALTKEYMASK
    | NX_DEVICELCMDKEYMASK
    | NX_DEVICERCMDKEYMASK;

fn device_bit(vk: u32) -> Option<u64> {
    MOD_BITS.iter().find(|m| m.0 == vk).map(|m| m.2)
}

fn mods_differ(physical: &BTreeSet<u32>, os: &BTreeSet<u32>) -> bool {
    MOD_BITS
        .iter()
        .any(|&(vk, _, _)| physical.contains(&vk) != os.contains(&vk))
}

/// Rewrite the modifier bits of `flags` (which reflect the physical keyboard)
/// so they reflect `os`: modifiers we suppressed are removed, modifiers we
/// injected are added; anything we don't know about is left alone.
fn adjust_flags(flags: u64, physical: &BTreeSet<u32>, os: &BTreeSet<u32>) -> u64 {
    let mut f = flags;
    let mut touched = 0u64;
    for &(vk, generic, dev) in &MOD_BITS {
        let (p, o) = (physical.contains(&vk), os.contains(&vk));
        if p && !o {
            f &= !dev;
            touched |= generic;
        } else if o && !p {
            f |= dev;
            touched |= generic;
        }
    }
    for generic in [
        kCGEventFlagMaskShift,
        kCGEventFlagMaskControl,
        kCGEventFlagMaskAlternate,
        kCGEventFlagMaskCommand,
    ] {
        let devs: u64 = MOD_BITS
            .iter()
            .filter(|m| m.1 == generic)
            .map(|m| m.2)
            .sum();
        if f & devs != 0 {
            f |= generic;
        } else if touched & generic != 0 {
            f &= !generic;
        }
    }
    f
}

/// Extra flags macOS puts on events of some keys (arrows, F-keys, keypad).
fn key_extra_flags(keycode: u16) -> u64 {
    use mac::*;
    match keycode {
        LEFT_ARROW | RIGHT_ARROW | UP_ARROW | DOWN_ARROW => {
            kCGEventFlagMaskNumericPad | kCGEventFlagMaskSecondaryFn
        }
        HOME | END | PAGE_UP | PAGE_DOWN | FORWARD_DELETE | HELP | F1 | F2 | F3 | F4 | F5 | F6
        | F7 | F8 | F9 | F10 | F11 | F12 | F13 | F14 | F15 | F16 | F17 | F18 | F19 | F20 => {
            kCGEventFlagMaskSecondaryFn
        }
        KEYPAD_0 | KEYPAD_1 | KEYPAD_2 | KEYPAD_3 | KEYPAD_4 | KEYPAD_5 | KEYPAD_6 | KEYPAD_7
        | KEYPAD_8 | KEYPAD_9 | KEYPAD_DECIMAL | KEYPAD_MULTIPLY | KEYPAD_PLUS | KEYPAD_CLEAR
        | KEYPAD_DIVIDE | KEYPAD_ENTER | KEYPAD_MINUS | KEYPAD_EQUALS => kCGEventFlagMaskNumericPad,
        _ => 0,
    }
}

fn mac_keycode(vk: u32) -> Option<u16> {
    vk::vk_to_mac(vk, vk::is_extended_vk(vk))
}

// ---------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------

/// Private-state event source tagged with [`INJECTED_TAG`].
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

/// Where posted events go: from the tap handler they become the
/// replacement of the current event (posted right after the tap, after the
/// lower-priority subscribers saw them); otherwise they are posted at the
/// HID level.
pub(crate) enum Sink<'a> {
    Hid,
    Tap(&'a mut Vec<pm_hook::Emit>),
}

unsafe fn post(sink: &mut Sink<'_>, ev: CGEventRef) {
    if ev.is_null() {
        return;
    }
    // A private source's user data isn't always copied into the event.
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

unsafe fn post_key(sink: &mut Sink<'_>, keycode: u16, down: bool, modifier: bool, flags: u64) {
    let ev = CGEventCreateKeyboardEvent(source(), keycode, down);
    if ev.is_null() {
        return;
    }
    if modifier {
        CGEventSetType(ev, kCGEventFlagsChanged);
        CGEventSetFlags(ev, flags | kCGEventFlagMaskNonCoalesced);
    } else {
        CGEventSetFlags(
            ev,
            flags | key_extra_flags(keycode) | kCGEventFlagMaskNonCoalesced,
        );
    }
    post(sink, ev);
}

/// Type `text` with unicode keyboard events (no modifiers). `\n` presses
/// Return and `\t` Tab, so it works in terminals and forms too.
unsafe fn post_text(sink: &mut Sink<'_>, text: &str) {
    const CHUNK: usize = 20; // CGEventKeyboardSetUnicodeString limit
    let mut buf: Vec<u16> = Vec::new();
    let flush = |sink: &mut Sink<'_>, buf: &mut Vec<u16>| {
        if buf.is_empty() {
            return;
        }
        for down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(source(), 0, down);
            if !ev.is_null() {
                CGEventKeyboardSetUnicodeString(ev, buf.len() as c_ulong, buf.as_ptr());
                CGEventSetFlags(ev, kCGEventFlagMaskNonCoalesced);
                post(sink, ev);
            }
        }
        buf.clear();
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let special = match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                Some(mac::RETURN)
            }
            '\n' => Some(mac::RETURN),
            '\t' => Some(mac::TAB),
            _ => None,
        };
        if let Some(k) = special {
            flush(sink, &mut buf);
            post_key(sink, k, true, false, 0);
            post_key(sink, k, false, false, 0);
            continue;
        }
        let mut units = [0u16; 2];
        let units = c.encode_utf16(&mut units);
        if buf.len() + units.len() > CHUNK {
            flush(sink, &mut buf);
        }
        buf.extend_from_slice(units);
    }
    flush(sink, &mut buf);
}

/// Perform engine actions. `flags_in` / `physical` describe the physical
/// state (for computing modifier flags), `os` the OS state before the first
/// action, `keep` non-modifier flags to carry over (caps lock).
#[allow(clippy::too_many_arguments)]
unsafe fn perform(
    shared: Option<&Shared>,
    sink: &mut Sink<'_>,
    acts: &[Action],
    flags_in: u64,
    keep: u64,
    physical: &BTreeSet<u32>,
    mut os: BTreeSet<u32>,
) {
    for a in acts {
        match a {
            Action::Key { vk: k, down } => {
                if *down {
                    os.insert(*k);
                } else {
                    os.remove(k);
                }
                if *k == vk::VK_CAPITAL {
                    if *down {
                        toggle_caps_lock();
                    }
                    continue;
                }
                let Some(kc) = mac_keycode(*k) else { continue }; // e.g. VK_DUMMY
                let mods = adjust_flags(flags_in, physical, &os) & (GENERIC_MODS | DEVICE_MODS);
                post_key(sink, kc, *down, is_modifier(*k), mods | keep);
            }
            Action::Text(t) => post_text(sink, t),
            other => {
                if let Some(s) = shared {
                    s.submit(other);
                }
            }
        }
    }
}

/// Inject actions from outside the tap (used when stopping).
pub fn inject(acts: &[Action]) -> Result<()> {
    if !unsafe { CGPreflightPostEventAccess() } {
        bail!("not permitted to post events (Accessibility)");
    }
    // Keys being released are, by definition, down at the OS level.
    let os: BTreeSet<u32> = acts
        .iter()
        .filter_map(|a| match a {
            Action::Key { vk, down: false } => Some(*vk),
            _ => None,
        })
        .collect();
    // Flags come purely from the keys still down (the HID system state would
    // include modifiers we posted ourselves).
    unsafe { perform(None, &mut Sink::Hid, acts, 0, 0, &BTreeSet::new(), os) };
    Ok(())
}

/// Synthesize Cmd+V.
pub fn send_paste() -> Result<()> {
    if !unsafe { CGPreflightPostEventAccess() } {
        bail!("not permitted to post events: grant Accessibility in System Settings > Privacy & Security");
    }
    let cmd = kCGEventFlagMaskCommand | NX_DEVICELCMDKEYMASK;
    unsafe {
        let p = &mut Sink::Hid;
        post_key(p, mac::COMMAND, true, true, cmd);
        post_key(p, mac::ANSI_V, true, false, cmd);
        post_key(p, mac::ANSI_V, false, false, cmd);
        post_key(p, mac::COMMAND, false, true, 0);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Caps lock state (IOKit)
// ---------------------------------------------------------------------------

unsafe fn with_hid_system<T>(f: impl FnOnce(u32) -> Option<T>) -> Option<T> {
    let service = IOServiceGetMatchingService(0, IOServiceMatching(c"IOHIDSystem".as_ptr()));
    if service == 0 {
        return None;
    }
    let mut conn = 0u32;
    let kr = IOServiceOpen(service, mach_task_self_, kIOHIDParamConnectType, &mut conn);
    IOObjectRelease(service);
    if kr != 0 {
        log::debug!("keyboard-manager: IOServiceOpen(IOHIDSystem) failed: {kr:#x}");
        return None;
    }
    let r = f(conn);
    IOServiceClose(conn);
    r
}

/// Current caps-lock state, if readable.
pub fn caps_lock_state() -> Option<bool> {
    unsafe {
        with_hid_system(|c| {
            let mut on = false;
            (IOHIDGetModifierLockState(c, kIOHIDCapsLockState, &mut on) == 0).then_some(on)
        })
    }
}

fn set_caps_lock(on: bool) -> bool {
    unsafe {
        with_hid_system(|c| {
            (IOHIDSetModifierLockState(c, kIOHIDCapsLockState, on) == 0).then_some(())
        })
        .is_some()
    }
}

fn toggle_caps_lock() {
    if let Some(on) = caps_lock_state() {
        set_caps_lock(!on);
    }
}

// ---------------------------------------------------------------------------
// Foreground app
// ---------------------------------------------------------------------------

unsafe fn dict_i64(d: *const c_void, key: CFStringRef) -> Option<i64> {
    let n = CFDictionaryGetValue(d, key);
    if n.is_null() {
        return None;
    }
    let mut v: i64 = 0;
    (CFNumberGetValue(n, kCFNumberSInt64Type, &mut v as *mut i64 as *mut c_void) != 0).then_some(v)
}

/// Pid of the owner of the front-most normal (layer 0) window.
fn frontmost_pid() -> Option<i32> {
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

/// Bundle id, localized name and executable name of the frontmost app.
pub fn foreground_app() -> Vec<String> {
    use objc2_app_kit::NSRunningApplication;
    let Some(pid) = frontmost_pid() else {
        return Vec::new();
    };
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

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

unsafe fn ax_copy(el: CFTypeRef, attr: &std::ffi::CStr) -> Option<CFTypeRef> {
    let name = CFStringCreateWithCString(std::ptr::null(), attr.as_ptr(), kCFStringEncodingUTF8);
    if name.is_null() {
        return None;
    }
    let mut out: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(el, name, &mut out);
    CFRelease(name);
    (err == 0 && !out.is_null()).then_some(out)
}

/// Copy a string attribute and release it.
unsafe fn ax_string(el: CFTypeRef, attr: &std::ffi::CStr) -> Option<String> {
    let v = ax_copy(el, attr)?;
    let mut res = None;
    if CFGetTypeID(v) == CFStringGetTypeID() {
        let mut buf = [0 as c_char; 128];
        if CFStringGetCString(
            v,
            buf.as_mut_ptr(),
            buf.len() as isize,
            kCFStringEncodingUTF8,
        ) != 0
        {
            res = Some(
                std::ffi::CStr::from_ptr(buf.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    CFRelease(v);
    res
}

/// `AXRole` of the frontmost app's focused UI element; the `AXSubrole`
/// instead when that is a text role (e.g. `AXSearchField`). `None` when it
/// can't be determined (no permission, no focused element, timeout).
pub fn focused_role() -> Option<String> {
    unsafe {
        // The system-wide element answers for whichever app has keyboard
        // focus (always current, unlike window-list heuristics).
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() {
            return None;
        }
        // Called from the event tap: never wait long for a busy app.
        AXUIElementSetMessagingTimeout(sys, 0.15);
        let mut focused = ax_copy(sys, c"AXFocusedUIElement");
        CFRelease(sys);
        if focused.is_none() {
            // Fall back to asking the front-most window's app directly.
            let app = AXUIElementCreateApplication(frontmost_pid()?);
            if app.is_null() {
                return None;
            }
            AXUIElementSetMessagingTimeout(app, 0.15);
            focused = ax_copy(app, c"AXFocusedUIElement");
            CFRelease(app);
        }
        let focused = focused?;
        let role = ax_string(focused, c"AXRole");
        let sub = ax_string(focused, c"AXSubrole");
        CFRelease(focused);
        match sub {
            Some(s) if crate::windows_style::is_text_role(&s) => Some(s),
            _ => role,
        }
    }
}

// ---------------------------------------------------------------------------
// Permission
// ---------------------------------------------------------------------------

pub fn has_permission(prompt: bool) -> bool {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let val = if prompt {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        };
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &val,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks as *const c_void,
            &raw const kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        let ok = AXIsProcessTrustedWithOptions(dict) != 0;
        if !dict.is_null() {
            CFRelease(dict);
        }
        ok
    }
}

// ---------------------------------------------------------------------------
// Event tap (a pm-hook subscriber)
// ---------------------------------------------------------------------------

struct TapCtx {
    shared: Arc<Shared>,
    /// Set after we switched caps lock back off; the resulting flagsChanged
    /// (if any) is swallowed.
    caps_echo: Cell<Option<Instant>>,
    /// Last time a hidutil re-apply was requested (rate limit).
    hid_reapply: Cell<Option<Instant>>,
}

/// The subscription; dropping it unsubscribes.
pub struct Hook {
    _guard: pm_hook::Guard,
}

pub fn start(shared: Arc<Shared>) -> Result<Hook> {
    use pm_hook::{EventMask, Subscription, Verdict};
    let mask = EventMask::KEYS | EventMask::MOUSE_BUTTONS | EventMask::DRAGS | EventMask::SCROLL;
    let _ = source();
    let ctx = TapCtx { shared, caps_echo: Cell::new(None), hid_reapply: Cell::new(None) };
    let guard = pm_hook::subscribe(
        Subscription::new("keyboard-manager", pm_hook::priority::KEYBOARD_MANAGER, mask, move |ev| {
            let (ty, event) = (ev.cg_type(), ev.cg_event());
            let mut emits = Vec::new();
            let keep = catch_unwind(AssertUnwindSafe(|| unsafe { handle(&ctx, &mut Sink::Tap(&mut emits), ty, event) }))
                .unwrap_or_else(|_| {
                    log::error!("keyboard-manager: event handler panicked");
                    true
                });
            match (keep, emits.is_empty()) {
                (true, true) => Verdict::Pass, // possibly edited in place
                (false, true) => Verdict::Swallow,
                (keep, false) => {
                    if keep {
                        emits.push(pm_hook::Emit::Original);
                    }
                    Verdict::Replace(emits)
                }
            }
        })
        .ignore_tags(&[INJECTED_TAG, MWB_TAG, crate::TEXT_EXPANDER_TAG]),
    );
    if !pm_hook::status().is_active() {
        drop(guard);
        bail!(
            "could not create the keyboard event tap: this process (or the terminal \
             running it) is not trusted for Accessibility. Grant it in System Settings > Privacy & \
             Security > Accessibility and restart it"
        );
    }
    Ok(Hook { _guard: guard })
}

/// Returns whether to keep (deliver) the event.
unsafe fn handle(ctx: &TapCtx, sink: &mut Sink<'_>, ty: u32, event: CGEventRef) -> bool {
    let Ok(mut eng) = ctx.shared.engine.lock() else {
        return true;
    };
    match ty {
        kCGEventKeyDown | kCGEventKeyUp => {
            let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;
            match vk::mac_to_vk(keycode) {
                Some((k, _)) => key_event(ctx, &mut eng, sink, event, k, ty == kCGEventKeyDown),
                None => {
                    fix_flags(&eng, event);
                    true
                }
            }
        }
        kCGEventFlagsChanged => {
            let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;
            let flags = CGEventGetFlags(event);
            if keycode == mac::CAPS_LOCK {
                return caps_event(ctx, &mut eng, sink, event);
            }
            let Some((k, _)) = vk::mac_to_vk(keycode).filter(|(k, _)| device_bit(*k).is_some())
            else {
                fix_flags(&eng, event); // fn key etc.
                return true;
            };
            let dev = device_bit(k).unwrap_or(0);
            let generic = MOD_BITS.iter().find(|m| m.0 == k).map_or(0, |m| m.1);
            let family: u64 = MOD_BITS
                .iter()
                .filter(|m| m.1 == generic)
                .map(|m| m.2)
                .sum();
            // Prefer the device bit; fall back to the generic bit for sources
            // that don't set device bits.
            let down = flags & dev != 0 || (flags & generic != 0 && flags & family == 0);
            key_event(ctx, &mut eng, sink, event, k, down)
        }
        kCGEventLeftMouseDown
        | kCGEventRightMouseDown
        | kCGEventOtherMouseDown
        | kCGEventScrollWheel
            if eng.hyper_pending() =>
        {
            // Hyper + click / scroll: press the Hyper modifiers first.
            let os_before = eng.os_keys().clone();
            let acts = eng.engage_hyper();
            let flags_in = CGEventGetFlags(event);
            let physical = eng.physical_keys().clone();
            perform(
                Some(&ctx.shared),
                sink,
                &acts,
                flags_in,
                flags_in & kCGEventFlagMaskAlphaShift,
                &physical,
                os_before,
            );
            fix_flags(&eng, event);
            true
        }
        _ => {
            fix_flags(&eng, event);
            true
        }
    }
}

/// Make a passing event's modifier flags match the OS view.
unsafe fn fix_flags(eng: &Engine, event: CGEventRef) {
    if mods_differ(eng.physical_keys(), eng.os_keys()) {
        let f = CGEventGetFlags(event);
        CGEventSetFlags(event, adjust_flags(f, eng.physical_keys(), eng.os_keys()));
    }
}

unsafe fn key_event(
    ctx: &TapCtx,
    eng: &mut Engine,
    sink: &mut Sink<'_>,
    event: CGEventRef,
    k: u32,
    down: bool,
) -> bool {
    let flags_in = CGEventGetFlags(event);
    let keep_bits = flags_in & kCGEventFlagMaskAlphaShift;

    // A hardware event's flags tell us which modifiers really are held; drop
    // any whose release we missed (secure input fields hide key events from
    // taps). Only modifiers the OS also has down are checked, since those are
    // the ones the hardware flags reliably report.
    if CGEventGetIntegerValueField(event, kCGEventSourceUnixProcessID) == 0 {
        let stale: Vec<u32> = eng
            .physical_keys()
            .iter()
            .copied()
            .filter(|&p| p != k && eng.os_keys().contains(&p))
            .filter(|&p| device_bit(p).is_some_and(|d| flags_in & d == 0))
            .collect();
        if !stale.is_empty() {
            log::debug!("keyboard-manager: releasing missed modifiers {stale:x?}");
            let os_before = eng.os_keys().clone();
            let acts = eng.release_missing(|p| !stale.contains(&p));
            let physical = eng.physical_keys().clone();
            perform(
                Some(&ctx.shared),
                sink,
                &acts,
                flags_in,
                keep_bits,
                &physical,
                os_before,
            );
        }
    }

    let os_before = eng.os_keys().clone();
    let outcome = eng.process(k, down, &mut foreground_app);
    log::trace!(
        "keyboard-manager: {k:#x} {} -> {outcome:?}",
        if down { "down" } else { "up" }
    );
    let physical = eng.physical_keys().clone();
    match outcome {
        Outcome::Pass => {
            fix_flags(eng, event);
            true
        }
        Outcome::Replace(acts) => {
            // 1:1 key change of the same kind: rewrite in place.
            if let [Action::Key { vk: t, down: d }] = acts.as_slice() {
                if *d == down && is_modifier(*t) == is_modifier(k) && *t != vk::VK_CAPITAL {
                    if let Some(kc) = mac_keycode(*t) {
                        CGEventSetIntegerValueField(event, kCGKeyboardEventKeycode, kc as i64);
                        let mut f = adjust_flags(flags_in, &physical, eng.os_keys());
                        if !is_modifier(*t) {
                            f = (f & !(kCGEventFlagMaskNumericPad | kCGEventFlagMaskSecondaryFn))
                                | key_extra_flags(kc);
                        }
                        CGEventSetFlags(event, f);
                        return true;
                    }
                }
            }
            perform(
                Some(&ctx.shared),
                sink,
                &acts,
                flags_in,
                keep_bits,
                &physical,
                os_before,
            );
            false
        }
    }
}

unsafe fn caps_event(
    ctx: &TapCtx,
    eng: &mut Engine,
    sink: &mut Sink<'_>,
    event: CGEventRef,
) -> bool {
    let flags_in = CGEventGetFlags(event);
    let caps_on = flags_in & kCGEventFlagMaskAlphaShift != 0;
    if eng.hyper_config().is_on() {
        // While the Hyper key is on, Caps Lock arrives as F18. A Caps Lock
        // flagsChanged is either the lock state we toggled (tap action) or a
        // keyboard that doesn't have the hidutil mapping yet (plugged in
        // later): map it, and let this one through.
        if CGEventGetIntegerValueField(event, kCGEventSourceUnixProcessID) == 0
            && ctx
                .hid_reapply
                .get()
                .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            ctx.hid_reapply.set(Some(Instant::now()));
            ctx.shared.submit_job(crate::Job::ReapplyHid);
        }
        fix_flags(eng, event);
        return true;
    }
    if let Some(t) = ctx.caps_echo.take() {
        if t.elapsed() < Duration::from_millis(500) && !caps_on {
            return false; // the flagsChanged caused by switching caps lock back off
        }
    }
    let os_before = eng.os_keys().clone();
    let o1 = eng.process(vk::VK_CAPITAL, true, &mut foreground_app);
    let o2 = eng.process(vk::VK_CAPITAL, false, &mut foreground_app);
    if o1 == Outcome::Pass && o2 == Outcome::Pass {
        fix_flags(eng, event);
        return true;
    }
    let acts: Vec<Action> = [o1, o2]
        .into_iter()
        .flat_map(|o| match o {
            Outcome::Replace(a) => a,
            Outcome::Pass => vec![],
        })
        .collect();
    let physical = eng.physical_keys().clone();
    let base = flags_in & !kCGEventFlagMaskAlphaShift;
    perform(
        Some(&ctx.shared),
        sink,
        &acts,
        base,
        0,
        &physical,
        os_before,
    );
    if caps_on {
        // The press toggled caps lock on underneath us; switch it back off.
        if set_caps_lock(false) {
            ctx.caps_echo.set(Some(Instant::now()));
        } else {
            log::warn!("keyboard-manager: could not reset caps lock state");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_input::vk::*;

    fn set(v: &[u32]) -> BTreeSet<u32> {
        v.iter().copied().collect()
    }

    #[test]
    fn flag_adjustment() {
        let opt = kCGEventFlagMaskAlternate | NX_DEVICELALTKEYMASK;
        let cmd = kCGEventFlagMaskCommand | NX_DEVICELCMDKEYMASK;
        let shift = kCGEventFlagMaskShift | NX_DEVICELSHIFTKEYMASK;
        // Option physically held, OS sees Command (swap)
        let f = adjust_flags(
            opt | kCGEventFlagMaskNumericPad,
            &set(&[VK_LMENU]),
            &set(&[VK_LWIN]),
        );
        assert_eq!(f, cmd | kCGEventFlagMaskNumericPad);
        // Unknown modifiers (held before start) are preserved
        let f = adjust_flags(opt | shift, &set(&[VK_LMENU]), &set(&[VK_LWIN]));
        assert_eq!(f, cmd | shift);
        // Both Option keys held, only left suppressed: generic Alt stays
        let ropt = kCGEventFlagMaskAlternate | NX_DEVICERALTKEYMASK;
        let f = adjust_flags(opt | ropt, &set(&[VK_LMENU, VK_RMENU]), &set(&[VK_RMENU]));
        assert_eq!(f, ropt);
        // no difference -> unchanged
        assert_eq!(adjust_flags(opt, &set(&[VK_LMENU]), &set(&[VK_LMENU])), opt);
        assert!(!mods_differ(&set(&[VK_LMENU, VK_A]), &set(&[VK_LMENU])));
        assert!(mods_differ(&set(&[VK_LMENU]), &set(&[])));
    }

    #[test]
    fn keycodes() {
        assert_eq!(mac_keycode(VK_ESCAPE), Some(mac::ESCAPE));
        assert_eq!(mac_keycode(VK_LWIN), Some(mac::COMMAND));
        assert_eq!(mac_keycode(VK_RMENU), Some(mac::RIGHT_OPTION));
        assert_eq!(mac_keycode(VK_LEFT), Some(mac::LEFT_ARROW));
        assert_eq!(mac_keycode(crate::keys::VK_DUMMY), None);
    }

    #[test]
    fn focused_role_does_not_crash() {
        // Value depends on what is in front (and on Accessibility trust).
        let _ = focused_role();
    }

    #[test]
    fn frontmost_app_lookup_needs_no_permission() {
        // May be empty on a headless machine, but must not crash.
        let ids = foreground_app();
        eprintln!("frontmost app: {ids:?}");
    }

    #[test]
    fn caps_lock_state_readable() {
        eprintln!("caps lock state: {:?}", caps_lock_state());
    }
}
