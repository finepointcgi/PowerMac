//! Hand-written bindings: CoreFoundation timers, CoreGraphics paths /
//! colors / images / bitmap contexts, IOKit power sources, libdispatch.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

use crate::geometry::{PathEl, Rect};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopTimerRef = *const c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl From<Rect> for CGRect {
    fn from(r: Rect) -> Self {
        CGRect { origin: CGPoint { x: r.x, y: r.y }, size: CGSize { width: r.w, height: r.h } }
    }
}

macro_rules! cf_ptr {
    ($name:ident, $cname:literal) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy)]
        pub struct $name(pub *const c_void);
        unsafe impl Encode for $name {
            const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct($cname, &[]));
        }
        unsafe impl RefEncode for $name {
            const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
        }
    };
}

cf_ptr!(CgColor, "CGColor");
cf_ptr!(CgPath, "CGPath");
cf_ptr!(CgImage, "CGImage");
cf_ptr!(CgContext, "CGContext");

pub type CFRunLoopTimerCallBack = extern "C" fn(timer: CFRunLoopTimerRef, info: *mut c_void);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFAbsoluteTimeGetCurrent() -> f64;
    pub fn CFRunLoopTimerCreate(
        alloc: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: CFRunLoopTimerCallBack,
        context: *mut c_void,
    ) -> CFRunLoopTimerRef;
    pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    pub fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
    pub fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, date: f64);
    pub fn CFArrayGetCount(a: CFTypeRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    pub fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFNumberGetValue(n: CFTypeRef, the_type: isize, out: *mut c_void) -> u8;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFStringCompare(a: CFTypeRef, b: CFTypeRef, opts: usize) -> isize;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGPathCreateMutable() -> *const c_void;
    pub fn CGPathMoveToPoint(p: *const c_void, m: *const c_void, x: f64, y: f64);
    pub fn CGPathAddLineToPoint(p: *const c_void, m: *const c_void, x: f64, y: f64);
    pub fn CGPathAddCurveToPoint(p: *const c_void, m: *const c_void, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64);
    pub fn CGPathCloseSubpath(p: *const c_void);
    pub fn CGPathAddRoundedRect(p: *const c_void, m: *const c_void, r: CGRect, cw: f64, ch: f64);
    pub fn CGPathRelease(p: *const c_void);
    pub fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *const c_void;
    pub fn CGColorRelease(c: *const c_void);
    pub fn CGImageRetain(i: *const c_void) -> *const c_void;
    pub fn CGImageRelease(i: *const c_void);
    pub fn CGImageGetWidth(i: *const c_void) -> usize;
    pub fn CGImageGetHeight(i: *const c_void) -> usize;
    pub fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
    pub fn CGColorSpaceCreateWithName(name: CFStringRef) -> *const c_void;
    pub fn CGColorSpaceRelease(cs: *const c_void);
    pub fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, cs: *const c_void, info: u32) -> *const c_void;
    pub fn CGBitmapContextCreateImage(ctx: *const c_void) -> *const c_void;
    pub fn CGContextRelease(ctx: *const c_void);
    pub fn CGContextScaleCTM(ctx: *const c_void, sx: f64, sy: f64);
    pub fn CGContextTranslateCTM(ctx: *const c_void, tx: f64, ty: f64);
    pub fn CGContextSetRGBFillColor(ctx: *const c_void, r: f64, g: f64, b: f64, a: f64);
    pub fn CGContextFillRect(ctx: *const c_void, r: CGRect);
    pub fn CGContextAddPath(ctx: *const c_void, p: *const c_void);
    pub fn CGContextFillPath(ctx: *const c_void);
    pub fn CGContextStrokePath(ctx: *const c_void);
    pub fn CGContextClip(ctx: *const c_void);
    pub fn CGContextDrawImage(ctx: *const c_void, r: CGRect, img: *const c_void);
    pub fn CGContextSetRGBStrokeColor(ctx: *const c_void, r: f64, g: f64, b: f64, a: f64);
    pub fn CGContextSetLineWidth(ctx: *const c_void, w: f64);
    pub fn CGContextSetInterpolationQuality(ctx: *const c_void, q: i32);
    pub fn CGContextSetBlendMode(ctx: *const c_void, mode: i32);
    pub fn CGContextSaveGState(ctx: *const c_void);
    pub fn CGContextRestoreGState(ctx: *const c_void);
    pub fn CGContextDrawLinearGradient(ctx: *const c_void, g: *const c_void, start: CGPoint, end: CGPoint, opts: u32);
    pub fn CGGradientCreateWithColorComponents(cs: *const c_void, comps: *const f64, locs: *const f64, n: usize) -> *const c_void;
    pub fn CGGradientRelease(g: *const c_void);
    pub fn CGMainDisplayID() -> u32;
    pub fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    pub fn CGDisplayIsBuiltin(d: u32) -> u32;
    pub fn CGDisplayPixelsWide(d: u32) -> usize;
    pub fn CGDisplayPixelsHigh(d: u32) -> usize;
    pub fn CGDisplayCopyDisplayMode(d: u32) -> *const c_void;
    pub fn CGDisplayModeGetPixelWidth(m: *const c_void) -> usize;
    pub fn CGDisplayModeGetPixelHeight(m: *const c_void) -> usize;
    pub fn CGDisplayModeRelease(m: *const c_void);
}

pub const kCGBlendModeSourceIn: i32 = 18;
/// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big
pub const BITMAP_RGBA: u32 = 1 | (4 << 12);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFTypeRef;
    pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFTypeRef;
    pub fn IOPSGetProvidingPowerSourceType(blob: CFTypeRef) -> CFStringRef;
}

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}

#[link(name = "QuickLookThumbnailing", kind = "framework")]
extern "C" {}

extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
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

/// Owned CGColor.
pub struct Color(pub CgColor);

impl Color {
    pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        Color(CgColor(unsafe { CGColorCreateSRGB(r, g, b, a) }))
    }
    pub fn white(a: f64) -> Self {
        Self::rgba(1.0, 1.0, 1.0, a)
    }
    pub fn black(a: f64) -> Self {
        Self::rgba(0.0, 0.0, 0.0, a)
    }
}

impl Drop for Color {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe { CGColorRelease(self.0 .0) }
        }
    }
}

/// Owned CGPath.
pub struct Path(pub CgPath);

impl Path {
    /// Build from top-left path elements inside a layer of height `h`
    /// (flipped to the layer's y-up space).
    pub fn from_els(els: &[PathEl], h: f64) -> Self {
        let f = |p: crate::geometry::Point| (p.x, h - p.y);
        unsafe {
            let p = CGPathCreateMutable();
            let n = std::ptr::null();
            for e in els {
                match *e {
                    PathEl::Move(a) => {
                        let a = f(a);
                        CGPathMoveToPoint(p, n, a.0, a.1)
                    }
                    PathEl::Line(a) => {
                        let a = f(a);
                        CGPathAddLineToPoint(p, n, a.0, a.1)
                    }
                    PathEl::Cubic(a, b, c) => {
                        let (a, b, c) = (f(a), f(b), f(c));
                        CGPathAddCurveToPoint(p, n, a.0, a.1, b.0, b.1, c.0, c.1)
                    }
                    PathEl::Close => CGPathCloseSubpath(p),
                }
            }
            Path(CgPath(p))
        }
    }
    pub fn rounded_rect(r: Rect, radius: f64) -> Self {
        unsafe {
            let p = CGPathCreateMutable();
            let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
            CGPathAddRoundedRect(p, std::ptr::null(), r.into(), rad, rad);
            Path(CgPath(p))
        }
    }
}

impl Drop for Path {
    fn drop(&mut self) {
        unsafe { CGPathRelease(self.0 .0) }
    }
}

/// Retained CGImage.
pub struct Image(pub CgImage);

impl Image {
    /// Takes over a +1 reference.
    pub fn from_owned(p: *const c_void) -> Option<Self> {
        (!p.is_null()).then_some(Image(CgImage(p)))
    }
    pub fn retain(p: *const c_void) -> Option<Self> {
        (!p.is_null()).then(|| Image(CgImage(unsafe { CGImageRetain(p) })))
    }
    pub fn as_obj(&self) -> *mut AnyObject {
        self.0 .0 as *mut AnyObject
    }
}

impl Clone for Image {
    fn clone(&self) -> Self {
        Image(CgImage(unsafe { CGImageRetain(self.0 .0) }))
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { CGImageRelease(self.0 .0) }
    }
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn now() -> f64 {
    unsafe { CFAbsoluteTimeGetCurrent() }
}

/// Run `f` inside a CATransaction with implicit animations disabled (or
/// with the given duration).
pub fn transaction<R>(animate: Option<f64>, f: impl FnOnce() -> R) -> R {
    unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        match animate {
            None => {
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            }
            Some(d) => {
                let _: () = msg_send![class!(CATransaction), setAnimationDuration: d];
            }
        }
    }
    let r = f();
    unsafe {
        let _: () = msg_send![class!(CATransaction), commit];
    }
    r
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
