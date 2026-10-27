//! Hand-written bindings: the shared CoreFoundation / CoreGraphics /
//! libdispatch ones from `pm_shelf_core::mac::ffi`, plus IOKit power
//! sources and the notch heuristic.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

use objc2::rc::Retained;

#[allow(unused_imports)]
pub use pm_shelf_core::mac::ffi::*;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFTypeRef;
    pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFTypeRef;
    pub fn IOPSGetProvidingPowerSourceType(blob: CFTypeRef) -> CFStringRef;
}

/// Battery state: (on AC power, percent).
pub fn power_state() -> Option<(bool, u8)> {
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return None;
        }
        let ty = IOPSGetProvidingPowerSourceType(blob);
        let ac_key = ns("AC Power");
        let on_ac = !ty.is_null() && CFStringCompare(ty, Retained::as_ptr(&ac_key) as CFTypeRef, 0) == 0;
        let list = IOPSCopyPowerSourcesList(blob);
        let mut pct = None;
        if !list.is_null() {
            for i in 0..CFArrayGetCount(list) {
                let ps = CFArrayGetValueAtIndex(list, i);
                let d = IOPSGetPowerSourceDescription(blob, ps);
                if d.is_null() {
                    continue;
                }
                let get = |k: &str| -> Option<i64> {
                    let key = ns(k);
                    let v = CFDictionaryGetValue(d, Retained::as_ptr(&key) as CFTypeRef);
                    if v.is_null() {
                        return None;
                    }
                    let mut out: i64 = 0;
                    // kCFNumberSInt64Type = 4
                    (CFNumberGetValue(v, 4, &mut out as *mut i64 as *mut c_void) != 0).then_some(out)
                };
                if let (Some(c), Some(m)) = (get("Current Capacity"), get("Max Capacity")) {
                    if m > 0 {
                        pct = Some(((c * 100) / m).clamp(0, 100) as u8);
                        break;
                    }
                }
            }
            CFRelease(list);
        }
        CFRelease(blob);
        pct.map(|p| (on_ac, p))
    }
}

/// Heuristic usable from any thread: is the built-in panel one of the
/// notched ones? Notched MacBook panels are ~1.54:1 (the extra rows hold
/// the menu bar beside the camera housing); the older ones are 16:10.
pub fn builtin_display_has_notch_heuristic() -> bool {
    unsafe {
        let mut ids = [0u32; 16];
        let mut n = 0u32;
        if CGGetOnlineDisplayList(16, ids.as_mut_ptr(), &mut n) != 0 {
            return false;
        }
        ids[..n as usize].iter().any(|&d| {
            if CGDisplayIsBuiltin(d) == 0 {
                return false;
            }
            let mode = CGDisplayCopyDisplayMode(d);
            let (w, h) = if mode.is_null() {
                (CGDisplayPixelsWide(d), CGDisplayPixelsHigh(d))
            } else {
                let r = (CGDisplayModeGetPixelWidth(mode), CGDisplayModeGetPixelHeight(mode));
                CGDisplayModeRelease(mode);
                r
            };
            h > 0 && crate::platform::aspect_suggests_notch(w as f64, h as f64)
        })
    }
}
