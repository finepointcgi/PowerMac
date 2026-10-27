//! Monitors via NSScreen, in top-left global coordinates, with the same ids
//! FancyZones uses (`crates/fancyzones/src/platform/macos/screens.rs`).

use anyhow::{bail, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSRect, NSString};

use super::ffi::*;
use crate::geometry::{Monitor, Rect};

fn cocoa_to_global(r: NSRect, primary_height: f64) -> Rect {
    let left = r.origin.x.round() as i32;
    let top = (primary_height - (r.origin.y + r.size.height)).round() as i32;
    Rect::new(left, top, r.size.width.round() as i32, r.size.height.round() as i32)
}

/// FancyZones' monitor id: vendor/model/serial of the display, falling
/// back to the CGDirectDisplayID.
fn monitor_id(display: u32) -> String {
    unsafe {
        let (v, m, s) = (CGDisplayVendorNumber(display), CGDisplayModelNumber(display), CGDisplaySerialNumber(display));
        if v == 0 && m == 0 && s == 0 {
            format!("display-{display}")
        } else {
            format!("{v:04X}-{m:04X}-{s:08X}")
        }
    }
}

/// Monitors with their CGDirectDisplayID (screens[0], the menu bar screen,
/// first).
pub fn monitors_with_displays() -> Result<Vec<(Monitor, u32)>> {
    let raw: Vec<(NSRect, NSRect, f64, u32, String)> = objc2::rc::autoreleasepool(|_| unsafe {
        let screens: Option<Retained<NSArray<AnyObject>>> = msg_send![class!(NSScreen), screens];
        let Some(screens) = screens else { return vec![] };
        let key = NSString::from_str("NSScreenNumber");
        let mut out = Vec::new();
        for s in screens.iter() {
            let frame: NSRect = msg_send![&*s, frame];
            let visible: NSRect = msg_send![&*s, visibleFrame];
            let scale: f64 = msg_send![&*s, backingScaleFactor];
            let desc: Option<Retained<AnyObject>> = msg_send![&*s, deviceDescription];
            let display_id = desc
                .and_then(|d| {
                    let n: Option<Retained<AnyObject>> = msg_send![&*d, objectForKey: &*key];
                    n
                })
                .map(|n| {
                    let v: u32 = msg_send![&*n, unsignedIntValue];
                    v
                })
                .unwrap_or(0);
            let responds: bool = msg_send![&*s, respondsToSelector: objc2::sel!(localizedName)];
            let name = if responds {
                let n: Option<Retained<NSString>> = msg_send![&*s, localizedName];
                n.map(|n| n.to_string()).unwrap_or_default()
            } else {
                String::new()
            };
            out.push((frame, visible, scale, display_id, name));
        }
        out
    });
    if raw.is_empty() {
        bail!("no screens reported by NSScreen");
    }
    let primary_h = raw[0].0.size.height;
    let main = unsafe { CGMainDisplayID() };
    let mut out: Vec<(Monitor, u32)> = Vec::new();
    for (i, (frame, visible, scale, display, name)) in raw.into_iter().enumerate() {
        let mut id = monitor_id(display);
        if out.iter().any(|(m, _)| m.id == id) {
            id = format!("{id}-{display}");
        }
        out.push((
            Monitor {
                id,
                name: if name.is_empty() { format!("Display {display}") } else { name },
                number: i as u32,
                frame: cocoa_to_global(frame, primary_h),
                work_area: cocoa_to_global(visible, primary_h),
                scale,
                primary: display == main,
            },
            display,
        ));
    }
    Ok(out)
}

pub fn monitors() -> Result<Vec<Monitor>> {
    Ok(monitors_with_displays()?.into_iter().map(|(m, _)| m).collect())
}
