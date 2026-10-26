//! Capturing a screen region. In order of preference:
//! 1. `SCScreenshotManager captureImageInRect:` (macOS 15.2+),
//! 2. `SCScreenshotManager captureImageWithFilter:configuration:` for the
//!    display under the region (macOS 14+),
//! 3. `CGWindowListCreateImage` (older systems; looked up at run time since
//!    newer SDKs mark it unavailable).
//!
//! All of them need the Screen Recording permission.

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSError, NSPoint, NSRect, NSSize};

use super::cg::{CGImageRetain, CgImage};

/// A rectangle in global display coordinates (points, origin at the top-left
/// of the main display).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl GRect {
    fn ns(&self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }
}

const TIMEOUT: Duration = Duration::from_secs(6);

fn load_screencapturekit() {
    extern "C" {
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // RTLD_LAZY | RTLD_GLOBAL; harmless if missing (pre-12.3).
        dlopen(
            c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit".as_ptr(),
            0x1 | 0x8,
        );
    });
}

fn class_responds(cls: &AnyClass, s: objc2::runtime::Sel) -> bool {
    let r: Bool = unsafe { msg_send![cls, respondsToSelector: s] };
    r.as_bool()
}

fn err_text(e: *mut NSError) -> String {
    unsafe { e.as_ref() }
        .map(|e| e.localizedDescription().to_string())
        .unwrap_or_else(|| "unknown error".into())
}

type ImageResult = std::result::Result<usize, String>;

/// A completion handler `(CGImageRef, NSError*)` that sends the (retained) image.
fn image_handler(tx: mpsc::Sender<ImageResult>) -> RcBlock<dyn Fn(*const c_void, *mut NSError)> {
    RcBlock::new(move |img: *const c_void, err: *mut NSError| {
        let r = if img.is_null() {
            Err(err_text(err))
        } else {
            unsafe { CGImageRetain(img) };
            Ok(img as usize)
        };
        let _ = tx.send(r);
    })
}

fn wait(rx: mpsc::Receiver<ImageResult>) -> Result<CgImage> {
    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(p)) => unsafe { CgImage::from_owned(p as *const c_void) }
            .ok_or_else(|| anyhow!("empty capture")),
        Ok(Err(e)) => bail!("screen capture failed: {e}"),
        Err(_) => bail!("screen capture timed out"),
    }
}

/// macOS 15.2+.
fn capture_in_rect(r: GRect) -> Option<Result<CgImage>> {
    let cls = AnyClass::get(c"SCScreenshotManager")?;
    if !class_responds(cls, sel!(captureImageInRect:completionHandler:)) {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let block = image_handler(tx);
    let _: () = unsafe { msg_send![cls, captureImageInRect: r.ns(), completionHandler: &*block] };
    Some(wait(rx))
}

/// macOS 14+: capture the display under the region with a source rect.
fn capture_with_filter(r: GRect) -> Option<Result<CgImage>> {
    let mgr = AnyClass::get(c"SCScreenshotManager")?;
    let content_cls = AnyClass::get(c"SCShareableContent")?;
    if !class_responds(
        mgr,
        sel!(captureImageWithFilter:configuration:completionHandler:),
    ) {
        return None;
    }
    Some(autoreleasepool(|_| -> Result<CgImage> {
        // Shareable content (the displays).
        let (tx, rx) = mpsc::channel::<std::result::Result<Retained<AnyObject>, String>>();
        let block = RcBlock::new(move |content: *mut AnyObject, err: *mut NSError| {
            let r = match unsafe { Retained::retain(content) } {
                Some(c) => Ok(c),
                None => Err(err_text(err)),
            };
            let _ = tx.send(r);
        });
        let _: () =
            unsafe { msg_send![content_cls, getShareableContentWithCompletionHandler: &*block] };
        let content = match rx.recv_timeout(TIMEOUT) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => bail!("screen capture failed: {e}"),
            Err(_) => bail!("screen capture timed out"),
        };
        let displays: Retained<NSArray<AnyObject>> = unsafe { msg_send![&*content, displays] };
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let display = displays
            .iter()
            .find(|d| {
                let f: NSRect = unsafe { msg_send![&**d, frame] };
                cx >= f.origin.x
                    && cx < f.origin.x + f.size.width
                    && cy >= f.origin.y
                    && cy < f.origin.y + f.size.height
            })
            .or_else(|| displays.firstObject())
            .ok_or_else(|| anyhow!("no display to capture"))?;
        let frame: NSRect = unsafe { msg_send![&*display, frame] };
        let empty = NSArray::<AnyObject>::new();
        let alloc: Allocated<AnyObject> = unsafe {
            msg_send![
                AnyClass::get(c"SCContentFilter")
                    .ok_or_else(|| anyhow!("SCContentFilter missing"))?,
                alloc
            ]
        };
        let filter: Retained<AnyObject> =
            unsafe { msg_send![alloc, initWithDisplay: &*display, excludingWindows: &*empty] };
        let scale: f32 = unsafe { msg_send![&*filter, pointPixelScale] };
        let scale = if scale > 0.0 { scale as f64 } else { 2.0 };
        let cfg: Retained<AnyObject> = unsafe {
            msg_send![
                AnyClass::get(c"SCStreamConfiguration")
                    .ok_or_else(|| anyhow!("SCStreamConfiguration missing"))?,
                new
            ]
        };
        let local = GRect {
            x: r.x - frame.origin.x,
            y: r.y - frame.origin.y,
            w: r.w,
            h: r.h,
        };
        unsafe {
            let _: () = msg_send![&*cfg, setSourceRect: local.ns()];
            let _: () = msg_send![&*cfg, setWidth: (r.w * scale).round().max(1.0) as usize];
            let _: () = msg_send![&*cfg, setHeight: (r.h * scale).round().max(1.0) as usize];
            let _: () = msg_send![&*cfg, setShowsCursor: false];
        }
        let (tx, rx) = mpsc::channel();
        let block = image_handler(tx);
        let _: () = unsafe {
            msg_send![mgr, captureImageWithFilter: &*filter, configuration: &*cfg, completionHandler: &*block]
        };
        wait(rx)
    }))
}

/// Pre-14 fallback.
fn capture_window_list(r: GRect) -> Result<CgImage> {
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    }
    type CreateImage = unsafe extern "C" fn(NSRect, u32, u32, u32) -> *const c_void;
    // RTLD_DEFAULT
    let f = unsafe { dlsym(-2isize as *mut c_void, c"CGWindowListCreateImage".as_ptr()) };
    if f.is_null() {
        bail!("no screen capture API available");
    }
    let f: CreateImage = unsafe { std::mem::transmute(f) };
    // kCGWindowListOptionOnScreenOnly, kCGNullWindowID, kCGWindowImageBestResolution
    let img = unsafe { f(r.ns(), 1, 0, 1 << 3) };
    unsafe { CgImage::from_owned(img) }.ok_or_else(|| anyhow!("screen capture failed"))
}

/// Capture `r` (global points). Call off the main thread.
///
/// `PM_TE_CAPTURE=filter|windowlist` forces an older path (diagnostics).
pub fn capture(r: GRect) -> Result<CgImage> {
    load_screencapturekit();
    let force = std::env::var("PM_TE_CAPTURE").unwrap_or_default();
    if force.is_empty() {
        if let Some(res) = capture_in_rect(r) {
            return res;
        }
    }
    if force.is_empty() || force == "filter" {
        if let Some(res) = capture_with_filter(r) {
            return res;
        }
    }
    capture_window_list(r)
}
