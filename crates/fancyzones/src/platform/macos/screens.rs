//! Monitor enumeration via NSScreen, converted to top-left global
//! coordinates (the CoreGraphics / Accessibility space).

use anyhow::{bail, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSRect, NSString};

use super::ffi::*;
use crate::engine::Monitor;
use crate::geometry::Rect;

/// Convert a Cocoa rect (bottom-left origin, relative to the primary
/// screen) to a top-left-origin global rect.
pub fn cocoa_to_global(r: NSRect, primary_height: f64) -> Rect {
    let left = r.origin.x.round() as i32;
    let top = (primary_height - (r.origin.y + r.size.height)).round() as i32;
    Rect::from_xywh(left, top, r.size.width.round() as i32, r.size.height.round() as i32)
}

/// Inverse of [`cocoa_to_global`].
pub fn global_to_cocoa(r: Rect, primary_height: f64) -> NSRect {
    NSRect::new(
        objc2_foundation::NSPoint::new(r.left as f64, primary_height - r.bottom as f64),
        objc2_foundation::NSSize::new(r.width() as f64, r.height() as f64),
    )
}

pub(crate) struct RawScreen {
    pub frame: NSRect,
    pub visible: NSRect,
    pub scale: f64,
    pub display_id: u32,
    pub name: String,
}

/// Reads `+[NSScreen screens]`. NSScreen is documented for main-thread use
/// but its getters are routinely (and safely) read from other threads; we
/// use raw `msg_send!` so the worker thread can refresh monitor info.
pub(crate) fn raw_screens() -> Vec<RawScreen> {
    objc2::rc::autoreleasepool(|_| unsafe {
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
            out.push(RawScreen { frame, visible, scale, display_id, name });
        }
        out
    })
}

/// Stable-ish monitor id: vendor/model/serial of the display, falling back to
/// the CGDirectDisplayID.
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

pub fn monitors() -> Result<Vec<Monitor>> {
    let raw = raw_screens();
    if raw.is_empty() {
        bail!("no screens reported by NSScreen");
    }
    // screens[0] is the screen with the menu bar; its frame origin is (0,0).
    let primary_h = raw[0].frame.size.height;
    let main = unsafe { CGMainDisplayID() };
    let mut out: Vec<Monitor> = Vec::new();
    for s in &raw {
        let mut id = monitor_id(s.display_id);
        if out.iter().any(|m| m.id == id) {
            id = format!("{id}-{}", s.display_id);
        }
        out.push(Monitor {
            id,
            name: if s.name.is_empty() { format!("Display {}", s.display_id) } else { s.name.clone() },
            frame: cocoa_to_global(s.frame, primary_h),
            work_area: cocoa_to_global(s.visible, primary_h),
            scale: s.scale,
            primary: s.display_id == main,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSSize};

    #[test]
    fn coordinate_conversion() {
        // primary 1440x900; menu bar 25 high, dock 70 high at bottom
        let visible = NSRect::new(NSPoint::new(0.0, 70.0), NSSize::new(1440.0, 805.0));
        assert_eq!(cocoa_to_global(visible, 900.0), Rect::new(0, 25, 1440, 830));
        // secondary above the primary
        let above = NSRect::new(NSPoint::new(0.0, 900.0), NSSize::new(1920.0, 1080.0));
        assert_eq!(cocoa_to_global(above, 900.0), Rect::new(0, -1080, 1920, 0));
        let r = Rect::new(100, 25, 500, 830);
        assert_eq!(cocoa_to_global(global_to_cocoa(r, 900.0), 900.0), r);
    }
}
