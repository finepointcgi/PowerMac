//! Battery levels of Bluetooth HID devices (Magic Mouse / Keyboard /
//! Trackpad, many third-party ones) from the IORegistry:
//! `AppleDeviceManagementHIDEventService` entries with `BatteryPercent`.
//! Read-only; no permission needed.

use std::ffi::{c_char, c_void, CString};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::msg_send;

use super::ffi::ns;
use super::rt;
use crate::widgets::bluetooth::HidBattery;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iter: *mut u32) -> i32;
    fn IOIteratorNext(iter: u32) -> u32;
    fn IOObjectRelease(obj: u32) -> i32;
    fn IORegistryEntryCreateCFProperties(entry: u32, props: *mut *mut c_void, alloc: *const c_void, options: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(p: *const c_void);
}

fn number(dict: &AnyObject, key: &str) -> Option<i64> {
    unsafe {
        let v: Option<Retained<AnyObject>> = msg_send![dict, objectForKey: &*ns(key)];
        let v = v?;
        let is_num: bool = msg_send![&*v, isKindOfClass: objc2::class!(NSNumber)];
        is_num.then(|| {
            let n: i64 = msg_send![&*v, longLongValue];
            n
        })
    }
}

fn string(dict: &AnyObject, key: &str) -> String {
    unsafe {
        let v: Option<Retained<AnyObject>> = msg_send![dict, objectForKey: &*ns(key)];
        match v {
            Some(v) => {
                let is_str: bool = msg_send![&*v, isKindOfClass: objc2::class!(NSString)];
                if is_str {
                    rt::string_of(Some(&v))
                } else {
                    String::new()
                }
            }
            None => String::new(),
        }
    }
}

/// HID devices that report a battery level.
pub fn hid_batteries() -> Vec<HidBattery> {
    let mut out = vec![];
    let Ok(class) = CString::new("AppleDeviceManagementHIDEventService") else { return out };
    autoreleasepool(|_| unsafe {
        let matching = IOServiceMatching(class.as_ptr());
        if matching.is_null() {
            return;
        }
        let mut iter: u32 = 0;
        // Consumes `matching`.
        if IOServiceGetMatchingServices(0, matching, &mut iter) != 0 {
            return;
        }
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            let mut props: *mut c_void = std::ptr::null_mut();
            if IORegistryEntryCreateCFProperties(entry, &mut props, std::ptr::null(), 0) == 0 && !props.is_null() {
                let dict = &*(props as *const AnyObject);
                if let Some(pct) = number(dict, "BatteryPercent").filter(|p| (0..=100).contains(p)) {
                    let name = string(dict, "Product");
                    if !name.is_empty() {
                        out.push(HidBattery { name, address: string(dict, "DeviceAddress"), percent: pct as u8 });
                    }
                }
                CFRelease(props);
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
    });
    out.dedup_by(|a, b| a.name == b.name && a.address == b.address);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn reading_the_registry_is_harmless() {
        let v = super::hid_batteries();
        println!("{v:?}");
        assert!(v.iter().all(|b| b.percent <= 100 && !b.name.is_empty()));
    }
}
