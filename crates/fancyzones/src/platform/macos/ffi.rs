//! Minimal hand-written bindings for CoreFoundation, HIServices
//! (Accessibility), CoreGraphics event taps and libdispatch.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFTypeID = usize;
pub type AXUIElementRef = *const c_void;
pub type AXValueRef = *const c_void;
pub type AXError = i32;
pub type CGEventRef = *const c_void;
pub type CGDirectDisplayID = u32;

pub const kAXErrorSuccess: AXError = 0;
pub const kAXErrorAPIDisabled: AXError = -25211;
pub const kAXValueCGPointType: u32 = 1;
pub const kAXValueCGSizeType: u32 = 2;
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

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

// CGEventTapLocation / placement / options

// CGEventType
pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventRightMouseDown: u32 = 3;
pub const kCGEventRightMouseUp: u32 = 4;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventFlagsChanged: u32 = 12;
pub const kCGEventScrollWheel: u32 = 22;
pub const kCGEventOtherMouseDown: u32 = 25;

// CGEventFlags
pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGMouseEventButtonNumber: u32 = 3;
pub const kCGScrollWheelEventDeltaAxis1: u32 = 11;
pub const kCGScrollWheelEventIsContinuous: u32 = 88;
pub const kCGScrollWheelEventPointDeltaAxis1: u32 = 96;
pub const kCGEventSourceStateCombinedSessionState: i32 = 0;

pub const kVK_LeftArrow: i64 = 0x7B;
pub const kVK_RightArrow: i64 = 0x7C;
pub const kVK_DownArrow: i64 = 0x7D;
pub const kVK_UpArrow: i64 = 0x7E;

pub type AXObserverRef = *const c_void;
pub type AXObserverCallback =
    extern "C" fn(observer: AXObserverRef, element: AXUIElementRef, notification: CFStringRef, refcon: *mut c_void);


#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFBooleanFalse: CFTypeRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFHash(cf: CFTypeRef) -> usize;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        is_external: u8,
    ) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: isize, encoding: u32) -> u8;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFArrayGetCount(a: CFTypeRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFUUIDCreateString(alloc: CFAllocatorRef, uuid: CFTypeRef) -> CFStringRef;
    pub fn CFArrayCreate(alloc: CFAllocatorRef, values: *const *const c_void, n: isize, callbacks: *const c_void) -> CFTypeRef;
    pub fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXUIElementGetTypeID() -> CFTypeID;
    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: *mut CFTypeRef) -> AXError;
    pub fn AXUIElementSetAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: CFTypeRef) -> AXError;
    pub fn AXUIElementCopyElementAtPosition(app: AXUIElementRef, x: f32, y: f32, el: *mut AXUIElementRef) -> AXError;
    pub fn AXUIElementGetPid(e: AXUIElementRef, pid: *mut i32) -> AXError;
    pub fn AXUIElementSetMessagingTimeout(e: AXUIElementRef, seconds: f32) -> AXError;
    pub fn AXValueCreate(t: u32, value: *const c_void) -> AXValueRef;
    pub fn AXValueGetValue(v: AXValueRef, t: u32, value: *mut c_void) -> u8;
    pub fn AXValueGetTypeID() -> CFTypeID;
    /// Private but long-stable SPI used by most window managers.
    pub fn _AXUIElementGetWindow(e: AXUIElementRef, wid: *mut u32) -> AXError;
    pub fn AXUIElementPerformAction(e: AXUIElementRef, action: CFStringRef) -> AXError;
    pub fn AXObserverCreate(pid: i32, callback: AXObserverCallback, out: *mut AXObserverRef) -> AXError;
    pub fn AXObserverAddNotification(o: AXObserverRef, e: AXUIElementRef, n: CFStringRef, refcon: *mut c_void) -> AXError;
    pub fn AXObserverRemoveNotification(o: AXObserverRef, e: AXUIElementRef, n: CFStringRef) -> AXError;
    pub fn AXObserverGetRunLoopSource(o: AXObserverRef) -> CFRunLoopSourceRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    pub fn CGEventGetFlags(e: CGEventRef) -> u64;
    pub fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
    pub fn CGEventSourceFlagsState(state: i32) -> u64;
    pub fn CGDisplayVendorNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayModelNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplaySerialNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayIsBuiltin(d: CGDirectDisplayID) -> u32;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGWindowListCreateDescriptionFromArray(windows: CFTypeRef) -> CFTypeRef;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    pub fn CGPreflightListenEventAccess() -> bool;
}

extern "C" {
    // libdispatch (libSystem)
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    // libproc
    pub fn proc_pidpath(pid: i32, buf: *mut c_void, size: u32) -> i32;
    pub fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
}

/// `RTLD_DEFAULT` on macOS.
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// Look up a (private) symbol in the already-loaded images.
pub fn find_symbol(name: &std::ffi::CStr) -> *mut c_void {
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

/// Current cursor position (global, top-left origin).
/// The window server's bounds of window `wid` (global, top-left origin).
/// Unlike the Accessibility frame, it follows a window while the user drags
/// it (macOS can report the old AX position until the drag ends).
pub fn window_server_bounds(wid: u32) -> Option<CGRect> {
    unsafe {
        let ids = [wid as usize as *const c_void];
        let arr = CFArrayCreate(std::ptr::null(), ids.as_ptr(), 1, std::ptr::null());
        if arr.is_null() {
            return None;
        }
        let desc = CGWindowListCreateDescriptionFromArray(arr);
        CFRelease(arr);
        if desc.is_null() {
            return None;
        }
        let mut out = None;
        if CFArrayGetCount(desc) > 0 {
            let d = CFArrayGetValueAtIndex(desc, 0);
            let key = CfString::new("kCGWindowBounds");
            let b = CFDictionaryGetValue(d, key.0);
            let mut r = std::mem::zeroed::<CGRect>();
            if !b.is_null() && CGRectMakeWithDictionaryRepresentation(b, &mut r) {
                out = Some(r);
            }
        }
        CFRelease(desc);
        out
    }
}

pub fn cursor_location() -> Option<CGPoint> {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return None;
        }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        Some(p)
    }
}

pub const fn event_mask(t: u32) -> u64 {
    1u64 << t
}

/// Owned CFString created from a Rust str.
pub struct CfString(pub CFStringRef);

impl CfString {
    pub fn new(s: &str) -> Self {
        let r = unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, 0) };
        CfString(r)
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Read a CFString into a Rust String.
pub unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
        return None;
    }
    let mut buf = vec![0u8; 1024];
    if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) == 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
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
