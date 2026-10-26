//! Small Objective-C runtime helpers for the widgets: loading frameworks at
//! runtime (so the host doesn't link EventKit / AVFoundation unless a
//! widget needs them), Info.plist checks, HTTP GET via NSURLSession,
//! opening URLs, media keys.

use std::ffi::{c_void, CString};
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSData, NSString};

use super::ffi::ns;

/// A retained object that may cross threads (the wrapped objects are
/// documented thread-safe, or only touched by one thread at a time).
pub struct SendObj(pub Retained<AnyObject>);
unsafe impl Send for SendObj {}

pub fn load_framework(path: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let b: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), bundleWithPath: &*ns(path)];
        match b {
            Some(b) => {
                let ok: bool = msg_send![&*b, load];
                ok
            }
            None => false,
        }
    })
}

pub fn class_named(name: &str) -> Option<&'static AnyClass> {
    let c = CString::new(name).ok()?;
    AnyClass::get(&c)
}

/// Does the host app's Info.plist have `key` (a usage description)?
pub fn info_plist_has(key: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let b: Retained<AnyObject> = msg_send![class!(NSBundle), mainBundle];
        let v: Option<Retained<AnyObject>> = msg_send![&*b, objectForInfoDictionaryKey: &*ns(key)];
        v.is_some()
    })
}

pub fn string_of(obj: Option<&AnyObject>) -> String {
    match obj {
        Some(o) => {
            let s: &NSString = unsafe { &*(o as *const AnyObject as *const NSString) };
            s.to_string()
        }
        None => String::new(),
    }
}

/// `NSDate` → unix seconds.
pub fn unix_of_date(d: Option<&AnyObject>) -> Option<f64> {
    d.map(|d| unsafe { msg_send![d, timeIntervalSince1970] })
}

/// HTTP GET with a timeout; any thread.
pub fn http_get(url: &str, timeout: f64) -> Result<Vec<u8>, String> {
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    autoreleasepool(|_| unsafe {
        let u: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*ns(url)];
        let Some(u) = u else {
            let _ = tx.send(Err("bad URL".into()));
            return;
        };
        // NSURLRequestReloadIgnoringLocalCacheData = 1
        let req: Retained<AnyObject> = msg_send![class!(NSURLRequest), requestWithURL: &*u, cachePolicy: 1usize, timeoutInterval: timeout];
        let session: Retained<AnyObject> = msg_send![class!(NSURLSession), sharedSession];
        let tx2 = tx.clone();
        let block = RcBlock::new(move |data: *mut AnyObject, resp: *mut AnyObject, err: *mut AnyObject| {
            let r = if !err.is_null() {
                let d: Retained<NSString> = msg_send![&*err, localizedDescription];
                Err(d.to_string())
            } else {
                let code: isize = if resp.is_null() { 0 } else { msg_send![&*resp, statusCode] };
                if !(200..300).contains(&code) {
                    Err(format!("HTTP {code}"))
                } else if data.is_null() {
                    Err("no data".into())
                } else {
                    Ok((*(data as *const NSData)).to_vec())
                }
            };
            let _ = tx2.send(r);
        });
        let task: Retained<AnyObject> = msg_send![&*session, dataTaskWithRequest: &*req, completionHandler: &*block];
        let _: () = msg_send![&*task, resume];
    });
    drop(tx);
    rx.recv_timeout(Duration::from_secs_f64(timeout + 2.0)).unwrap_or_else(|_| Err("timed out".into()))
}

/// Open a URL with its default handler (main thread or not).
pub fn open_url(url: &str) {
    autoreleasepool(|_| unsafe {
        let u: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*ns(url)];
        if let Some(u) = u {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![&*ws, openURL: &*u];
        }
    })
}

pub fn open_path(path: &str) {
    autoreleasepool(|_| unsafe {
        let u: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*ns(path)];
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: bool = msg_send![&*ws, openURL: &*u];
    })
}

/// Is an app with this bundle id running? (any thread)
pub fn app_running(bundle_id: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let arr: Retained<AnyObject> = msg_send![class!(NSRunningApplication), runningApplicationsWithBundleIdentifier: &*ns(bundle_id)];
        let n: usize = msg_send![&*arr, count];
        n > 0
    })
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventPost(tap: u32, event: *const c_void);
}

/// Press and release a media key (NX_KEYTYPE_*): what the keyboard's
/// play/next/previous keys send. Main thread.
pub fn post_media_key(key: i32) {
    autoreleasepool(|_| unsafe {
        for down in [true, false] {
            let flags: usize = if down { 0xa00 } else { 0xb00 };
            let state: isize = if down { 0xa } else { 0xb };
            let data1: isize = ((key as isize) << 16) | (state << 8);
            let zero = objc2_foundation::NSPoint::new(0.0, 0.0);
            // NSEventTypeSystemDefined = 14, subtype 8 (NX_SUBTYPE_AUX_CONTROL_BUTTONS)
            let ev: Option<Retained<AnyObject>> = msg_send![class!(NSEvent),
                otherEventWithType: 14usize, location: zero, modifierFlags: flags, timestamp: 0.0f64,
                windowNumber: 0isize, context: std::ptr::null::<AnyObject>(), subtype: 8i16, data1: data1, data2: -1isize];
            if let Some(ev) = ev {
                let cg: *const c_void = msg_send![&*ev, CGEvent];
                if !cg.is_null() {
                    CGEventPost(0, cg);
                }
            }
        }
    })
}

/// Play a system sound by name ("Glass", "Hero", ...).
pub fn play_sound(name: &str) {
    autoreleasepool(|_| unsafe {
        let s: Option<Retained<AnyObject>> = msg_send![class!(NSSound), soundNamed: &*ns(name)];
        if let Some(s) = s {
            let _: Bool = msg_send![&*s, play];
        }
    })
}

/// Seconds east of UTC for the local time zone, now.
pub fn utc_offset() -> f64 {
    autoreleasepool(|_| unsafe {
        let tz: Retained<AnyObject> = msg_send![class!(NSTimeZone), localTimeZone];
        let s: isize = msg_send![&*tz, secondsFromGMT];
        s as f64
    })
}

/// Should temperatures be Fahrenheit (system setting / region)?
pub fn prefers_fahrenheit() -> bool {
    autoreleasepool(|_| unsafe {
        let d: Retained<AnyObject> = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let u: Option<Retained<NSString>> = msg_send![&*d, stringForKey: &*ns("AppleTemperatureUnit")];
        if let Some(u) = u {
            return u.to_string() == "Fahrenheit";
        }
        let loc: Retained<AnyObject> = msg_send![class!(NSLocale), currentLocale];
        let ms: Option<Retained<AnyObject>> = msg_send![&*loc, objectForKey: &*ns("kCFLocaleMeasurementSystemKey")];
        string_of(ms.as_deref()) == "U.S."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_helpers() {
        assert!(load_framework("/System/Library/Frameworks/EventKit.framework"));
        assert!(class_named("EKEventStore").is_some());
        assert!(class_named("NoSuchClassHopefully").is_none());
        // The test binary has no Info.plist usage keys.
        assert!(!info_plist_has("NSCameraUsageDescription"));
        assert!(utc_offset().abs() <= 14.0 * 3600.0);
        let _ = prefers_fahrenheit();
        assert!(!app_running("com.example.not-running"));
        assert!(http_get("not a url", 1.0).is_err());
    }
}
