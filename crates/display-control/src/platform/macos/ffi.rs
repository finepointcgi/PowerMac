//! Hand-written bindings: CoreFoundation, IOKit registry, CoreGraphics
//! displays / event taps / gamma, libdispatch, dlopen; plus small CF helpers.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void, CStr, CString};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFMachPortRef = *const c_void;
pub type CFTypeID = usize;
pub type CGEventRef = *const c_void;
pub type CGEventTapProxy = *const c_void;
pub type CGDirectDisplayID = u32;
pub type IoObject = u32;
pub type KernReturn = i32;

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

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;
pub const kCFNumberFloat64Type: isize = 6;

pub const kIORegistryIterateRecursively: u32 = 1;

pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventSystemDefined: u32 = 14;
pub const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;
pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

pub type CGEventTapCallBack = extern "C" fn(proxy: CGEventTapProxy, etype: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFAllocatorDefault: CFAllocatorRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFDataGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const c_char, encoding: u32) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    pub fn CFStringGetLength(s: CFStringRef) -> isize;
    pub fn CFNumberGetValue(n: CFTypeRef, the_type: isize, out: *mut c_void) -> u8;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFDictionaryGetCount(d: CFDictionaryRef) -> isize;
    pub fn CFDictionaryGetKeysAndValues(d: CFDictionaryRef, keys: *mut CFTypeRef, values: *mut CFTypeRef);
    pub fn CFDataGetLength(d: CFTypeRef) -> isize;
    pub fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFMachPortCreateRunLoopSource(alloc: CFAllocatorRef, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IORegistryCreateIterator(main_port: u32, plane: *const c_char, options: u32, iterator: *mut IoObject) -> KernReturn;
    pub fn IOIteratorNext(iterator: IoObject) -> IoObject;
    pub fn IOObjectRelease(object: IoObject) -> KernReturn;
    pub fn IOObjectGetClass(object: IoObject, class_name: *mut c_char) -> KernReturn;
    pub fn IORegistryEntryGetName(entry: IoObject, name: *mut c_char) -> KernReturn;
    pub fn IORegistryEntryGetPath(entry: IoObject, plane: *const c_char, path: *mut c_char) -> KernReturn;
    pub fn IORegistryEntryCreateCFProperty(entry: IoObject, key: CFStringRef, alloc: CFAllocatorRef, options: u32) -> CFTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGGetOnlineDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> i32;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayIsBuiltin(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayIsActive(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayIsAsleep(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayMirrorsDisplay(d: CGDirectDisplayID) -> CGDirectDisplayID;
    pub fn CGDisplayBounds(d: CGDirectDisplayID) -> CGRect;
    pub fn CGDisplayVendorNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayModelNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplaySerialNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayScreenSize(d: CGDirectDisplayID) -> CGSize;
    pub fn CGDisplayGammaTableCapacity(d: CGDirectDisplayID) -> u32;
    pub fn CGGetDisplayTransferByTable(d: CGDirectDisplayID, capacity: u32, red: *mut f32, green: *mut f32, blue: *mut f32, count: *mut u32) -> i32;
    pub fn CGSetDisplayTransferByTable(d: CGDirectDisplayID, size: u32, red: *const f32, green: *const f32, blue: *const f32) -> i32;
    pub fn CGDisplayRestoreColorSyncSettings();

    pub fn CGEventTapCreate(tap: u32, place: u32, options: u32, events_of_interest: u64, callback: CGEventTapCallBack, user_info: *mut c_void) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetFlags(e: CGEventRef) -> u64;
    pub fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    pub fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
}

pub const RTLD_LAZY: i32 = 0x1;
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

extern "C" {
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
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

/// Look up `symbol` in the framework at `path` (loaded on demand), or in
/// the already-loaded images when `path` is `None`. Null when absent.
pub fn find_symbol(path: Option<&CStr>, symbol: &CStr) -> *mut c_void {
    unsafe {
        let handle = match path {
            Some(p) => {
                let h = dlopen(p.as_ptr(), RTLD_LAZY);
                if h.is_null() {
                    return std::ptr::null_mut();
                }
                h
            }
            None => RTLD_DEFAULT,
        };
        dlsym(handle, symbol.as_ptr())
    }
}

/// Current pointer location (global, top-left origin; no permission needed).
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

/// An owned CF object (released on drop).
pub struct Cf(pub CFTypeRef);

impl Cf {
    /// Take ownership of a +1 reference (None if null).
    pub fn owned(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then_some(Self(p))
    }

    pub fn string(s: &str) -> Self {
        let c = CString::new(s).unwrap_or_default();
        Self(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) })
    }

    pub fn get(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

unsafe impl Send for Cf {}

/// Borrowed-CF accessors (the caller keeps the object alive).
pub fn cf_to_string(v: CFTypeRef) -> Option<String> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
            return None;
        }
        let len = CFStringGetLength(v).max(0) as usize;
        let mut buf = vec![0 as c_char; len * 4 + 1];
        if CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) == 0 {
            return None;
        }
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}

pub fn cf_to_i64(v: CFTypeRef) -> Option<i64> {
    unsafe {
        if v.is_null() {
            return None;
        }
        if CFGetTypeID(v) == CFBooleanGetTypeID() {
            return Some(CFBooleanGetValue(v) as i64);
        }
        if CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        if CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) == 0 {
            // Floating-point numbers report a lossy conversion but still convert.
            let mut f: f64 = 0.0;
            CFNumberGetValue(v, kCFNumberFloat64Type, &mut f as *mut f64 as *mut c_void);
            return Some(f as i64);
        }
        Some(out)
    }
}

pub fn cf_to_bytes(v: CFTypeRef) -> Option<Vec<u8>> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFDataGetTypeID() {
            return None;
        }
        let len = CFDataGetLength(v).max(0) as usize;
        let p = CFDataGetBytePtr(v);
        if p.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(p, len).to_vec())
    }
}

pub fn is_dict(v: CFTypeRef) -> bool {
    !v.is_null() && unsafe { CFGetTypeID(v) == CFDictionaryGetTypeID() }
}

/// `dict[key]` (borrowed; null if missing or `dict` isn't a dictionary).
pub fn dict_get(dict: CFTypeRef, key: &str) -> CFTypeRef {
    if !is_dict(dict) {
        return std::ptr::null();
    }
    let k = Cf::string(key);
    unsafe { CFDictionaryGetValue(dict, k.get()) }
}

/// All values of a dictionary (borrowed).
pub fn dict_values(dict: CFTypeRef) -> Vec<CFTypeRef> {
    if !is_dict(dict) {
        return Vec::new();
    }
    unsafe {
        let n = CFDictionaryGetCount(dict).max(0) as usize;
        let mut keys = vec![std::ptr::null(); n];
        let mut vals = vec![std::ptr::null(); n];
        CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), vals.as_mut_ptr());
        vals
    }
}

/// An IORegistry property (owned).
pub fn io_property(entry: IoObject, key: &str) -> Option<Cf> {
    let k = Cf::string(key);
    Cf::owned(unsafe { IORegistryEntryCreateCFProperty(entry, k.get(), std::ptr::null(), 0) })
}

pub fn io_name(entry: IoObject) -> String {
    let mut buf = [0 as c_char; 128];
    unsafe {
        if IORegistryEntryGetName(entry, buf.as_mut_ptr()) != 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

pub fn io_class(entry: IoObject) -> String {
    let mut buf = [0 as c_char; 128];
    unsafe {
        if IOObjectGetClass(entry, buf.as_mut_ptr()) != 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

pub fn io_path(entry: IoObject) -> String {
    let mut buf = [0 as c_char; 1024];
    unsafe {
        if IORegistryEntryGetPath(entry, c"IOService".as_ptr(), buf.as_mut_ptr()) != 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}
