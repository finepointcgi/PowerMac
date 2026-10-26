//! macOS: pointer devices through the HID event system client
//! (`IOHIDEventSystemClient` / `IOHIDServiceClient`, exported by IOKit; not in
//! the public headers but stable for many releases and used by LinearMouse).
//!
//! Every pointing service exposes its acceleration (16.16 fixed point, under
//! the key named by its `HIDPointerAccelerationType` property, normally
//! `HIDMouseAcceleration` or `HIDTrackpadAcceleration`; -1.0 = acceleration
//! off) and its `HIDPointerResolution` (16.16 dpi; lower = faster). Setting
//! them on the service changes only that device, in memory, until the device
//! reconnects or the system pushes its preferences again.
//!
//! Observed on macOS 26/27: the "simple" client hides most properties, so the
//! full client (`IOHIDEventSystemClientCreate`) is used. Writes go to the
//! event system's pointer filter and are not visible to reads: a service
//! reports its driver's resolution and no acceleration value at all, while
//! the client reports the system-wide acceleration preference.

#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::{c_char, c_void, CStr};

use crate::{DeviceInfo, DeviceKind};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFNumberRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFTypeID = usize;
type ClientRef = *mut c_void;
type ServiceRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    fn CFNumberGetValue(n: CFNumberRef, ty: isize, out: *mut c_void) -> u8;
    fn CFNumberCreate(alloc: CFAllocatorRef, ty: isize, value: *const c_void) -> CFNumberRef;
    fn CFArrayGetCount(a: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> *const c_void;
    fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, enc: u32) -> u8;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDEventSystemClientCreateSimpleClient(alloc: CFAllocatorRef) -> ClientRef;
    fn IOHIDEventSystemClientCreate(alloc: CFAllocatorRef) -> ClientRef;
    fn IOHIDEventSystemClientCopyProperty(client: ClientRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDEventSystemClientCopyServices(client: ClientRef) -> CFArrayRef;
    fn IOHIDServiceClientCopyProperty(service: ServiceRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientSetProperty(service: ServiceRef, key: CFStringRef, value: CFTypeRef) -> u8;
    fn IOHIDServiceClientConformsTo(service: ServiceRef, page: u32, usage: u32) -> u8;
    fn IOHIDServiceClientGetRegistryID(service: ServiceRef) -> CFTypeRef;
}

const kCFStringEncodingUTF8: u32 = 0x0800_0100;
const kCFNumberSInt64Type: isize = 4;
const kCFNumberFloat64Type: isize = 6;

pub(crate) const KEY_ACCEL_TYPE: &str = "HIDPointerAccelerationType";
pub(crate) const KEY_MOUSE_ACCEL: &str = "HIDMouseAcceleration";
pub(crate) const KEY_TRACKPAD_ACCEL: &str = "HIDTrackpadAcceleration";
pub(crate) const KEY_RESOLUTION: &str = "HIDPointerResolution";

/// An owned CFString.
struct CfStr(CFStringRef);
impl CfStr {
    fn new(s: &str) -> Option<Self> {
        let c = std::ffi::CString::new(s).ok()?;
        let r = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) };
        (!r.is_null()).then(|| Self(r))
    }
}
impl Drop for CfStr {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

/// An owned CF object.
struct Owned(CFTypeRef);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

unsafe fn cf_to_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    let ty = CFGetTypeID(v);
    if ty == CFNumberGetTypeID() {
        let mut out: i64 = 0;
        if CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) != 0 {
            return Some(out);
        }
        let mut f: f64 = 0.0;
        CFNumberGetValue(v, kCFNumberFloat64Type, &mut f as *mut f64 as *mut c_void);
        return Some(f as i64);
    }
    if ty == CFBooleanGetTypeID() {
        return Some(CFBooleanGetValue(v) as i64);
    }
    None
}

unsafe fn cf_to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    if CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) == 0 {
        return None;
    }
    Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
}

/// The HID event system client.
pub(crate) struct Client(ClientRef);

// SAFETY: the client is only used from the thread that owns the `Client`
// (moved there once); IOHIDEventSystemClient calls are thread-safe.
unsafe impl Send for Client {}

impl Client {
    pub(crate) fn new() -> Option<Self> {
        let mut c = unsafe { IOHIDEventSystemClientCreate(std::ptr::null()) };
        if c.is_null() {
            c = unsafe { IOHIDEventSystemClientCreateSimpleClient(std::ptr::null()) };
        }
        (!c.is_null()).then(|| Self(c))
    }

    /// A system-wide event-system property (e.g. the acceleration preferences).
    pub(crate) fn get_i64(&self, key: &str) -> Option<i64> {
        let k = CfStr::new(key)?;
        let v = unsafe { IOHIDEventSystemClientCopyProperty(self.0, k.0) };
        if v.is_null() {
            return None;
        }
        let v = Owned(v);
        unsafe { cf_to_i64(v.0) }
    }

    /// Every HID service (keyboards, pointers, sensors, ...).
    pub(crate) fn services(&self) -> Vec<Service> {
        let mut out = Vec::new();
        unsafe {
            let arr = IOHIDEventSystemClientCopyServices(self.0);
            if arr.is_null() {
                return out;
            }
            for i in 0..CFArrayGetCount(arr) {
                let s = CFArrayGetValueAtIndex(arr, i);
                if !s.is_null() {
                    out.push(Service(CFRetain(s) as ServiceRef));
                }
            }
            CFRelease(arr);
        }
        out
    }

    /// Pointing services with their device info.
    pub(crate) fn pointers(&self) -> Vec<(Service, DeviceInfo)> {
        self.services()
            .into_iter()
            .filter_map(|s| s.pointer_info().map(|i| (s, i)))
            .collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

/// A retained `IOHIDServiceClient`.
pub(crate) struct Service(ServiceRef);

impl Drop for Service {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

impl Service {
    fn copy(&self, key: &str) -> Option<Owned> {
        let k = CfStr::new(key)?;
        let v = unsafe { IOHIDServiceClientCopyProperty(self.0, k.0) };
        (!v.is_null()).then(|| Owned(v))
    }

    pub(crate) fn get_i64(&self, key: &str) -> Option<i64> {
        self.copy(key).and_then(|v| unsafe { cf_to_i64(v.0) })
    }

    pub(crate) fn get_string(&self, key: &str) -> Option<String> {
        self.copy(key).and_then(|v| unsafe { cf_to_string(v.0) })
    }

    pub(crate) fn set_i64(&self, key: &str, value: i64) -> bool {
        let Some(k) = CfStr::new(key) else { return false };
        unsafe {
            let n = CFNumberCreate(std::ptr::null(), kCFNumberSInt64Type, &value as *const i64 as *const c_void);
            if n.is_null() {
                return false;
            }
            let ok = IOHIDServiceClientSetProperty(self.0, k.0, n) != 0;
            CFRelease(n);
            ok
        }
    }

    fn conforms(&self, page: u32, usage: u32) -> bool {
        unsafe { IOHIDServiceClientConformsTo(self.0, page, usage) != 0 }
    }

    pub(crate) fn registry_id(&self) -> Option<u64> {
        unsafe { cf_to_i64(IOHIDServiceClientGetRegistryID(self.0)).map(|v| v as u64) }
    }

    /// The acceleration property this service uses.
    pub(crate) fn accel_key(&self, kind: DeviceKind) -> String {
        self.get_string(KEY_ACCEL_TYPE).unwrap_or_else(|| {
            match kind {
                DeviceKind::Trackpad => KEY_TRACKPAD_ACCEL,
                DeviceKind::Mouse => KEY_MOUSE_ACCEL,
            }
            .to_string()
        })
    }

    /// `Some` for pointing devices (mice, trackpads), `None` otherwise.
    pub(crate) fn pointer_info(&self) -> Option<DeviceInfo> {
        let accel_type = self.get_string(KEY_ACCEL_TYPE);
        let product = self.get_string("Product").unwrap_or_default();
        let is_touchpad = self.conforms(0x0D, 0x05);
        let is_mouse = self.conforms(0x01, 0x02);
        let is_pointer = self.conforms(0x01, 0x01);
        let kind = classify(accel_type.as_deref(), &product, is_touchpad, is_mouse || is_pointer)?;
        // Only services that actually move the pointer (keyboard halves of
        // combo devices and sensors have no resolution / acceleration).
        if accel_type.is_none() && self.get_i64(KEY_RESOLUTION).is_none() {
            return None;
        }
        Some(DeviceInfo {
            id: self.registry_id().unwrap_or(0),
            name: if product.is_empty() { "Unknown pointing device".into() } else { product },
            vendor_id: self.get_i64("VendorID").unwrap_or(0) as u32,
            product_id: self.get_i64("ProductID").unwrap_or(0) as u32,
            kind,
            transport: self.get_string("Transport").unwrap_or_default(),
        })
    }
}

/// Device kind from the service's properties. The built-in trackpad presents
/// itself as a generic-desktop *mouse*, so the acceleration type decides first.
pub(crate) fn classify(
    accel_type: Option<&str>,
    product: &str,
    conforms_touchpad: bool,
    conforms_pointer: bool,
) -> Option<DeviceKind> {
    match accel_type {
        Some(KEY_TRACKPAD_ACCEL) => return Some(DeviceKind::Trackpad),
        Some(KEY_MOUSE_ACCEL) => return Some(DeviceKind::Mouse),
        _ => {}
    }
    if conforms_touchpad || product.to_ascii_lowercase().contains("trackpad") {
        Some(DeviceKind::Trackpad)
    } else if conforms_pointer {
        Some(DeviceKind::Mouse)
    } else {
        None
    }
}

/// All pointing devices, one entry per distinct (kind, vendor, product, name).
pub(crate) fn list_devices() -> Vec<DeviceInfo> {
    let Some(client) = Client::new() else { return Vec::new() };
    let mut out: Vec<DeviceInfo> = Vec::new();
    for (_, info) in client.pointers() {
        if !out.iter().any(|d| d.same_model(&info)) {
            out.push(info);
        }
    }
    out
}
