//! Hand-written bindings: CoreFoundation, CoreGraphics (window list, events,
//! images), Accessibility (HIServices), libdispatch.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2_foundation::NSString;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopTimerRef = *const c_void;
pub type AXUIElementRef = *const c_void;
pub type CGEventRef = *const c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub type CFRunLoopTimerCallBack = extern "C" fn(timer: CFRunLoopTimerRef, info: *mut c_void);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
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
    pub fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, date: f64);
    pub fn CFArrayGetCount(a: CFTypeRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    pub fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFNumberGetValue(n: CFTypeRef, the_type: isize, out: *mut c_void) -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub static kCGWindowNumber: CFStringRef;
    pub static kCGWindowLayer: CFStringRef;
    pub static kCGWindowBounds: CFStringRef;
    pub static kCGWindowOwnerPID: CFStringRef;
    pub static kCGWindowOwnerName: CFStringRef;
    pub static kCGWindowName: CFStringRef;
    pub static kCGWindowIsOnscreen: CFStringRef;
    pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
    pub fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    pub fn CGEventCreateMouseEvent(source: *const c_void, ty: u32, p: CGPoint, button: u32) -> CGEventRef;
    pub fn CGEventSetFlags(e: CGEventRef, flags: u64);
    pub fn CGEventSetIntegerValueField(e: CGEventRef, field: u32, v: i64);
    pub fn CGEventPost(tap: u32, e: CGEventRef);
    pub fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    pub fn CGImageRelease(i: *const c_void);
    pub fn CGImageGetWidth(i: *const c_void) -> usize;
    pub fn CGImageGetHeight(i: *const c_void) -> usize;
}

pub const kCGWindowListOptionAll: u32 = 0;
pub const kCGWindowListOptionOnScreenOnly: u32 = 1;
pub const kCGWindowListOptionIncludingWindow: u32 = 8;
pub const kCGWindowImageBoundsIgnoreFraming: u32 = 1;
pub const kCGWindowImageBestResolution: u32 = 8;
pub const kCGStatusWindowLevel: i64 = 25;
pub const kCGPopUpMenuWindowLevel: i64 = 101;

pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGHIDEventTap: u32 = 0;
pub const kCGMouseEventClickState: u32 = 1;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    pub fn AXUIElementCopyElementAtPosition(el: AXUIElementRef, x: f32, y: f32, out: *mut AXUIElementRef) -> i32;
    pub fn AXUIElementGetPid(el: AXUIElementRef, pid: *mut i32) -> i32;
    pub fn AXUIElementCopyAttributeValue(el: AXUIElementRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
    pub fn AXUIElementPerformAction(el: AXUIElementRef, action: CFStringRef) -> i32;
    pub fn AXUIElementSetMessagingTimeout(el: AXUIElementRef, secs: f32) -> i32;
    pub fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> bool;
}

pub const kAXValueCGPointType: u32 = 1;
pub const kAXValueCGSizeType: u32 = 2;

extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
}

const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// Run `f` on the main thread via the main dispatch queue (asynchronously).
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(ctx: *mut c_void) {
        let f: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(ctx as *mut Box<dyn FnOnce()>) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    }
    let b: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
    unsafe { dispatch_async_f(&_dispatch_main_q as *const c_void, Box::into_raw(b) as *mut c_void, trampoline) };
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn cfstr(s: &NSString) -> CFStringRef {
    s as *const NSString as CFStringRef
}

pub fn now() -> f64 {
    unsafe { CFAbsoluteTimeGetCurrent() }
}

/// A +1 CF reference released on drop.
pub struct Owned(pub CFTypeRef);

impl Owned {
    pub fn new(p: CFTypeRef) -> Option<Owned> {
        (!p.is_null()).then_some(Owned(p))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

/// CFString → String (non-owning).
pub fn cf_to_string(s: CFTypeRef) -> Option<String> {
    if s.is_null() || unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
        return None;
    }
    let ns: &NSString = unsafe { &*(s as *const NSString) };
    Some(ns.to_string())
}

pub fn dict_get(d: CFTypeRef, key: CFStringRef) -> CFTypeRef {
    unsafe { CFDictionaryGetValue(d, key) }
}

pub fn dict_i64(d: CFTypeRef, key: CFStringRef) -> Option<i64> {
    let v = dict_get(d, key);
    if v.is_null() {
        return None;
    }
    let mut out: i64 = 0;
    // kCFNumberSInt64Type = 4
    (unsafe { CFNumberGetValue(v, 4, &mut out as *mut i64 as *mut c_void) } != 0).then_some(out)
}

/// Current pointer position (global, top-left).
pub fn cursor() -> CGPoint {
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

/// Post a mouse event at `p` with `flags`.
pub fn post_mouse(ty: u32, p: CGPoint, flags: u64, clicks: i64) {
    unsafe {
        let e = CGEventCreateMouseEvent(std::ptr::null(), ty, p, 0);
        if e.is_null() {
            return;
        }
        CGEventSetFlags(e, flags);
        if clicks > 0 {
            CGEventSetIntegerValueField(e, kCGMouseEventClickState, clicks);
        }
        CGEventPost(kCGHIDEventTap, e);
        CFRelease(e);
    }
}

type CreateImageFn = unsafe extern "C" fn(CGRect, u32, u32, u32) -> *const c_void;

/// `CGWindowListCreateImage` looked up at run time (it's obsoleted in
/// recent SDKs but still present on older systems).
pub fn window_image(window_id: u32) -> Option<*const c_void> {
    let sym = unsafe { dlsym(RTLD_DEFAULT, c"CGWindowListCreateImage".as_ptr()) };
    if sym.is_null() {
        return None;
    }
    let f: CreateImageFn = unsafe { std::mem::transmute(sym) };
    // CGRectNull: the window's own bounds.
    let null = CGRect { origin: CGPoint { x: f64::INFINITY, y: f64::INFINITY }, size: CGSize::default() };
    let img = unsafe { f(null, kCGWindowListOptionIncludingWindow, window_id, kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution) };
    if img.is_null() {
        return None;
    }
    let ok = unsafe { CGImageGetWidth(img) > 1 && CGImageGetHeight(img) > 1 };
    if !ok {
        unsafe { CGImageRelease(img) };
        return None;
    }
    Some(img)
}

/// AX helpers.
pub mod ax {
    use super::*;

    pub struct El(pub Owned);

    impl El {
        pub fn app(pid: i32, timeout: f32) -> Option<El> {
            let e = Owned::new(unsafe { AXUIElementCreateApplication(pid) })?;
            unsafe { AXUIElementSetMessagingTimeout(e.0, timeout) };
            Some(El(e))
        }
        pub fn ptr(&self) -> AXUIElementRef {
            self.0 .0
        }
        pub fn attr(&self, name: &str) -> Option<Owned> {
            let n = ns(name);
            let mut out: CFTypeRef = std::ptr::null();
            let r = unsafe { AXUIElementCopyAttributeValue(self.ptr(), cfstr(&n), &mut out) };
            if r != 0 {
                return None;
            }
            Owned::new(out)
        }
        pub fn string(&self, name: &str) -> Option<String> {
            self.attr(name).and_then(|v| cf_to_string(v.0)).filter(|s| !s.is_empty())
        }
        pub fn children(&self) -> Vec<El> {
            let Some(arr) = self.attr("AXChildren") else { return vec![] };
            let n = unsafe { CFArrayGetCount(arr.0) };
            (0..n)
                .filter_map(|i| {
                    let v = unsafe { CFArrayGetValueAtIndex(arr.0, i) };
                    (!v.is_null()).then(|| El(Owned(unsafe { CFRetain(v) })))
                })
                .collect()
        }
        pub fn frame(&self) -> Option<CGRect> {
            let p = self.attr("AXPosition")?;
            let s = self.attr("AXSize")?;
            let mut pt = CGPoint::default();
            let mut sz = CGSize::default();
            let ok = unsafe {
                AXValueGetValue(p.0, kAXValueCGPointType, &mut pt as *mut CGPoint as *mut c_void)
                    && AXValueGetValue(s.0, kAXValueCGSizeType, &mut sz as *mut CGSize as *mut c_void)
            };
            ok.then_some(CGRect { origin: pt, size: sz })
        }
        pub fn press(&self) -> bool {
            let a = ns("AXPress");
            unsafe { AXUIElementPerformAction(self.ptr(), cfstr(&a)) == 0 }
        }
    }
}
