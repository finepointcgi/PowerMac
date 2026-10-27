//! Hand-written CoreFoundation / IOKit bindings plus a few CF helpers.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void, CString};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFTypeID = usize;
pub type IoObject = u32;
pub type IoConnect = u32;
pub type KernReturn = i32;
pub type IONotificationPortRef = *mut c_void;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;
pub const kCFNumberFloat64Type: isize = 6;
pub const KERN_SUCCESS: KernReturn = 0;

pub type IOServiceInterestCallback = extern "C" fn(
    refcon: *mut c_void,
    service: IoObject,
    message_type: u32,
    message_argument: *mut c_void,
);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        s: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    pub fn CFNumberGetValue(n: CFTypeRef, the_type: isize, out: *mut c_void) -> u8;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
    pub fn IOServiceGetMatchingService(main_port: u32, matching: CFDictionaryRef) -> IoObject;
    pub fn IOServiceOpen(
        service: IoObject,
        owning_task: u32,
        conn_type: u32,
        connect: *mut IoConnect,
    ) -> KernReturn;
    pub fn IOServiceClose(connect: IoConnect) -> KernReturn;
    pub fn IOObjectRelease(object: IoObject) -> KernReturn;
    pub fn IOConnectCallStructMethod(
        connection: IoConnect,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
    pub fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFStringRef,
        alloc: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    pub fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    pub fn IONotificationPortGetRunLoopSource(notify: IONotificationPortRef) -> CFRunLoopSourceRef;
    pub fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> KernReturn;
}

extern "C" {
    pub static mach_task_self_: u32;
}

/// Owned CF object, released on drop.
pub struct Cf(pub CFTypeRef);

impl Cf {
    pub fn string(s: &str) -> Option<Cf> {
        let c = CString::new(s).ok()?;
        // SAFETY: valid NUL-terminated string; NULL allocator = default.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8)
        };
        (!r.is_null()).then_some(Cf(r))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this reference (Create/Copy rule).
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Borrowed CF value → i64 (numbers and booleans).
///
/// # Safety
/// `v` must be NULL or a valid CF object.
pub unsafe fn cf_to_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    let t = CFGetTypeID(v);
    if t == CFNumberGetTypeID() {
        let mut out: i64 = 0;
        if CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) != 0 {
            return Some(out);
        }
        let mut f: f64 = 0.0;
        if CFNumberGetValue(v, kCFNumberFloat64Type, &mut f as *mut f64 as *mut c_void) != 0 {
            return Some(f as i64);
        }
        None
    } else if t == CFBooleanGetTypeID() {
        Some(CFBooleanGetValue(v) as i64)
    } else {
        None
    }
}

/// Borrowed lookup `dict[key]` (no ownership transfer).
///
/// # Safety
/// `dict` must be a valid CFDictionary.
pub unsafe fn dict_get(dict: CFDictionaryRef, key: &str) -> CFTypeRef {
    match Cf::string(key) {
        Some(k) => CFDictionaryGetValue(dict, k.0),
        None => std::ptr::null(),
    }
}

/// True if `v` is a CFDictionary.
///
/// # Safety
/// `v` must be NULL or a valid CF object.
pub unsafe fn is_dict(v: CFTypeRef) -> bool {
    !v.is_null() && CFGetTypeID(v) == CFDictionaryGetTypeID()
}

/// Owned IOKit object.
pub struct IoObj(pub IoObject);

impl IoObj {
    /// First service matching `class`.
    pub fn matching(class: &str) -> Option<IoObj> {
        let c = CString::new(class).ok()?;
        // SAFETY: IOServiceMatching returns a dict that
        // IOServiceGetMatchingService consumes.
        let s = unsafe { IOServiceGetMatchingService(0, IOServiceMatching(c.as_ptr())) };
        (s != 0).then_some(IoObj(s))
    }

    /// Copy one registry property (owned).
    pub fn property(&self, key: &str) -> Option<Cf> {
        let k = Cf::string(key)?;
        // SAFETY: valid entry and key; returns +1 reference or NULL.
        let v = unsafe { IORegistryEntryCreateCFProperty(self.0, k.0, std::ptr::null(), 0) };
        (!v.is_null()).then_some(Cf(v))
    }
}

impl Drop for IoObj {
    fn drop(&mut self) {
        // SAFETY: we own this object reference.
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}
