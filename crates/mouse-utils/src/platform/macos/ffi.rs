//! Minimal hand-written bindings for CoreFoundation, CoreGraphics (event
//! taps, paths, colors) and libdispatch.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

use objc2::encode::{Encode, Encoding};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFRunLoopTimerRef = *const c_void;
pub type CFMachPortRef = *const c_void;
pub type CGEventRef = *const c_void;
pub type CGEventTapProxy = *const c_void;
pub type CGMutablePathRef = *const c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// `CGColorRef` with the right Objective-C type encoding for `msg_send!`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgColor(pub *const c_void);

unsafe impl Encode for CgColor {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

/// `CGPathRef` with the right Objective-C type encoding for `msg_send!`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgPath(pub *const c_void);

unsafe impl Encode for CgPath {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGPath", &[]));
}

// CGEventTapLocation / placement / options
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;
pub const kCGEventTapOptionListenOnly: u32 = 1;

// CGEventType
pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventRightMouseDown: u32 = 3;
pub const kCGEventRightMouseUp: u32 = 4;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventRightMouseDragged: u32 = 7;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventFlagsChanged: u32 = 12;
pub const kCGEventOtherMouseDown: u32 = 25;
pub const kCGEventOtherMouseDragged: u32 = 27;
pub const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

// CGEventFlags
pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

// Device-dependent modifier bits (IOKit NX_DEVICE*KEYMASK).
pub const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
pub const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
pub const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
pub const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
pub const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
pub const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
pub const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
pub const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

// CGEventField
pub const kCGMouseEventDeltaX: u32 = 4;
pub const kCGMouseEventDeltaY: u32 = 5;
pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGEventSourceUserData: u32 = 42;

pub type CGEventTapCallBack =
    extern "C" fn(proxy: CGEventTapProxy, etype: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef;
pub type CFRunLoopTimerCallBack = extern "C" fn(timer: CFRunLoopTimerRef, info: *mut c_void);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFAbsoluteTimeGetCurrent() -> f64;
    pub fn CFRunLoopTimerCreate(
        alloc: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: CFRunLoopTimerCallBack,
        context: *mut c_void,
    ) -> CFRunLoopTimerRef;
    pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    pub fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    pub fn CGEventGetFlags(e: CGEventRef) -> u64;
    pub fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    pub fn CGEventGetDoubleValueField(e: CGEventRef, field: u32) -> f64;
    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
    pub fn CGPreflightListenEventAccess() -> bool;

    pub fn CGPathCreateMutable() -> CGMutablePathRef;
    pub fn CGPathAddRect(path: CGMutablePathRef, m: *const c_void, rect: CGRect);
    pub fn CGPathAddEllipseInRect(path: CGMutablePathRef, m: *const c_void, rect: CGRect);
    pub fn CGPathRelease(path: CGMutablePathRef);
    pub fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *const c_void;
    pub fn CGColorRelease(c: *const c_void);
}

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {
    pub static kCAFillRuleEvenOdd: &'static objc2_foundation::NSString;
}

extern "C" {
    // libdispatch (libSystem)
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

pub const fn event_mask(t: u32) -> u64 {
    1u64 << t
}

/// Run `f` on the main thread via the main dispatch queue (asynchronously).
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(ctx: *mut c_void) {
        let f: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(ctx as *mut Box<dyn FnOnce()>) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    }
    let b: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
    unsafe { dispatch_async_f(&_dispatch_main_q as *const c_void, Box::into_raw(b) as *mut c_void, trampoline) };
}

/// Owned CGColor.
pub struct OwnedColor(pub CgColor);

impl OwnedColor {
    pub fn new(c: crate::config::Color) -> Self {
        let f = |v: u8| v as f64 / 255.0;
        OwnedColor(CgColor(unsafe { CGColorCreateSRGB(f(c.r), f(c.g), f(c.b), f(c.a)) }))
    }
}

impl Drop for OwnedColor {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe { CGColorRelease(self.0 .0) }
        }
    }
}

/// Current pointer location in global top-left coordinates (no permission needed).
pub fn cursor_location() -> CGPoint {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return CGPoint::default();
        }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        p
    }
}
