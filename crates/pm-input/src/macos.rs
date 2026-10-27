//! macOS backend: capture through the shared event tap (`pm-hook`),
//! `CGEventPost` injection.
//!
//! Coordinates are CoreGraphics global display coordinates: origin at the
//! top-left of the primary (menu-bar) display, y grows down, units are
//! *points* (on a Retina display 1 point = 2 physical pixels). Screens, cursor
//! position, captured events and injected events all use the same space.
//!
//! Permissions: an active (filtering) event tap and `CGEventPost` both require
//! the process (or the terminal that launched it) to be trusted for
//! Accessibility. Input Monitoring (`CGPreflightListenEventAccess`) is requested
//! too because macOS may additionally gate keyboard listening on it.
//!
//! The CoreGraphics / CoreFoundation / ApplicationServices C APIs are bound
//! directly below; nothing here needs Objective-C.

#![allow(non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms)]

use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{anyhow, bail, Result};

use crate::vk::{self, mac};
use crate::{
    CaptureCallback, CaptureHandle, Disposition, InputBackend, InputEvent, KeyEvent, MouseButton,
    MouseEvent, Rect, INJECTED_TAG, WHEEL_DELTA,
};

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGDirectDisplayID = u32;
type CGError = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;

    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num: isize,
        key_cb: *const c_void,
        value_cb: *const c_void,
    ) -> CFDictionaryRef;
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        s: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {

    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetType(event: CGEventRef, ty: u32);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        ty: u32,
        pos: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);

    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventSourceSetUserData(source: CGEventSourceRef, data: i64);
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, secs: f64);

    fn CGWarpMouseCursorPosition(pt: CGPoint) -> CGError;
    fn CGAssociateMouseAndMouseCursorPosition(connected: i32) -> CGError;
    fn CGDisplayHideCursor(display: CGDirectDisplayID) -> CGError;
    fn CGDisplayShowCursor(display: CGDirectDisplayID) -> CGError;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGGetActiveDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32)
        -> CGError;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;

    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
}

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
const kCFStringEncodingUTF8: u32 = 0x0800_0100;

// CGEventTapLocation
const kCGHIDEventTap: u32 = 0;
// CGEventSourceStateID
const kCGEventSourceStateHIDSystemState: i32 = 1;

// CGEventType
const kCGEventLeftMouseDown: u32 = 1;
const kCGEventLeftMouseUp: u32 = 2;
const kCGEventRightMouseDown: u32 = 3;
const kCGEventRightMouseUp: u32 = 4;
const kCGEventMouseMoved: u32 = 5;
const kCGEventLeftMouseDragged: u32 = 6;
const kCGEventRightMouseDragged: u32 = 7;
const kCGEventKeyDown: u32 = 10;
const kCGEventKeyUp: u32 = 11;
const kCGEventFlagsChanged: u32 = 12;
const kCGEventScrollWheel: u32 = 22;
const kCGEventOtherMouseDown: u32 = 25;
const kCGEventOtherMouseUp: u32 = 26;
const kCGEventOtherMouseDragged: u32 = 27;

// CGEventField
const kCGMouseEventClickState: u32 = 1;
const kCGMouseEventButtonNumber: u32 = 3;
const kCGMouseEventDeltaX: u32 = 4;
const kCGMouseEventDeltaY: u32 = 5;
const kCGKeyboardEventKeycode: u32 = 9;
const kCGScrollWheelEventDeltaAxis1: u32 = 11;
const kCGScrollWheelEventDeltaAxis2: u32 = 12;
const kCGScrollWheelEventIsContinuous: u32 = 88;
const kCGScrollWheelEventFixedPtDeltaAxis1: u32 = 93;
const kCGScrollWheelEventFixedPtDeltaAxis2: u32 = 94;

// CGMouseButton
const kCGMouseButtonLeft: u32 = 0;
const kCGMouseButtonRight: u32 = 1;
const kCGMouseButtonCenter: u32 = 2;

// CGScrollEventUnit
const kCGScrollEventUnitLine: u32 = 1;

// CGEventFlags
const kCGEventFlagMaskAlphaShift: u64 = 0x0001_0000;
const kCGEventFlagMaskShift: u64 = 0x0002_0000;
const kCGEventFlagMaskControl: u64 = 0x0004_0000;
const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
const kCGEventFlagMaskCommand: u64 = 0x0010_0000;
const kCGEventFlagMaskNumericPad: u64 = 0x0020_0000;
const kCGEventFlagMaskSecondaryFn: u64 = 0x0080_0000;
const kCGEventFlagMaskNonCoalesced: u64 = 0x0000_0100;
// Device-dependent modifier bits (IOKit NX_DEVICE*KEYMASK)
const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

/// (generic flag, device flag, all device flags of that family) for a modifier keycode.
fn modifier_bits(keycode: u16) -> Option<(u64, u64, u64)> {
    let shift = NX_DEVICELSHIFTKEYMASK | NX_DEVICERSHIFTKEYMASK;
    let ctl = NX_DEVICELCTLKEYMASK | NX_DEVICERCTLKEYMASK;
    let alt = NX_DEVICELALTKEYMASK | NX_DEVICERALTKEYMASK;
    let cmd = NX_DEVICELCMDKEYMASK | NX_DEVICERCMDKEYMASK;
    Some(match keycode {
        mac::SHIFT => (kCGEventFlagMaskShift, NX_DEVICELSHIFTKEYMASK, shift),
        mac::RIGHT_SHIFT => (kCGEventFlagMaskShift, NX_DEVICERSHIFTKEYMASK, shift),
        mac::CONTROL => (kCGEventFlagMaskControl, NX_DEVICELCTLKEYMASK, ctl),
        mac::RIGHT_CONTROL => (kCGEventFlagMaskControl, NX_DEVICERCTLKEYMASK, ctl),
        mac::OPTION => (kCGEventFlagMaskAlternate, NX_DEVICELALTKEYMASK, alt),
        mac::RIGHT_OPTION => (kCGEventFlagMaskAlternate, NX_DEVICERALTKEYMASK, alt),
        mac::COMMAND => (kCGEventFlagMaskCommand, NX_DEVICELCMDKEYMASK, cmd),
        mac::RIGHT_COMMAND => (kCGEventFlagMaskCommand, NX_DEVICERCMDKEYMASK, cmd),
        _ => return None,
    })
}

/// Extra flags macOS itself puts on non-modifier key events of some keys.
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

/// Pointer wrapper so CF objects can live in Send/Sync structs. All CF objects
/// used here are only mutated under a Mutex or are thread-safe (CFRunLoopStop).
#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

const DOUBLE_CLICK_SECS: f64 = 0.5;
const DOUBLE_CLICK_DIST: f64 = 4.0;

#[derive(Default)]
struct ClickTrack {
    button: Option<MouseButton>,
    at: Option<Instant>,
    pos: CGPoint,
    count: i64,
}

struct InjectState {
    source: SendPtr,
    /// Mouse buttons currently held by injection (bit per `button_index`).
    buttons: u8,
    /// Modifier keycodes currently held by injection.
    held_mods: Vec<u16>,
    caps_on: bool,
    click: ClickTrack,
    wheel_rem: i32,
    hwheel_rem: i32,
}

impl InjectState {
    fn mod_flags(&self) -> u64 {
        let mut f = kCGEventFlagMaskNonCoalesced;
        for &k in &self.held_mods {
            if let Some((generic, dev, _)) = modifier_bits(k) {
                f |= generic | dev;
            }
        }
        if self.caps_on {
            f |= kCGEventFlagMaskAlphaShift;
        }
        f
    }
}

pub struct MacBackend {
    inject: Mutex<InjectState>,
    cursor_hidden: Mutex<bool>,
    locked: AtomicBool,
    post_ok: AtomicBool,
}

// SAFETY: the only raw pointer (event source) is used under the `inject` mutex.
unsafe impl Send for MacBackend {}
unsafe impl Sync for MacBackend {}

impl MacBackend {
    pub fn new() -> Result<Self> {
        let source = unsafe { CGEventSourceCreate(kCGEventSourceStateHIDSystemState) };
        if source.is_null() {
            bail!("CGEventSourceCreate failed");
        }
        unsafe {
            CGEventSourceSetUserData(source, INJECTED_TAG as i64);
            // Without this, injected events suppress local hardware events for 0.25s.
            CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0);
        }
        Ok(Self {
            inject: Mutex::new(InjectState {
                source: SendPtr(source),
                buttons: 0,
                held_mods: Vec::new(),
                caps_on: false,
                click: ClickTrack::default(),
                wheel_rem: 0,
                hwheel_rem: 0,
            }),
            cursor_hidden: Mutex::new(false),
            locked: AtomicBool::new(false),
            post_ok: AtomicBool::new(false),
        })
    }

    /// `CGEventPost` silently drops events when the process lacks Accessibility
    /// trust; surface that as an error instead. Once granted it stays cached.
    fn ensure_post_access(&self) -> Result<()> {
        if self.post_ok.load(Ordering::Relaxed) {
            return Ok(());
        }
        if unsafe { CGPreflightPostEventAccess() } {
            self.post_ok.store(true, Ordering::Relaxed);
            return Ok(());
        }
        bail!("not permitted to post events: grant Accessibility in System Settings > Privacy & Security")
    }

    fn post(ev: CGEventRef) -> Result<()> {
        if ev.is_null() {
            bail!("failed to create CGEvent");
        }
        unsafe {
            CGEventPost(kCGHIDEventTap, ev);
            CFRelease(ev as CFTypeRef);
        }
        Ok(())
    }

    fn current_location() -> CGPoint {
        unsafe {
            let ev = CGEventCreate(std::ptr::null_mut());
            if ev.is_null() {
                return CGPoint::default();
            }
            let p = CGEventGetLocation(ev);
            CFRelease(ev as CFTypeRef);
            p
        }
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        if let Ok(st) = self.inject.lock() {
            unsafe { CFRelease(st.source.0 as CFTypeRef) };
        }
        if self.locked.load(Ordering::SeqCst) {
            unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        }
        if let Ok(h) = self.cursor_hidden.lock() {
            if *h {
                unsafe { CGDisplayShowCursor(CGMainDisplayID()) };
            }
        }
    }
}

fn button_index(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::X1 => 3,
        MouseButton::X2 => 4,
    }
}

/// (down type, up type, CGMouseButton number)
fn button_types(b: MouseButton) -> (u32, u32, u32) {
    match b {
        MouseButton::Left => (kCGEventLeftMouseDown, kCGEventLeftMouseUp, kCGMouseButtonLeft),
        MouseButton::Right => (kCGEventRightMouseDown, kCGEventRightMouseUp, kCGMouseButtonRight),
        MouseButton::Middle => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGMouseButtonCenter),
        MouseButton::X1 => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 3),
        MouseButton::X2 => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 4),
    }
}

/// Convert a WHEEL_DELTA amount (plus carried remainder) into whole lines.
fn wheel_to_lines(delta: i32, rem: &mut i32) -> i32 {
    let total = *rem + delta;
    let lines = total / WHEEL_DELTA;
    *rem = total - lines * WHEEL_DELTA;
    lines
}

impl InputBackend for MacBackend {
    fn start_capture(&self, cb: CaptureCallback) -> Result<Box<dyn CaptureHandle>> {
        start_tap(cb)
    }

    fn inject_mouse(&self, ev: MouseEvent) -> Result<()> {
        self.ensure_post_access()?;
        let mut st = self.inject.lock().map_err(|_| anyhow!("inject state poisoned"))?;
        let src = st.source.0;
        let flags = st.mod_flags();
        match ev {
            MouseEvent::Move { x, y, .. } => {
                let pos = CGPoint { x: x as f64, y: y as f64 };
                let cur = Self::current_location();
                // A move while a button is held must be a *drag* or apps won't see it.
                let (ty, btn) = if st.buttons & 1 != 0 {
                    (kCGEventLeftMouseDragged, kCGMouseButtonLeft)
                } else if st.buttons & 2 != 0 {
                    (kCGEventRightMouseDragged, kCGMouseButtonRight)
                } else if st.buttons & 0b11100 != 0 {
                    let n = (2..5).find(|i| st.buttons & (1 << i) != 0).unwrap_or(2);
                    (kCGEventOtherMouseDragged, n as u32)
                } else {
                    (kCGEventMouseMoved, kCGMouseButtonLeft)
                };
                unsafe {
                    let e = CGEventCreateMouseEvent(src, ty, pos, btn);
                    if !e.is_null() {
                        CGEventSetIntegerValueField(e, kCGMouseEventDeltaX, (pos.x - cur.x) as i64);
                        CGEventSetIntegerValueField(e, kCGMouseEventDeltaY, (pos.y - cur.y) as i64);
                        if ty == kCGEventOtherMouseDragged {
                            CGEventSetIntegerValueField(e, kCGMouseEventButtonNumber, btn as i64);
                        }
                        CGEventSetFlags(e, flags);
                    }
                    Self::post(e)
                }
            }
            MouseEvent::Button { button, down, x, y } => {
                let pos = CGPoint { x: x as f64, y: y as f64 };
                let (dty, uty, num) = button_types(button);
                let bit = 1u8 << button_index(button);
                if down {
                    st.buttons |= bit;
                    let now = Instant::now();
                    let c = &mut st.click;
                    let repeat = c.button == Some(button)
                        && c.at.is_some_and(|t| now.duration_since(t).as_secs_f64() < DOUBLE_CLICK_SECS)
                        && (c.pos.x - pos.x).abs() <= DOUBLE_CLICK_DIST
                        && (c.pos.y - pos.y).abs() <= DOUBLE_CLICK_DIST;
                    c.count = if repeat { c.count + 1 } else { 1 };
                    c.button = Some(button);
                    c.at = Some(now);
                    c.pos = pos;
                } else {
                    st.buttons &= !bit;
                }
                let count = if st.click.button == Some(button) { st.click.count.max(1) } else { 1 };
                unsafe {
                    let e = CGEventCreateMouseEvent(src, if down { dty } else { uty }, pos, num);
                    if !e.is_null() {
                        CGEventSetIntegerValueField(e, kCGMouseEventClickState, count);
                        CGEventSetIntegerValueField(e, kCGMouseEventButtonNumber, num as i64);
                        CGEventSetFlags(e, flags);
                    }
                    Self::post(e)
                }
            }
            MouseEvent::Wheel { delta } => {
                let lines = wheel_to_lines(delta, &mut st.wheel_rem);
                if lines == 0 {
                    return Ok(());
                }
                unsafe {
                    let e = CGEventCreateScrollWheelEvent2(src, kCGScrollEventUnitLine, 1, lines, 0, 0);
                    if !e.is_null() {
                        CGEventSetFlags(e, flags);
                    }
                    Self::post(e)
                }
            }
            MouseEvent::HWheel { delta } => {
                let lines = wheel_to_lines(delta, &mut st.hwheel_rem);
                if lines == 0 {
                    return Ok(());
                }
                unsafe {
                    // CG axis 2 is positive = left; Windows HWHEEL positive = right.
                    let e = CGEventCreateScrollWheelEvent2(src, kCGScrollEventUnitLine, 2, 0, -lines, 0);
                    if !e.is_null() {
                        CGEventSetFlags(e, flags);
                    }
                    Self::post(e)
                }
            }
        }
    }

    fn inject_key(&self, ev: KeyEvent) -> Result<()> {
        let Some(keycode) = vk::vk_to_mac(ev.vk, ev.extended) else {
            log::debug!("pm-input: no mac keycode for vk {:#x}", ev.vk);
            return Ok(());
        };
        self.ensure_post_access()?;
        let mut st = self.inject.lock().map_err(|_| anyhow!("inject state poisoned"))?;
        let src = st.source.0;
        if modifier_bits(keycode).is_some() {
            if ev.down {
                if !st.held_mods.contains(&keycode) {
                    st.held_mods.push(keycode);
                }
            } else {
                st.held_mods.retain(|&k| k != keycode);
            }
            let flags = st.mod_flags();
            unsafe {
                let e = CGEventCreateKeyboardEvent(src, keycode, ev.down);
                if !e.is_null() {
                    CGEventSetType(e, kCGEventFlagsChanged);
                    CGEventSetFlags(e, flags);
                }
                return Self::post(e);
            }
        }
        if keycode == mac::CAPS_LOCK {
            // Windows sends press+release per toggle; macOS has one flagsChanged
            // per toggle. Best effort: this updates the flags apps see but may not
            // toggle the hardware caps-lock state/LED.
            if !ev.down {
                return Ok(());
            }
            st.caps_on = !st.caps_on;
            let flags = st.mod_flags();
            unsafe {
                let e = CGEventCreateKeyboardEvent(src, keycode, true);
                if !e.is_null() {
                    CGEventSetType(e, kCGEventFlagsChanged);
                    CGEventSetFlags(e, flags);
                }
                return Self::post(e);
            }
        }
        let flags = st.mod_flags() | key_extra_flags(keycode);
        unsafe {
            let e = CGEventCreateKeyboardEvent(src, keycode, ev.down);
            if !e.is_null() {
                CGEventSetFlags(e, flags);
            }
            Self::post(e)
        }
    }

    fn set_cursor_pos(&self, x: i32, y: i32) -> Result<()> {
        let err = unsafe { CGWarpMouseCursorPosition(CGPoint { x: x as f64, y: y as f64 }) };
        if err != 0 {
            bail!("CGWarpMouseCursorPosition failed: {err}");
        }
        // Warping suppresses local mouse input for ~0.25s; re-associating cancels
        // that. While locked we must keep the mouse dissociated instead.
        let connected = if self.locked.load(Ordering::SeqCst) { 0 } else { 1 };
        unsafe { CGAssociateMouseAndMouseCursorPosition(connected) };
        Ok(())
    }

    fn cursor_pos(&self) -> Result<(i32, i32)> {
        let p = Self::current_location();
        Ok((p.x.floor() as i32, p.y.floor() as i32))
    }

    fn set_cursor_visible(&self, visible: bool) -> Result<()> {
        let mut hidden = self.cursor_hidden.lock().map_err(|_| anyhow!("poisoned"))?;
        // CGDisplayHideCursor is reference counted per connection; keep balanced.
        if visible == !*hidden {
            return Ok(());
        }
        unsafe {
            if !visible {
                enable_background_cursor_control();
                let err = CGDisplayHideCursor(CGMainDisplayID());
                if err != 0 {
                    bail!("CGDisplayHideCursor failed: {err}");
                }
            } else {
                let err = CGDisplayShowCursor(CGMainDisplayID());
                if err != 0 {
                    bail!("CGDisplayShowCursor failed: {err}");
                }
            }
        }
        *hidden = !visible;
        Ok(())
    }

    fn screens(&self) -> Result<Vec<Rect>> {
        let mut ids = [0u32; 32];
        let mut n = 0u32;
        let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) };
        if err != 0 {
            bail!("CGGetActiveDisplayList failed: {err}");
        }
        let main = unsafe { CGMainDisplayID() };
        let mut ids: Vec<u32> = ids[..n as usize].to_vec();
        ids.sort_by_key(|&id| id != main); // primary first, stable otherwise
        Ok(ids
            .into_iter()
            .map(|id| {
                let b = unsafe { CGDisplayBounds(id) };
                Rect {
                    left: b.origin.x.round() as i32,
                    top: b.origin.y.round() as i32,
                    right: (b.origin.x + b.size.width).round() as i32,
                    bottom: (b.origin.y + b.size.height).round() as i32,
                }
            })
            .collect())
    }

    fn has_permissions(&self, prompt: bool) -> bool {
        unsafe {
            let key = kAXTrustedCheckOptionPrompt;
            let val = if prompt { kCFBooleanTrue } else { kCFBooleanFalse };
            let dict = CFDictionaryCreate(
                std::ptr::null(),
                &key,
                &val,
                1,
                &raw const kCFTypeDictionaryKeyCallBacks as *const c_void,
                &raw const kCFTypeDictionaryValueCallBacks as *const c_void,
            );
            let ax = AXIsProcessTrustedWithOptions(dict) != 0;
            if !dict.is_null() {
                CFRelease(dict);
            }
            let mut listen = CGPreflightListenEventAccess();
            if !listen && prompt {
                listen = CGRequestListenEventAccess();
            }
            let post = CGPreflightPostEventAccess();
            log::debug!("pm-input: accessibility={ax} listen={listen} post={post}");
            // An active tap + CGEventPost need Accessibility; Input Monitoring
            // alone only permits listen-only taps, so Accessibility is decisive.
            ax
        }
    }

    fn set_cursor_locked(&self, locked: bool) -> Result<()> {
        let err = unsafe { CGAssociateMouseAndMouseCursorPosition(if locked { 0 } else { 1 }) };
        if err != 0 {
            bail!("CGAssociateMouseAndMouseCursorPosition failed: {err}");
        }
        self.locked.store(locked, Ordering::SeqCst);
        Ok(())
    }
}

/// `CGDisplayHideCursor` only takes effect while the calling app is frontmost.
/// A background agent (menu-bar app / daemon controlling another machine) needs
/// the private WindowServer connection property "SetsCursorInBackground" set
/// first. This is the long-standing trick used by Synergy/Deskflow/Barrier. The
/// private symbols are looked up with `dlsym`, so if Apple removes them we just
/// skip it (hiding then only works while frontmost).
unsafe fn enable_background_cursor_control() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    type DefConn = unsafe extern "C" fn() -> i32;
    type SetProp = unsafe extern "C" fn(i32, i32, CFStringRef, CFTypeRef) -> i32;
    let conn = dlsym(RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr());
    let setp = dlsym(RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr());
    if conn.is_null() || setp.is_null() {
        log::warn!("pm-input: CGSSetConnectionProperty unavailable; cursor hide needs foreground");
        return;
    }
    let conn: DefConn = std::mem::transmute(conn);
    let setp: SetProp = std::mem::transmute(setp);
    let key: &CStr = c"SetsCursorInBackground";
    let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), kCFStringEncodingUTF8);
    if key.is_null() {
        return;
    }
    let cid = conn();
    let err = setp(cid, cid, key, kCFBooleanTrue);
    if err != 0 {
        log::warn!("pm-input: SetsCursorInBackground failed: {err}");
    }
    CFRelease(key);
}

// ---------------------------------------------------------------------------
// Capture (a pm-hook subscriber)
// ---------------------------------------------------------------------------

/// Keeps the subscription to the global tap alive.
struct MacCapture {
    _guard: pm_hook::Guard,
}

impl CaptureHandle for MacCapture {}

/// Subscribe to the shared event tap (`pm-hook`) at Mouse Without Borders'
/// priority. Events this crate injected ([`INJECTED_TAG`]) are skipped.
fn start_tap(cb: CaptureCallback) -> Result<Box<dyn CaptureHandle>> {
    use pm_hook::{EventMask, Subscription, Verdict};
    let mask = EventMask::KEYS | EventMask::MOUSE_BUTTONS | EventMask::MOVES | EventMask::SCROLL;
    let wheel_rem = Cell::new(0.0);
    let hwheel_rem = Cell::new(0.0);
    let guard = pm_hook::subscribe(
        Subscription::new("mouse-without-borders", pm_hook::priority::MOUSE_WITHOUT_BORDERS, mask, move |ev| {
            let disp = unsafe { handle_event(&cb, ev.cg_type(), ev.cg_event(), &wheel_rem, &hwheel_rem) };
            match disp {
                Disposition::Pass => Verdict::Pass,
                Disposition::Swallow => Verdict::Swallow,
            }
        })
        .ignore_tags(&[INJECTED_TAG]),
    );
    let st = pm_hook::status();
    if !st.is_active() {
        drop(guard);
        bail!(
            "could not create the event tap ({:?}): the process is not trusted for \
             Accessibility/Input Monitoring (System Settings > Privacy & Security)",
            st.state
        );
    }
    Ok(Box::new(MacCapture { _guard: guard }))
}

fn dispatch(cb: &CaptureCallback, ev: InputEvent) -> Disposition {
    match catch_unwind(AssertUnwindSafe(|| cb(&ev))) {
        Ok(d) => d,
        Err(_) => {
            log::error!("pm-input: capture callback panicked");
            Disposition::Pass
        }
    }
}

fn combine(a: Disposition, b: Disposition) -> Disposition {
    if a == Disposition::Swallow || b == Disposition::Swallow {
        Disposition::Swallow
    } else {
        Disposition::Pass
    }
}

/// Fractional wheel amount (in lines) -> WHEEL_DELTA units with carried remainder.
fn lines_to_wheel(lines: f64, rem: &Cell<f64>) -> i32 {
    let total = rem.get() + lines * WHEEL_DELTA as f64;
    let whole = total.trunc();
    rem.set(total - whole);
    whole as i32
}

/// Translate one tapped event to Windows semantics and ask the callback.
unsafe fn handle_event(
    cb: &CaptureCallback,
    ty: u32,
    event: CGEventRef,
    wheel_rem: &Cell<f64>,
    hwheel_rem: &Cell<f64>,
) -> Disposition {
    if event.is_null() {
        return Disposition::Pass;
    }
    let loc = CGEventGetLocation(event);
    let (x, y) = (loc.x.floor() as i32, loc.y.floor() as i32);

    match ty {
        kCGEventMouseMoved
        | kCGEventLeftMouseDragged
        | kCGEventRightMouseDragged
        | kCGEventOtherMouseDragged => {
            // Deltas keep flowing when the cursor is pinned at a screen edge or
            // dissociated (set_cursor_locked), unlike the absolute location.
            let dx = CGEventGetIntegerValueField(event, kCGMouseEventDeltaX) as i32;
            let dy = CGEventGetIntegerValueField(event, kCGMouseEventDeltaY) as i32;
            dispatch(cb, InputEvent::Mouse(MouseEvent::Move { x, y, dx, dy }))
        }
        kCGEventLeftMouseDown | kCGEventLeftMouseUp | kCGEventRightMouseDown
        | kCGEventRightMouseUp | kCGEventOtherMouseDown | kCGEventOtherMouseUp => {
            let down = matches!(
                ty,
                kCGEventLeftMouseDown | kCGEventRightMouseDown | kCGEventOtherMouseDown
            );
            let button = match ty {
                kCGEventLeftMouseDown | kCGEventLeftMouseUp => Some(MouseButton::Left),
                kCGEventRightMouseDown | kCGEventRightMouseUp => Some(MouseButton::Right),
                _ => match CGEventGetIntegerValueField(event, kCGMouseEventButtonNumber) {
                    2 => Some(MouseButton::Middle),
                    3 => Some(MouseButton::X1),
                    4 => Some(MouseButton::X2),
                    _ => None,
                },
            };
            match button {
                Some(button) => {
                    dispatch(cb, InputEvent::Mouse(MouseEvent::Button { button, down, x, y }))
                }
                None => Disposition::Pass,
            }
        }
        kCGEventScrollWheel => {
            let continuous = CGEventGetIntegerValueField(event, kCGScrollWheelEventIsContinuous) != 0;
            let (v, h) = if continuous {
                // Trackpad / Magic Mouse: fractional lines in 16.16 fixed point.
                let v = CGEventGetIntegerValueField(event, kCGScrollWheelEventFixedPtDeltaAxis1)
                    as f64
                    / 65536.0;
                let h = CGEventGetIntegerValueField(event, kCGScrollWheelEventFixedPtDeltaAxis2)
                    as f64
                    / 65536.0;
                (lines_to_wheel(v, wheel_rem), lines_to_wheel(h, hwheel_rem))
            } else {
                let v = CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis1) as i32;
                let h = CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis2) as i32;
                (v * WHEEL_DELTA, h * WHEEL_DELTA)
            };
            let mut d = Disposition::Pass;
            if v != 0 {
                d = combine(d, dispatch(cb, InputEvent::Mouse(MouseEvent::Wheel { delta: v })));
            }
            if h != 0 {
                // CG axis 2 positive = left; Windows HWHEEL positive = right.
                d = combine(d, dispatch(cb, InputEvent::Mouse(MouseEvent::HWheel { delta: -h })));
            }
            d
        }
        kCGEventKeyDown | kCGEventKeyUp => {
            let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;
            match vk::mac_key_event(keycode, ty == kCGEventKeyDown) {
                Some(k) => dispatch(cb, InputEvent::Key(k)),
                None => Disposition::Pass,
            }
        }
        kCGEventFlagsChanged => {
            let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;
            let flags = CGEventGetFlags(event);
            if let Some((generic, dev, family)) = modifier_bits(keycode) {
                // Prefer the left/right device bit; fall back to the generic bit
                // for synthetic sources that don't set device bits.
                let down = flags & dev != 0 || (flags & generic != 0 && flags & family == 0);
                match vk::mac_key_event(keycode, down) {
                    Some(k) => dispatch(cb, InputEvent::Key(k)),
                    None => Disposition::Pass,
                }
            } else if keycode == mac::CAPS_LOCK {
                // One flagsChanged per toggle -> a Windows-style press + release.
                let d1 = vk::mac_key_event(keycode, true)
                    .map_or(Disposition::Pass, |k| dispatch(cb, InputEvent::Key(k)));
                let d2 = vk::mac_key_event(keycode, false)
                    .map_or(Disposition::Pass, |k| dispatch(cb, InputEvent::Key(k)));
                combine(d1, d2)
            } else {
                Disposition::Pass // fn key etc.
            }
        }
        _ => Disposition::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_accumulates() {
        let mut rem = 0;
        assert_eq!(wheel_to_lines(120, &mut rem), 1);
        assert_eq!(wheel_to_lines(60, &mut rem), 0);
        assert_eq!(wheel_to_lines(60, &mut rem), 1);
        assert_eq!(wheel_to_lines(-240, &mut rem), -2);
        assert_eq!(rem, 0);
    }

    #[test]
    fn fractional_lines() {
        let rem = Cell::new(0.0);
        assert_eq!(lines_to_wheel(0.5, &rem), 60);
        assert_eq!(lines_to_wheel(0.004, &rem), 0);
        assert_eq!(lines_to_wheel(0.004, &rem), 0);
        assert_eq!(lines_to_wheel(0.004, &rem), 1);
    }

    #[test]
    fn screens_and_cursor() {
        // Needs no permissions.
        let b = MacBackend::new().unwrap();
        let s = b.screens().unwrap();
        assert!(!s.is_empty());
        assert_eq!((s[0].left, s[0].top), (0, 0), "primary display is at the origin");
        let _ = b.cursor_pos().unwrap();
    }
}
