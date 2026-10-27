//! Hand-written bindings: CoreFoundation, Accessibility, CoreGraphics and a
//! few private CGS / SkyLight functions resolved at runtime with `dlsym`.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_void, CStr};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFTypeID = usize;
pub type AXUIElementRef = *const c_void;
pub type AXError = i32;
pub type CGImageRef = *const c_void;
pub type CGContextRef = *const c_void;
pub type CGColorSpaceRef = *const c_void;

pub const kAXErrorSuccess: AXError = 0;
pub const kAXValueCGPointType: u32 = 1;
pub const kAXValueCGSizeType: u32 = 2;
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;

pub const kCGWindowListOptionAll: u32 = 0;
pub const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
pub const kCGNullWindowID: u32 = 0;
/// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big
pub const kRGBA_PREMULTIPLIED: u32 = 1 | (4 << 12);

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

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
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
    pub fn CFURLGetTypeID() -> CFTypeID;
    pub fn CFURLCopyFileSystemPath(url: CFTypeRef, style: isize) -> CFStringRef;
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
    pub fn CFUUIDCreateString(alloc: CFAllocatorRef, uuid: CFTypeRef) -> CFStringRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXUIElementGetTypeID() -> CFTypeID;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: *mut CFTypeRef) -> AXError;
    pub fn AXUIElementSetAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: CFTypeRef) -> AXError;
    pub fn AXUIElementSetMessagingTimeout(e: AXUIElementRef, seconds: f32) -> AXError;
    pub fn AXUIElementPerformAction(e: AXUIElementRef, action: CFStringRef) -> AXError;
    pub fn AXValueCreate(t: u32, value: *const c_void) -> CFTypeRef;
    pub fn AXValueGetValue(v: CFTypeRef, t: u32, value: *mut c_void) -> u8;
    pub fn AXValueGetTypeID() -> CFTypeID;
    /// Private but long-stable SPI used by most window managers.
    pub fn _AXUIElementGetWindow(e: AXUIElementRef, wid: *mut u32) -> AXError;
    pub fn GetProcessForPID(pid: i32, psn: *mut ProcessSerialNumber) -> i32;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;
    pub fn CGDisplayVendorNumber(d: u32) -> u32;
    pub fn CGDisplayModelNumber(d: u32) -> u32;
    pub fn CGDisplaySerialNumber(d: u32) -> u32;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(c: CGColorSpaceRef);
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub fn CGContextRelease(ctx: CGContextRef);
    pub fn CGContextDrawImage(ctx: CGContextRef, r: CGRect, img: CGImageRef);
    pub fn CGContextSetInterpolationQuality(ctx: CGContextRef, q: i32);
}

extern "C" {
    pub fn proc_pidpath(pid: i32, buf: *mut c_void, size: u32) -> i32;
    pub fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
}

/// `RTLD_DEFAULT` on macOS.
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// Look up a (private) symbol in the already-loaded images.
pub fn find_symbol(name: &CStr) -> *mut c_void {
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
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
    num_i64(v)
}

/// # Safety
/// `v` must be null or a valid CF object.
pub unsafe fn num_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
        return None;
    }
    let mut out: i64 = 0;
    (CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) != 0).then_some(out)
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

/// A CFArray of CFNumbers (owned).
///
/// # Safety
/// Plain CF calls.
pub unsafe fn number_array(values: &[u64]) -> CfOwned {
    let nums: Vec<CFTypeRef> = values
        .iter()
        .map(|&v| {
            let v = v as i64;
            CFNumberCreate(std::ptr::null(), kCFNumberSInt64Type, &v as *const i64 as *const c_void)
        })
        .collect();
    let arr = CFArrayCreate(std::ptr::null(), nums.as_ptr(), nums.len() as isize, &kCFTypeArrayCallBacks);
    for n in nums {
        CFRelease(n);
    }
    CfOwned(arr)
}

/// # Safety
/// `arr` must be null or a valid CF object.
pub unsafe fn i64_array(arr: CFArrayRef) -> Vec<i64> {
    let mut out = Vec::new();
    if arr.is_null() || CFGetTypeID(arr) != CFArrayGetTypeID() {
        return out;
    }
    for i in 0..CFArrayGetCount(arr) {
        if let Some(n) = num_i64(CFArrayGetValueAtIndex(arr, i)) {
            out.push(n);
        }
    }
    out
}

/// `CGImageRef` with its Objective-C encoding (for `msg_send!` returns).
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgImage(pub CGImageRef);

unsafe impl objc2::encode::Encode for CgImage {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGImage", &[]));
}
