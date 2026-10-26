//! Hand-written bindings: CoreFoundation, Accessibility, CoreGraphics
//! (event taps, window list, images), libdispatch and a few private
//! SkyLight / CGS functions resolved at runtime with `dlsym`.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_void, CStr};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFMachPortRef = *const c_void;
pub type CFTypeID = usize;
pub type AXUIElementRef = *const c_void;
pub type AXError = i32;
pub type CGEventRef = *const c_void;
pub type CGEventTapProxy = *const c_void;
pub type CGImageRef = *const c_void;
pub type CGColorRef = *const c_void;
pub type CGContextRef = *const c_void;
pub type CGColorSpaceRef = *const c_void;

pub const kAXErrorSuccess: AXError = 0;
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;
pub const kCFNumberDoubleType: isize = 13;

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

impl CGRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
    }
}


// Event taps
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;

pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventFlagsChanged: u32 = 12;
pub const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGEventSourceUserData: u32 = 42;

// Window list
pub const kCGWindowListOptionAll: u32 = 0;
pub const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
pub const kCGWindowListOptionIncludingWindow: u32 = 1 << 3;
pub const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
pub const kCGWindowImageBoundsIgnoreFraming: u32 = 1 << 0;
pub const kCGWindowImageNominalResolution: u32 = 1 << 4;
pub const kCGNullWindowID: u32 = 0;

pub type CGEventTapCallBack =
    extern "C" fn(proxy: CGEventTapProxy, etype: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFBooleanFalse: CFTypeRef;
    pub static kCFTypeArrayCallBacks: c_void;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithBytes(alloc: CFAllocatorRef, bytes: *const u8, len: isize, encoding: u32, ext: u8) -> CFStringRef;
    pub fn CFStringGetLength(s: CFStringRef) -> isize;
    pub fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: isize, encoding: u32) -> u8;
    pub fn CFNumberGetValue(n: CFNumberRef, ty: isize, out: *mut c_void) -> u8;
    pub fn CFNumberCreate(alloc: CFAllocatorRef, ty: isize, value: *const c_void) -> CFNumberRef;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFArrayGetCount(a: CFArrayRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> CFTypeRef;
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFArrayCreate(alloc: CFAllocatorRef, values: *const CFTypeRef, n: isize, cb: *const c_void) -> CFArrayRef;
    pub fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, len: isize) -> CFTypeRef;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFMachPortCreateRunLoopSource(alloc: CFAllocatorRef, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXUIElementGetTypeID() -> CFTypeID;
    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: *mut CFTypeRef) -> AXError;
    pub fn AXUIElementSetAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: CFTypeRef) -> AXError;
    pub fn AXUIElementGetPid(e: AXUIElementRef, pid: *mut i32) -> AXError;
    pub fn AXUIElementSetMessagingTimeout(e: AXUIElementRef, seconds: f32) -> AXError;
    pub fn AXUIElementPerformAction(e: AXUIElementRef, action: CFStringRef) -> AXError;
    /// Private but long-stable SPI used by most window managers.
    pub fn _AXUIElementGetWindow(e: AXUIElementRef, wid: *mut u32) -> AXError;
    /// Deprecated but still exported; needed for the SkyLight focus calls.
    pub fn GetProcessForPID(pid: i32, psn: *mut ProcessSerialNumber) -> i32;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
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
    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
    pub fn CGEventSourceFlagsState(state: i32) -> u64;

    pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    pub fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    pub fn CGDisplayBounds(display: u32) -> CGRect;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;

    pub fn CGImageRelease(img: CGImageRef);
    pub fn CGImageRetain(img: CGImageRef) -> CGImageRef;
    pub fn CGImageGetWidth(img: CGImageRef) -> usize;
    pub fn CGImageGetHeight(img: CGImageRef) -> usize;

    pub fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> CGColorRef;
    pub fn CGColorRelease(c: CGColorRef);

    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(c: CGColorSpaceRef);
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bpc: usize,
        bpr: usize,
        space: CGColorSpaceRef,
        info: u32,
    ) -> CGContextRef;
    pub fn CGBitmapContextCreateImage(ctx: CGContextRef) -> CGImageRef;
    pub fn CGContextRelease(ctx: CGContextRef);
    pub fn CGContextScaleCTM(ctx: CGContextRef, sx: f64, sy: f64);
    pub fn CGContextTranslateCTM(ctx: CGContextRef, tx: f64, ty: f64);
    pub fn CGContextSetRGBFillColor(ctx: CGContextRef, r: f64, g: f64, b: f64, a: f64);
    pub fn CGContextFillRect(ctx: CGContextRef, r: CGRect);
}

extern "C" {
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn proc_pidpath(pid: i32, buf: *mut c_void, size: u32) -> i32;
    pub fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    pub fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
}

pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
pub const RTLD_LAZY: i32 = 1;

/// Look up a (private) symbol in the already-loaded images.
pub fn find_symbol(name: &CStr) -> *mut c_void {
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

/// Load a system framework by path (no-op if already loaded).
pub fn load_framework(path: &CStr) -> bool {
    unsafe { !dlopen(path.as_ptr(), RTLD_LAZY).is_null() }
}

pub const fn event_mask(t: u32) -> u64 {
    1u64 << t
}

/// Owned CF object (released on drop).
pub struct CfOwned(pub CFTypeRef);

impl CfOwned {
    /// Takes ownership of a +1 reference; `None` for null.
    pub fn new(r: CFTypeRef) -> Option<Self> {
        (!r.is_null()).then_some(Self(r))
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Owned CFString created from a Rust str.
pub struct CfString(pub CFStringRef);

impl CfString {
    pub fn new(s: &str) -> Self {
        CfString(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, 0) })
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
///
/// # Safety
/// `s` must be null or a valid CF object.
pub unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
        return None;
    }
    let len = CFStringGetLength(s);
    let cap = CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) + 1;
    let mut buf = vec![0u8; cap.max(1) as usize];
    if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) == 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Value for `key` in a CFDictionary (borrowed).
///
/// # Safety
/// `d` must be a valid CFDictionary.
pub unsafe fn dict_get(d: CFDictionaryRef, key: &str) -> CFTypeRef {
    let k = CfString::new(key);
    CFDictionaryGetValue(d, k.0)
}

/// # Safety
/// `d` must be a valid CFDictionary.
pub unsafe fn dict_i64(d: CFDictionaryRef, key: &str) -> Option<i64> {
    let v = dict_get(d, key);
    if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
        return None;
    }
    let mut out: i64 = 0;
    (CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) != 0).then_some(out)
}

/// # Safety
/// `d` must be a valid CFDictionary.
pub unsafe fn dict_f64(d: CFDictionaryRef, key: &str) -> Option<f64> {
    let v = dict_get(d, key);
    if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
        return None;
    }
    let mut out: f64 = 0.0;
    (CFNumberGetValue(v, kCFNumberDoubleType, &mut out as *mut f64 as *mut c_void) != 0).then_some(out)
}

/// # Safety
/// `d` must be a valid CFDictionary.
pub unsafe fn dict_bool(d: CFDictionaryRef, key: &str) -> Option<bool> {
    let v = dict_get(d, key);
    if v.is_null() {
        return None;
    }
    if CFGetTypeID(v) == CFBooleanGetTypeID() {
        return Some(CFBooleanGetValue(v) != 0);
    }
    dict_i64(d, key).map(|n| n != 0)
}

/// # Safety
/// `d` must be a valid CFDictionary.
pub unsafe fn dict_string(d: CFDictionaryRef, key: &str) -> Option<String> {
    cfstring_to_string(dict_get(d, key))
}

/// Current cursor position (global, top-left origin).
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

/// Run `f` on the main thread via the main dispatch queue (asynchronously).
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(ctx: *mut c_void) {
        let f: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(ctx as *mut Box<dyn FnOnce()>) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    }
    let b: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
    unsafe { dispatch_async_f(&_dispatch_main_q as *const c_void, Box::into_raw(b) as *mut c_void, trampoline) };
}

/// Retained CGImage that can cross threads (CGImage is immutable and
/// thread-safe).
pub struct Image(CGImageRef);

unsafe impl Send for Image {}
unsafe impl Sync for Image {}

impl Image {
    /// Takes ownership of a +1 reference.
    ///
    /// # Safety
    /// `r` must be null or an owned CGImageRef.
    pub unsafe fn from_owned(r: CGImageRef) -> Option<Self> {
        (!r.is_null()).then_some(Self(r))
    }
    /// Retains a borrowed reference.
    ///
    /// # Safety
    /// `r` must be null or a valid CGImageRef.
    pub unsafe fn from_borrowed(r: CGImageRef) -> Option<Self> {
        (!r.is_null()).then(|| Self(CGImageRetain(r)))
    }
    pub fn raw(&self) -> CGImageRef {
        self.0
    }
    pub fn size(&self) -> (f64, f64) {
        unsafe { (CGImageGetWidth(self.0) as f64, CGImageGetHeight(self.0) as f64) }
    }
}

impl Clone for Image {
    fn clone(&self) -> Self {
        Self(unsafe { CGImageRetain(self.0) })
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { CGImageRelease(self.0) }
    }
}

/// Global display bounds (points, top-left origin); the main display first.
pub fn display_bounds() -> Vec<CGRect> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    unsafe {
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) != 0 {
            return vec![];
        }
        let main = CGMainDisplayID();
        let mut v: Vec<(bool, CGRect)> = ids[..n as usize].iter().map(|&d| (d == main, CGDisplayBounds(d))).collect();
        v.sort_by_key(|(m, _)| !*m);
        v.into_iter().map(|(_, r)| r).collect()
    }
}

/// `CGColorRef` with the right Objective-C type encoding for `msg_send!`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgColor(pub CGColorRef);

unsafe impl objc2::encode::Encode for CgColor {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGColor", &[]));
}

/// Owned sRGB `CGColor`.
pub struct OwnedColor(pub CgColor);

impl OwnedColor {
    pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        OwnedColor(CgColor(unsafe { CGColorCreateSRGB(r, g, b, a) }))
    }
    pub fn white(w: f64, a: f64) -> Self {
        Self::rgba(w, w, w, a)
    }
}

impl Drop for OwnedColor {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe { CGColorRelease(self.0 .0) }
        }
    }
}

/// `CGContextRef` with its Objective-C encoding (for `renderInContext:`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgContext(pub CGContextRef);

unsafe impl objc2::encode::Encode for CgContext {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGContext", &[]));
}

/// `CGImageRef` with its Objective-C encoding.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgImage(pub CGImageRef);

unsafe impl objc2::encode::Encode for CgImage {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGImage", &[]));
}
