//! NSWorkspace / NSRunningApplication helpers (callable from any thread),
//! app icons and the list of installed apps.

use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSRect, NSString, NSURL};

use super::ffi::*;
use crate::matching::InstalledApp;

/// A running app with a Dock presence.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub pid: i32,
    pub name: String,
    pub bundle_id: String,
    pub path: String,
    pub hidden: bool,
}

fn running_app(pid: i32) -> Option<Retained<AnyObject>> {
    unsafe { msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid] }
}

fn info(a: &AnyObject) -> Option<AppInfo> {
    unsafe {
        let policy: isize = msg_send![a, activationPolicy];
        let pid: i32 = msg_send![a, processIdentifier];
        let terminated: bool = msg_send![a, isTerminated];
        if policy != 0 || pid <= 0 || terminated {
            return None;
        }
        let name: Option<Retained<NSString>> = msg_send![a, localizedName];
        let bid: Option<Retained<NSString>> = msg_send![a, bundleIdentifier];
        let url: Option<Retained<NSURL>> = msg_send![a, bundleURL];
        let path = url.and_then(|u| u.path()).map(|p| p.to_string()).unwrap_or_default();
        let hidden: bool = msg_send![a, isHidden];
        Some(AppInfo {
            pid,
            name: name.map(|s| s.to_string()).unwrap_or_default(),
            bundle_id: bid.map(|s| s.to_string()).unwrap_or_default(),
            path,
            hidden,
        })
    }
}

/// Regular (Dock) apps among `pids`.
///
/// `NSWorkspace.runningApplications` is only refreshed by notifications on
/// the main run loop, which a CLI launch (or a worker thread) doesn't run,
/// so apps started after the first call would be missing. The pids come
/// from the window list instead and each is looked up on its own.
pub fn regular_apps_for(pids: &[i32]) -> Vec<AppInfo> {
    autoreleasepool(|_| pids.iter().filter_map(|&pid| running_app(pid).and_then(|a| info(&a))).collect())
}

/// Whether the app is hidden right now.
pub fn is_hidden(pid: i32) -> bool {
    autoreleasepool(|_| unsafe { running_app(pid).map(|a| msg_send![&*a, isHidden]).unwrap_or(false) })
}

pub fn set_hidden(pid: i32, hide: bool) -> bool {
    autoreleasepool(|_| unsafe {
        match running_app(pid) {
            Some(app) => {
                if hide {
                    msg_send![&*app, hide]
                } else {
                    msg_send![&*app, unhide]
                }
            }
            None => false,
        }
    })
}

type SetFrontFn = unsafe extern "C" fn(*const ProcessSerialNumber, u32, u32) -> i32;
type PostEventRecordFn = unsafe extern "C" fn(*const ProcessSerialNumber, *const u8) -> i32;

struct SlpsApi {
    set_front: SetFrontFn,
    post_event_record: PostEventRecordFn,
}

fn slps() -> Option<&'static SlpsApi> {
    static API: OnceLock<Option<SlpsApi>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let f = find_symbol(c"_SLPSSetFrontProcessWithOptions");
        let p = find_symbol(c"SLPSPostEventRecordTo");
        if f.is_null() || p.is_null() {
            return None;
        }
        Some(SlpsApi {
            set_front: std::mem::transmute::<*mut c_void, SetFrontFn>(f),
            post_event_record: std::mem::transmute::<*mut c_void, PostEventRecordFn>(p),
        })
    })
    .as_ref()
}

/// Make `wid` of `pid` the frontmost window (AltTab's approach, as in the
/// window switcher: SkyLight's front-process call plus a "make key window"
/// event record; works when this process isn't active itself), falling
/// back to `NSRunningApplication.activate`.
pub fn bring_front(pid: i32, wid: u32) {
    if let Some(api) = slps() {
        let mut psn = ProcessSerialNumber::default();
        if unsafe { GetProcessForPID(pid, &mut psn) } == 0 {
            unsafe {
                // kCPSUserGenerated
                (api.set_front)(&psn, wid, 0x200);
                let mut bytes = [0u8; 0xf8];
                bytes[0x04] = 0xf8;
                bytes[0x3a] = 0x10;
                bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
                for b in &mut bytes[0x20..0x30] {
                    *b = 0xff;
                }
                bytes[0x08] = 0x01;
                (api.post_event_record)(&psn, bytes.as_ptr());
                bytes[0x08] = 0x02;
                (api.post_event_record)(&psn, bytes.as_ptr());
            }
            return;
        }
    }
    autoreleasepool(|_| unsafe {
        if let Some(app) = running_app(pid) {
            // NSApplicationActivateIgnoringOtherApps (deprecated, still honoured).
            let _: bool = msg_send![&*app, activateWithOptions: 2usize];
        }
    })
}

/// The app's icon for `path` (an .app bundle or any file), as premultiplied
/// RGBA `size`×`size` pixels.
pub fn icon_rgba(path: &str, bundle_id: &str, size: u32) -> Option<Vec<u8>> {
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let mut p = path.to_string();
        if (p.is_empty() || !Path::new(&p).exists()) && !bundle_id.is_empty() {
            let bid = NSString::from_str(bundle_id);
            let url: Option<Retained<NSURL>> = msg_send![&*ws, URLForApplicationWithBundleIdentifier: &*bid];
            p = url.and_then(|u| u.path()).map(|s| s.to_string()).unwrap_or_default();
        }
        if p.is_empty() || !Path::new(&p).exists() {
            return None;
        }
        let ns = NSString::from_str(&p);
        let img: Option<Retained<AnyObject>> = msg_send![&*ws, iconForFile: &*ns];
        let img = img?;
        let mut rect = NSRect::new(objc2_foundation::NSPoint::new(0.0, 0.0), objc2_foundation::NSSize::new(size as f64, size as f64));
        let nil: *const AnyObject = std::ptr::null();
        let cg: CgImage = msg_send![&*img, CGImageForProposedRect: &mut rect, context: nil, hints: nil];
        if cg.0.is_null() {
            return None;
        }
        let mut buf = vec![0u8; (size * size * 4) as usize];
        let cs = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, size as usize, size as usize, 8, size as usize * 4, cs, kRGBA_PREMULTIPLIED);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        // kCGInterpolationHigh = 3
        CGContextSetInterpolationQuality(ctx, 3);
        CGContextDrawImage(ctx, CGRect { origin: CGPoint::default(), size: CGSize { width: size as f64, height: size as f64 } }, cg.0);
        CGContextRelease(ctx);
        Some(buf)
    })
}

fn bundle_info(path: &Path) -> Option<InstalledApp> {
    autoreleasepool(|_| unsafe {
        let ns = NSString::from_str(&path.to_string_lossy());
        let b: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), bundleWithPath: &*ns];
        let b = b?;
        let bid: Option<Retained<NSString>> = msg_send![&*b, bundleIdentifier];
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        Some(InstalledApp { name: stem, bundle_id: bid.map(|s| s.to_string()).unwrap_or_default(), path: path.to_string_lossy().into_owned() })
    })
}

/// Apps in the usual folders (for mapping imported Windows entries).
pub fn installed_apps() -> Vec<InstalledApp> {
    let mut dirs = vec![
        "/Applications".to_string(),
        "/Applications/Utilities".into(),
        "/System/Applications".into(),
        "/System/Applications/Utilities".into(),
    ];
    if let Some(h) = std::env::var_os("HOME") {
        dirs.push(format!("{}/Applications", h.to_string_lossy()));
    }
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "app") {
                if let Some(a) = bundle_info(&p) {
                    out.push(a);
                }
            }
        }
    }
    if let Some(a) = bundle_info(Path::new("/System/Library/CoreServices/Finder.app")) {
        out.push(a);
    }
    out
}

/// `true` if the app at `path` / with `bundle_id` exists on this Mac.
pub fn app_exists(path: &str, bundle_id: &str) -> bool {
    if !path.is_empty() && Path::new(path).exists() {
        return true;
    }
    if bundle_id.is_empty() {
        return false;
    }
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let bid = NSString::from_str(bundle_id);
        let url: Option<Retained<NSURL>> = msg_send![&*ws, URLForApplicationWithBundleIdentifier: &*bid];
        url.is_some()
    })
}
