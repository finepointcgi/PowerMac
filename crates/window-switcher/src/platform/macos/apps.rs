//! NSWorkspace / NSRunningApplication helpers (callable from any thread).

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSString, NSURL};

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

/// Regular (Dock) apps.
pub fn regular_apps() -> Vec<AppInfo> {
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: Retained<NSArray<AnyObject>> = msg_send![&*ws, runningApplications];
        let mut out = Vec::new();
        for a in apps.iter() {
            let policy: isize = msg_send![&*a, activationPolicy];
            let pid: i32 = msg_send![&*a, processIdentifier];
            if policy != 0 || pid <= 0 {
                continue;
            }
            let terminated: bool = msg_send![&*a, isTerminated];
            if terminated {
                continue;
            }
            let name: Option<Retained<NSString>> = msg_send![&*a, localizedName];
            let bid: Option<Retained<NSString>> = msg_send![&*a, bundleIdentifier];
            let url: Option<Retained<NSURL>> = msg_send![&*a, bundleURL];
            let path = url.and_then(|u| u.path()).map(|p| p.to_string()).unwrap_or_default();
            let hidden: bool = msg_send![&*a, isHidden];
            out.push(AppInfo {
                pid,
                name: name.map(|s| s.to_string()).unwrap_or_default(),
                bundle_id: bid.map(|s| s.to_string()).unwrap_or_default(),
                path,
                hidden,
            });
        }
        out
    })
}

pub fn frontmost_pid() -> Option<i32> {
    autoreleasepool(|_| unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
        let pid: i32 = msg_send![&*app?, processIdentifier];
        Some(pid)
    })
}

/// Bring the app forward. Without `NSApplicationActivateAllWindows` only
/// its key/main window comes to the front.
pub fn activate(pid: i32) {
    autoreleasepool(|_| unsafe {
        if let Some(app) = running_app(pid) {
            // NSApplicationActivateIgnoringOtherApps (deprecated, still honoured).
            let _: bool = msg_send![&*app, activateWithOptions: 2usize];
        }
    })
}

pub fn set_hidden(pid: i32, hide: bool) {
    autoreleasepool(|_| unsafe {
        if let Some(app) = running_app(pid) {
            let _: bool = if hide { msg_send![&*app, hide] } else { msg_send![&*app, unhide] };
        }
    })
}

pub fn is_hidden(pid: i32) -> bool {
    autoreleasepool(|_| unsafe { running_app(pid).map(|a| msg_send![&*a, isHidden]).unwrap_or(false) })
}

pub fn terminate(pid: i32) {
    autoreleasepool(|_| unsafe {
        if let Some(app) = running_app(pid) {
            let _: bool = msg_send![&*app, terminate];
        }
    })
}

/// The app's icon (`NSImage`), for the overlay.
pub fn icon(pid: i32) -> Option<Retained<AnyObject>> {
    unsafe {
        let app = running_app(pid)?;
        msg_send![&*app, icon]
    }
}

/// Icon of an app bundle / file path (fallback when the app quit).
pub fn icon_for_path(path: &str) -> Option<Retained<AnyObject>> {
    if path.is_empty() {
        return None;
    }
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let p = NSString::from_str(path);
        msg_send![&*ws, iconForFile: &*p]
    }
}

/// AltTab's configured shortcuts (see [`crate::conflicts`]); its defaults
/// for shortcut 1 when nothing is stored.
pub fn alt_tab_hotkeys() -> Vec<crate::Hotkey> {
    use super::ffi::*;
    use crate::conflicts::{parse_alt_tab, ALT_TAB_BUNDLE_ID, ALT_TAB_DEFAULT_HOLD, ALT_TAB_DEFAULT_KEY};

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFPreferencesCopyAppValue(key: CFStringRef, app: CFStringRef) -> CFTypeRef;
    }
    /// A string pref, or the `string` member of a ShortcutRecorder dictionary.
    fn pref(key: &str) -> Option<String> {
        unsafe {
            let app = CfString::new(ALT_TAB_BUNDLE_ID);
            let k = CfString::new(key);
            let v = CfOwned::new(CFPreferencesCopyAppValue(k.0, app.0))?;
            let t = CFGetTypeID(v.0);
            if t == CFStringGetTypeID() {
                cfstring_to_string(v.0)
            } else if t == CFDictionaryGetTypeID() {
                dict_string(v.0, "string")
            } else {
                None
            }
        }
    }
    let mut v = Vec::new();
    let hold = pref("holdShortcut").unwrap_or_else(|| ALT_TAB_DEFAULT_HOLD.into());
    let key = pref("nextWindowShortcut").unwrap_or_else(|| ALT_TAB_DEFAULT_KEY.into());
    v.extend(parse_alt_tab(&hold, &key));
    if let (Some(h), Some(k)) = (pref("holdShortcut2"), pref("nextWindowShortcut2")) {
        v.extend(parse_alt_tab(&h, &k));
    }
    v
}
