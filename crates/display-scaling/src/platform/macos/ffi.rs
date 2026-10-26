//! Hand-written CoreFoundation / CoreGraphics bindings and small CF helpers.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void, CStr, CString};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFTypeID = usize;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *const c_void;
pub type CGDisplayConfigRef = *mut c_void;
pub type CGError = i32;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
pub struct CFDictionaryCallBacks {
    _opaque: [u8; 0],
}

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;

pub const kCGConfigureForAppOnly: u32 = 0;
pub const kCGConfigureForSession: u32 = 1;
pub const kCGConfigurePermanently: u32 = 2;
pub const kCGNullDirectDisplay: CGDirectDisplayID = 0;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
    pub static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFDataGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const c_char, encoding: u32) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    pub fn CFNumberGetValue(n: CFTypeRef, the_type: isize, out: *mut c_void) -> u8;
    pub fn CFDictionaryCreate(alloc: CFAllocatorRef, keys: *const CFTypeRef, values: *const CFTypeRef, n: isize, kcb: *const CFDictionaryCallBacks, vcb: *const CFDictionaryCallBacks) -> CFDictionaryRef;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFDictionaryGetCount(d: CFDictionaryRef) -> isize;
    pub fn CFDictionaryGetKeysAndValues(d: CFDictionaryRef, keys: *mut CFTypeRef, values: *mut CFTypeRef);
    pub fn CFArrayGetCount(a: CFArrayRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> CFTypeRef;
    pub fn CFDataGetLength(d: CFTypeRef) -> isize;
    pub fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub static kCGDisplayShowDuplicateLowResolutionModes: CFStringRef;
    pub fn CGGetOnlineDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> CGError;
    pub fn CGDisplayIsBuiltin(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayIsOnline(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayVendorNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayModelNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplaySerialNumber(d: CGDirectDisplayID) -> u32;
    pub fn CGDisplayScreenSize(d: CGDirectDisplayID) -> CGSize;
    pub fn CGDisplayMirrorsDisplay(d: CGDirectDisplayID) -> CGDirectDisplayID;
    pub fn CGDisplayCopyAllDisplayModes(d: CGDirectDisplayID, options: CFDictionaryRef) -> CFArrayRef;
    pub fn CGDisplayCopyDisplayMode(d: CGDirectDisplayID) -> CGDisplayModeRef;
    pub fn CGDisplayModeGetWidth(m: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetHeight(m: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelWidth(m: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelHeight(m: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetRefreshRate(m: CGDisplayModeRef) -> f64;
    pub fn CGDisplayModeGetIOFlags(m: CGDisplayModeRef) -> u32;
    pub fn CGDisplayModeIsUsableForDesktopGUI(m: CGDisplayModeRef) -> bool;
    pub fn CGDisplayModeRetain(m: CGDisplayModeRef) -> CGDisplayModeRef;
    pub fn CGDisplayModeRelease(m: CGDisplayModeRef);
    pub fn CGBeginDisplayConfiguration(cfg: *mut CGDisplayConfigRef) -> CGError;
    pub fn CGConfigureDisplayWithDisplayMode(cfg: CGDisplayConfigRef, d: CGDirectDisplayID, mode: CGDisplayModeRef, options: CFDictionaryRef) -> CGError;
    pub fn CGConfigureDisplayMirrorOfDisplay(cfg: CGDisplayConfigRef, d: CGDirectDisplayID, master: CGDirectDisplayID) -> CGError;
    pub fn CGCompleteDisplayConfiguration(cfg: CGDisplayConfigRef, option: u32) -> CGError;
    pub fn CGCancelDisplayConfiguration(cfg: CGDisplayConfigRef) -> CGError;
}

extern "C" {
    pub fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}
pub const RTLD_LAZY: i32 = 1;

/// An owned CF object (released on drop).
pub struct Cf(pub CFTypeRef);

impl Cf {
    pub fn owned(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then(|| Self(p))
    }
    pub fn get(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

pub fn cfstr(s: &str) -> Cf {
    let c = CString::new(s).unwrap_or_default();
    Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) })
}

pub fn dict_get(d: CFTypeRef, key: &str) -> CFTypeRef {
    if d.is_null() || unsafe { CFGetTypeID(d) != CFDictionaryGetTypeID() } {
        return std::ptr::null();
    }
    let k = cfstr(key);
    unsafe { CFDictionaryGetValue(d, k.get()) }
}

pub fn dict_values(d: CFTypeRef) -> Vec<CFTypeRef> {
    if d.is_null() || unsafe { CFGetTypeID(d) != CFDictionaryGetTypeID() } {
        return Vec::new();
    }
    let n = unsafe { CFDictionaryGetCount(d) }.max(0) as usize;
    let mut keys = vec![std::ptr::null(); n];
    let mut vals = vec![std::ptr::null(); n];
    unsafe { CFDictionaryGetKeysAndValues(d, keys.as_mut_ptr(), vals.as_mut_ptr()) };
    vals
}

pub fn cf_to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = vec![0 as c_char; 1024];
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) } == 0 {
        return None;
    }
    Some(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

pub fn cf_to_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i64;
    (unsafe { CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) } != 0).then_some(out)
}

pub fn cf_to_bytes(v: CFTypeRef) -> Option<Vec<u8>> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFDataGetTypeID() } {
        return None;
    }
    let n = unsafe { CFDataGetLength(v) }.max(0) as usize;
    let p = unsafe { CFDataGetBytePtr(v) };
    (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts(p, n) }.to_vec())
}

pub fn online_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) } != 0 {
        return Vec::new();
    }
    ids[..n as usize].to_vec()
}

/// Run one display-configuration transaction.
pub fn configure(option: u32, f: impl FnOnce(CGDisplayConfigRef) -> CGError) -> anyhow::Result<()> {
    let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
    let e = unsafe { CGBeginDisplayConfiguration(&mut cfg) };
    if e != 0 || cfg.is_null() {
        anyhow::bail!("CGBeginDisplayConfiguration failed ({e})");
    }
    let e = f(cfg);
    if e != 0 {
        unsafe { CGCancelDisplayConfiguration(cfg) };
        anyhow::bail!("display configuration failed ({e})");
    }
    let e = unsafe { CGCompleteDisplayConfiguration(cfg, option) };
    if e != 0 {
        anyhow::bail!("CGCompleteDisplayConfiguration failed ({e})");
    }
    Ok(())
}
