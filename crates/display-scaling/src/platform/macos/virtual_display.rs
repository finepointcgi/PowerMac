//! The private `CGVirtualDisplay` API (CoreGraphics, macOS 11+), looked up
//! through the Objective-C runtime so a missing class only disables HiDPI.
//! This is what BetterDisplay, DeskPad and Chromium's tests use:
//!
//! ```text
//! CGVirtualDisplayDescriptor  queue, name, maxPixelsWide/High, sizeInMillimeters,
//!                             vendorID, productID, serialNum
//! CGVirtualDisplay            -initWithDescriptor:, -applySettings:, displayID
//! CGVirtualDisplaySettings    hiDPI, modes
//! CGVirtualDisplayMode        -initWithWidth:height:refreshRate:
//! ```
//!
//! The display exists while the `CGVirtualDisplay` object is alive (and
//! disappears with the process).

use std::ffi::{c_char, c_void, CString};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{msg_send, sel};

use super::ffi::{online_displays, CGDisplayCopyDisplayMode, CGDisplayModeRelease};

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
}
extern "C" {
    fn pthread_main_np() -> i32;
}

/// Wait a little; on the main thread, run its run loop meanwhile
/// (WindowServer's display notifications are delivered through it).
pub fn pump(d: Duration) {
    if unsafe { pthread_main_np() } != 0 {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, d.as_secs_f64(), 0) };
    } else {
        std::thread::sleep(d);
    }
}

fn has_mode(d: u32) -> bool {
    let m = unsafe { CGDisplayCopyDisplayMode(d) };
    if m.is_null() {
        return false;
    }
    unsafe { CGDisplayModeRelease(m) };
    true
}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // Referenced so CoreGraphics (which holds the classes) is loaded.
    fn CGMainDisplayID() -> u32;
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut AnyObject;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

struct Classes {
    descriptor: &'static AnyClass,
    display: &'static AnyClass,
    settings: &'static AnyClass,
    mode: &'static AnyClass,
}

fn classes() -> Option<Classes> {
    let _ = unsafe { CGMainDisplayID() };
    let c = Classes {
        descriptor: AnyClass::get(c"CGVirtualDisplayDescriptor")?,
        display: AnyClass::get(c"CGVirtualDisplay")?,
        settings: AnyClass::get(c"CGVirtualDisplaySettings")?,
        mode: AnyClass::get(c"CGVirtualDisplayMode")?,
    };
    let needed: [(&AnyClass, Sel); 12] = [
        (c.descriptor, sel!(setQueue:)),
        (c.descriptor, sel!(setName:)),
        (c.descriptor, sel!(setMaxPixelsWide:)),
        (c.descriptor, sel!(setMaxPixelsHigh:)),
        (c.descriptor, sel!(setSizeInMillimeters:)),
        (c.descriptor, sel!(setVendorID:)),
        (c.descriptor, sel!(setProductID:)),
        (c.descriptor, sel!(setSerialNum:)),
        (c.display, sel!(initWithDescriptor:)),
        (c.display, sel!(applySettings:)),
        (c.settings, sel!(setModes:)),
        (c.mode, sel!(initWithWidth:height:refreshRate:)),
    ];
    for (cls, s) in needed {
        if cls.instance_method(s).is_none() {
            log::warn!("display-scaling: {:?} lacks {}", cls.name(), s.name().to_string_lossy());
            return None;
        }
    }
    if c.settings.instance_method(sel!(setHiDPI:)).is_none() || c.display.instance_method(sel!(displayID)).is_none() {
        return None;
    }
    Some(c)
}

/// Whether the private API is present.
pub fn available() -> bool {
    classes().is_some()
}

/// Method type encodings of the API (diagnostics).
pub fn signatures() -> Vec<String> {
    let Some(c) = classes() else { return vec!["CGVirtualDisplay API not found".into()] };
    let list: [(&AnyClass, Sel); 7] = [
        (c.descriptor, sel!(setMaxPixelsWide:)),
        (c.descriptor, sel!(setSizeInMillimeters:)),
        (c.descriptor, sel!(setSerialNum:)),
        (c.display, sel!(applySettings:)),
        (c.display, sel!(displayID)),
        (c.settings, sel!(setHiDPI:)),
        (c.mode, sel!(initWithWidth:height:refreshRate:)),
    ];
    list.iter()
        .filter_map(|(cls, s)| {
            let m = cls.instance_method(*s)?;
            let args: Vec<String> = (2..m.arguments_count()).filter_map(|i| m.argument_type(i).map(|a| a.to_string_lossy().into_owned())).collect();
            Some(format!("{}.{} -> {} ({})", cls.name().to_string_lossy(), s.name().to_string_lossy(), m.return_type().to_string_lossy(), args.join(", ")))
        })
        .collect()
}

fn nsstring(s: &str) -> Result<Retained<AnyObject>> {
    let cls = AnyClass::get(c"NSString").ok_or_else(|| anyhow!("NSString missing"))?;
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    let a: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
    let p: *const c_char = c.as_ptr();
    let o: Option<Retained<AnyObject>> = unsafe { msg_send![a, initWithUTF8String: p] };
    o.ok_or_else(|| anyhow!("NSString init failed"))
}

/// One mode of the virtual display: a logical size. With `hidpi` macOS
/// offers it both as a HiDPI mode (2x backing store, if that fits the
/// descriptor's max pixels) and as a 1x mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VMode {
    pub width: u32,
    pub height: u32,
    pub refresh: f64,
}

pub struct VirtualDisplay {
    obj: Retained<AnyObject>,
    _queue: Retained<AnyObject>,
    pub display_id: u32,
}

pub struct Descriptor<'a> {
    pub name: &'a str,
    pub vendor: u32,
    pub product: u32,
    pub serial: u32,
    pub max_pixels: (u32, u32),
    pub size_mm: (f64, f64),
}

impl VirtualDisplay {
    /// Create the display (it shows up as an extended desktop until modes
    /// are applied and it is mirrored or arranged).
    pub fn create(d: &Descriptor) -> Result<Self> {
        let c = classes().ok_or_else(|| anyhow!("CGVirtualDisplay API not available (macOS 11+ needed)"))?;
        objc2::rc::autoreleasepool(|_| unsafe {
            let q = dispatch_queue_create(c"PowerMac.display-scaling.virtual".as_ptr(), std::ptr::null());
            let queue = Retained::from_raw(q).ok_or_else(|| anyhow!("dispatch_queue_create failed"))?;
            let desc: Retained<AnyObject> = msg_send![c.descriptor, new];
            let name = nsstring(d.name)?;
            let _: () = msg_send![&*desc, setQueue: &*queue];
            let _: () = msg_send![&*desc, setName: &*name];
            let _: () = msg_send![&*desc, setMaxPixelsWide: d.max_pixels.0];
            let _: () = msg_send![&*desc, setMaxPixelsHigh: d.max_pixels.1];
            let (w, h) = if d.size_mm.0 > 10.0 && d.size_mm.1 > 10.0 { d.size_mm } else { (600.0, 340.0) };
            let _: () = msg_send![&*desc, setSizeInMillimeters: CGSize { width: w, height: h }];
            let _: () = msg_send![&*desc, setVendorID: d.vendor];
            let _: () = msg_send![&*desc, setProductID: d.product];
            let _: () = msg_send![&*desc, setSerialNum: d.serial];
            let a: Allocated<AnyObject> = msg_send![c.display, alloc];
            let obj: Option<Retained<AnyObject>> = msg_send![a, initWithDescriptor: &*desc];
            let obj = obj.ok_or_else(|| anyhow!("CGVirtualDisplay initWithDescriptor: returned nil"))?;
            let display_id: u32 = msg_send![&*obj, displayID];
            if display_id == 0 {
                bail!("CGVirtualDisplay has no display id");
            }
            Ok(Self { obj, _queue: queue, display_id })
        })
    }

    /// Replace the display's modes.
    pub fn apply_modes(&self, modes: &[VMode], hidpi: bool) -> Result<()> {
        let c = classes().ok_or_else(|| anyhow!("CGVirtualDisplay API not available"))?;
        let arr_cls = AnyClass::get(c"NSMutableArray").ok_or_else(|| anyhow!("NSMutableArray missing"))?;
        objc2::rc::autoreleasepool(|_| unsafe {
            let arr: Retained<AnyObject> = msg_send![arr_cls, new];
            for m in modes {
                let a: Allocated<AnyObject> = msg_send![c.mode, alloc];
                let mode: Option<Retained<AnyObject>> = msg_send![a, initWithWidth: m.width, height: m.height, refreshRate: m.refresh];
                let mode = mode.ok_or_else(|| anyhow!("CGVirtualDisplayMode init failed"))?;
                let _: () = msg_send![&*arr, addObject: &*mode];
            }
            let settings: Retained<AnyObject> = msg_send![c.settings, new];
            let _: () = msg_send![&*settings, setHiDPI: hidpi as u32];
            let _: () = msg_send![&*settings, setModes: &*arr];
            let ok: Bool = msg_send![&*self.obj, applySettings: &*settings];
            if !ok.as_bool() {
                bail!("CGVirtualDisplay applySettings: failed");
            }
            Ok(())
        })
    }

    /// Wait until WindowServer lists the display (it appears asynchronously).
    pub fn wait_online(&self, timeout: Duration) -> bool {
        wait(timeout, || online_displays().contains(&self.display_id))
    }

    /// Wait until this process can see the display's modes. CoreGraphics
    /// refreshes its per-process display cache when WindowServer's
    /// reconfiguration notification is handled, which needs the main
    /// thread's run loop (the tray app's) to be running.
    pub fn wait_modes(&self, timeout: Duration) -> bool {
        wait(timeout, || has_mode(self.display_id))
    }
}

fn wait(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        pump(Duration::from_millis(25));
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        log::debug!("display-scaling: destroying virtual display {}", self.display_id);
    }
}
