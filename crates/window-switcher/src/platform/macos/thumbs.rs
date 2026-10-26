//! Window thumbnails, captured asynchronously and cached.
//!
//! ScreenCaptureKit (`SCShareableContent` + `SCScreenshotManager`, macOS 14+)
//! is used when present; otherwise the old `CGWindowListCreateImage`
//! (resolved at runtime because newer SDKs mark it obsolete) on a
//! background thread. Both need the Screen Recording permission; without
//! it [`has_permission`] is false and the switcher shows app icons.
//! ScreenCaptureKit is loaded with `dlopen` and driven through the
//! Objective-C runtime so nothing is linked at build time.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::NSArray;

use super::ffi::*;

/// Longest edge of a captured thumbnail, in pixels.
pub const MAX_EDGE: f64 = 720.0;
/// Longest edge of a full-resolution preview capture, in pixels.
pub const MAX_EDGE_HIRES: f64 = 5120.0;
/// Full-resolution captures kept (the selected window and its neighbours).
const HIRES_KEEP: usize = 6;

struct Cached {
    image: Image,
    at: Instant,
}

fn cache_of(hires: bool) -> &'static Mutex<HashMap<u32, Cached>> {
    static C: OnceLock<Mutex<HashMap<u32, Cached>>> = OnceLock::new();
    static H: OnceLock<Mutex<HashMap<u32, Cached>>> = OnceLock::new();
    if hires { H.get_or_init(Default::default) } else { C.get_or_init(Default::default) }
}

/// Cached thumbnail of a window, if any.
pub fn cached(wid: u32) -> Option<Image> {
    cache_of(false).lock().unwrap_or_else(|e| e.into_inner()).get(&wid).map(|c| c.image.clone())
}

/// Cached full-resolution capture of a window (for the preview), if any.
pub fn cached_hires(wid: u32) -> Option<Image> {
    cache_of(true).lock().unwrap_or_else(|e| e.into_inner()).get(&wid).map(|c| c.image.clone())
}

/// The sharpest image of a window we have.
pub fn best(wid: u32) -> Option<Image> {
    cached_hires(wid).or_else(|| cached(wid))
}

/// Drop thumbnails of windows that are gone.
pub fn retain(alive: impl Fn(u32) -> bool) {
    for hires in [false, true] {
        cache_of(hires).lock().unwrap_or_else(|e| e.into_inner()).retain(|w, _| alive(*w));
    }
}

fn store(wid: u32, image: Image, hires: bool) {
    let mut c = cache_of(hires).lock().unwrap_or_else(|e| e.into_inner());
    c.insert(wid, Cached { image, at: Instant::now() });
    if hires {
        while c.len() > HIRES_KEEP {
            let Some(oldest) = c.iter().min_by_key(|(_, v)| v.at).map(|(k, _)| *k) else { break };
            c.remove(&oldest);
        }
    }
}

fn fresh(wid: u32, max_age: Duration, hires: bool) -> bool {
    cache_of(hires).lock().unwrap_or_else(|e| e.into_inner()).get(&wid).is_some_and(|c| c.at.elapsed() < max_age)
}

/// Screen Recording granted? Never prompts. Cached for two seconds.
pub fn has_permission() -> bool {
    static C: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut c = C.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *c {
        if t.elapsed() < Duration::from_secs(2) {
            return v;
        }
    }
    let v = unsafe { CGPreflightScreenCaptureAccess() };
    *c = Some((Instant::now(), v));
    v
}

type Done = Arc<dyn Fn(u32, Image) + Send + Sync>;

/// A window to capture: id and size in points (for the aspect ratio).
#[derive(Debug, Clone, Copy)]
pub struct Request {
    pub wid: u32,
    pub w: f64,
    pub h: f64,
    /// Skip if the cached image is younger than this.
    pub max_age: Duration,
    /// Full resolution for the preview (own cache) instead of a thumbnail.
    pub hires: bool,
}

impl Request {
    pub fn thumbnail(wid: u32, w: f64, h: f64) -> Self {
        Self { wid, w, h, max_age: Duration::from_millis(500), hires: false }
    }

    pub fn full(wid: u32, w: f64, h: f64) -> Self {
        Self { wid, w, h, max_age: Duration::from_millis(400), hires: true }
    }

    /// Output size in pixels (aspect kept, longest edge capped).
    pub fn pixel_size(&self) -> (usize, usize) {
        let (w, h) = (self.w.max(1.0), self.h.max(1.0));
        let k = if self.hires { (MAX_EDGE_HIRES / w.max(h)).min(2.0) } else { (MAX_EDGE / w.max(h)).min(2.0) };
        (((w * k).round().max(1.0)) as usize, ((h * k).round().max(1.0)) as usize)
    }
}

/// Capture the given windows in the background; `done` runs (on some
/// background thread) for every new image, after it's cached.
pub fn capture(reqs: Vec<Request>, done: impl Fn(u32, Image) + Send + Sync + 'static) {
    if !has_permission() {
        return;
    }
    let reqs: Vec<Request> = reqs.into_iter().filter(|r| !fresh(r.wid, r.max_age, r.hires)).collect();
    if reqs.is_empty() {
        return;
    }
    let done: Done = Arc::new(done);
    if sck_available() {
        sck_capture(reqs, done);
    } else if let Some(f) = cg_create_image() {
        std::thread::Builder::new()
            .name("window-switcher-thumbs".into())
            .spawn(move || {
                for r in reqs {
                    let img = unsafe {
                        let null = CGRect::new(f64::INFINITY, f64::INFINITY, 0.0, 0.0);
                        f(null, kCGWindowListOptionIncludingWindow, r.wid, kCGWindowImageBoundsIgnoreFraming | if r.hires { 0 } else { kCGWindowImageNominalResolution })
                    };
                    if let Some(img) = unsafe { Image::from_owned(img) } {
                        store(r.wid, img.clone(), r.hires);
                        done(r.wid, img);
                    }
                }
            })
            .ok();
    }
}

type CgCreateImageFn = unsafe extern "C" fn(CGRect, u32, u32, u32) -> CGImageRef;

fn cg_create_image() -> Option<CgCreateImageFn> {
    static F: OnceLock<Option<CgCreateImageFn>> = OnceLock::new();
    *F.get_or_init(|| {
        let p = find_symbol(c"CGWindowListCreateImage");
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, CgCreateImageFn>(p) })
    })
}

fn sck_available() -> bool {
    static A: OnceLock<bool> = OnceLock::new();
    *A.get_or_init(|| {
        load_framework(c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
            && AnyClass::get(c"SCShareableContent").is_some()
            && AnyClass::get(c"SCScreenshotManager").is_some()
            && AnyClass::get(c"SCContentFilter").is_some()
            && AnyClass::get(c"SCStreamConfiguration").is_some()
    })
}

fn sck_capture(reqs: Vec<Request>, done: Done) {
    let Some(content_cls) = AnyClass::get(c"SCShareableContent") else { return };
    let reqs = Mutex::new(Some(reqs));
    let handler = RcBlock::new(move |content: *mut AnyObject, _err: *mut AnyObject| {
        let Some(reqs) = reqs.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if content.is_null() {
            return;
        }
        autoreleasepool(|_| unsafe {
            let content = &*content;
            let windows: Option<Retained<NSArray<AnyObject>>> = msg_send![content, windows];
            let Some(windows) = windows else { return };
            for r in &reqs {
                let scw = windows.iter().find(|w| {
                    let id: u32 = msg_send![&**w, windowID];
                    id == r.wid
                });
                if let Some(scw) = scw {
                    sck_shoot(&scw, *r, done.clone());
                }
            }
        });
    });
    unsafe {
        let _: () = msg_send![
            content_cls,
            getShareableContentExcludingDesktopWindows: true,
            onScreenWindowsOnly: false,
            completionHandler: &*handler
        ];
    }
}

unsafe fn sck_shoot(scw: &AnyObject, r: Request, done: Done) {
    let (Some(filter_cls), Some(conf_cls), Some(mgr)) =
        (AnyClass::get(c"SCContentFilter"), AnyClass::get(c"SCStreamConfiguration"), AnyClass::get(c"SCScreenshotManager"))
    else {
        return;
    };
    let alloc: Allocated<AnyObject> = msg_send![filter_cls, alloc];
    let filter: Option<Retained<AnyObject>> = msg_send![alloc, initWithDesktopIndependentWindow: scw];
    let Some(filter) = filter else { return };
    let conf: Retained<AnyObject> = msg_send![conf_cls, new];
    let (pw, ph) = r.pixel_size();
    let _: () = msg_send![&*conf, setWidth: pw];
    let _: () = msg_send![&*conf, setHeight: ph];
    let _: () = msg_send![&*conf, setShowsCursor: false];
    let sel = objc2::sel!(setIgnoreShadowsSingleWindow:);
    let responds: bool = msg_send![&*conf, respondsToSelector: sel];
    if responds {
        let _: () = msg_send![&*conf, setIgnoreShadowsSingleWindow: true];
    }
    let (wid, hires) = (r.wid, r.hires);
    let handler = RcBlock::new(move |img: *const c_void, _err: *mut AnyObject| {
        if let Some(img) = unsafe { Image::from_borrowed(img) } {
            store(wid, img.clone(), hires);
            done(wid, img);
        }
    });
    let _: () = msg_send![mgr, captureImageWithFilter: &*filter, configuration: &*conf, completionHandler: &*handler];
}
