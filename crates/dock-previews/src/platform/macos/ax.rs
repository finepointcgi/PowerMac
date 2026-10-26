//! Accessibility (AXUIElement) helpers.

use std::ffi::c_void;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};

use super::ffi::*;

pub fn is_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Owned (retained) AXUIElementRef.
pub struct AxElement(AXUIElementRef);

// AXUIElementRef is a CF object; the AX API is usable from any thread.
unsafe impl Send for AxElement {}
unsafe impl Sync for AxElement {}

impl Clone for AxElement {
    fn clone(&self) -> Self {
        unsafe { CFRetain(self.0) };
        AxElement(self.0)
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

impl std::fmt::Debug for AxElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AxElement({:p})", self.0)
    }
}

/// Seconds an AX call may block on an unresponsive app.
const AX_TIMEOUT: f32 = 0.3;

impl AxElement {
    /// # Safety
    /// `r` must be null or an owned (+1) `AXUIElementRef`.
    pub unsafe fn from_create(r: AXUIElementRef) -> Option<Self> {
        (!r.is_null()).then_some(AxElement(r))
    }

    pub fn application(pid: i32) -> Self {
        let e = unsafe { AxElement(AXUIElementCreateApplication(pid)) };
        unsafe { AXUIElementSetMessagingTimeout(e.0, AX_TIMEOUT) };
        e
    }

    fn copy_attr(&self, attr: &str) -> Result<CFTypeRef> {
        let name = CfString::new(attr);
        let mut v: CFTypeRef = std::ptr::null();
        let e = unsafe { AXUIElementCopyAttributeValue(self.0, name.0, &mut v) };
        if e != kAXErrorSuccess || v.is_null() {
            return Err(anyhow!("AX get {attr} failed ({e})"));
        }
        Ok(v)
    }

    pub fn element_attr(&self, attr: &str) -> Option<AxElement> {
        let v = self.copy_attr(attr).ok()?;
        unsafe {
            if CFGetTypeID(v) != AXUIElementGetTypeID() {
                CFRelease(v);
                return None;
            }
        }
        Some(AxElement(v))
    }

    pub fn string_attr(&self, attr: &str) -> Option<String> {
        let v = self.copy_attr(attr).ok()?;
        let s = unsafe { cfstring_to_string(v) };
        unsafe { CFRelease(v) };
        s
    }

    pub fn bool_attr(&self, attr: &str) -> Option<bool> {
        let v = self.copy_attr(attr).ok()?;
        let r = unsafe {
            if CFGetTypeID(v) == CFBooleanGetTypeID() {
                Some(CFBooleanGetValue(v) != 0)
            } else if CFGetTypeID(v) == CFNumberGetTypeID() {
                let mut n: i64 = 0;
                (CFNumberGetValue(v, kCFNumberSInt64Type, &mut n as *mut i64 as *mut c_void) != 0).then_some(n != 0)
            } else {
                None
            }
        };
        unsafe { CFRelease(v) };
        r
    }

    pub fn set_bool_attr(&self, attr: &str, value: bool) -> Result<()> {
        let name = CfString::new(attr);
        let v = unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } };
        let e = unsafe { AXUIElementSetAttributeValue(self.0, name.0, v) };
        if e != kAXErrorSuccess {
            return Err(anyhow!("AX set {attr} failed ({e})"));
        }
        Ok(())
    }

    pub fn perform(&self, action: &str) -> Result<()> {
        let a = CfString::new(action);
        let e = unsafe { AXUIElementPerformAction(self.0, a.0) };
        if e != kAXErrorSuccess {
            return Err(anyhow!("AX {action} failed ({e})"));
        }
        Ok(())
    }

    pub fn elements_attr(&self, attr: &str) -> Vec<AxElement> {
        let Ok(v) = self.copy_attr(attr) else { return vec![] };
        let mut out = Vec::new();
        unsafe {
            if CFGetTypeID(v) == CFArrayGetTypeID() {
                for i in 0..CFArrayGetCount(v) {
                    let e = CFArrayGetValueAtIndex(v, i);
                    if !e.is_null() && CFGetTypeID(e) == AXUIElementGetTypeID() {
                        CFRetain(e);
                        out.push(AxElement(e));
                    }
                }
            }
            CFRelease(v);
        }
        out
    }

    /// `AXPosition` + `AXSize` as a rect (global, top-left origin).
    pub fn frame(&self) -> Option<crate::geom::RectF> {
        let p = self.copy_attr("AXPosition").ok()?;
        let s = self.copy_attr("AXSize").ok();
        let mut pt = CGPoint::default();
        let mut sz = CGSize::default();
        unsafe {
            let ok = CFGetTypeID(p) == AXValueGetTypeID() && AXValueGetValue(p, kAXValueCGPointType, &mut pt as *mut CGPoint as *mut c_void) != 0;
            CFRelease(p);
            let s = s?;
            let ok2 = CFGetTypeID(s) == AXValueGetTypeID() && AXValueGetValue(s, kAXValueCGSizeType, &mut sz as *mut CGSize as *mut c_void) != 0;
            CFRelease(s);
            (ok && ok2).then(|| crate::geom::RectF::new(pt.x, pt.y, sz.width, sz.height))
        }
    }

    /// A file URL attribute (`AXURL`) as a POSIX path.
    pub fn path_attr(&self, attr: &str) -> Option<String> {
        let v = self.copy_attr(attr).ok()?;
        unsafe {
            let r = if CFGetTypeID(v) == CFURLGetTypeID() {
                let s = CFURLCopyFileSystemPath(v, 0);
                let out = cfstring_to_string(s);
                if !s.is_null() {
                    CFRelease(s);
                }
                out
            } else {
                cfstring_to_string(v)
            };
            CFRelease(v);
            r.map(|p| if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p })
        }
    }

    /// Owning process id.
    pub fn pid(&self) -> Option<i32> {
        let mut pid = 0;
        (unsafe { AXUIElementGetPid(self.0, &mut pid) } == kAXErrorSuccess).then_some(pid)
    }

    /// Set the messaging timeout for calls on this element.
    pub fn set_timeout(&self, secs: f32) {
        unsafe { AXUIElementSetMessagingTimeout(self.0, secs) };
    }

    /// CGWindowID via the `_AXUIElementGetWindow` SPI.
    pub fn window_id(&self) -> Option<u32> {
        let mut id = 0u32;
        (unsafe { _AXUIElementGetWindow(self.0, &mut id) } == kAXErrorSuccess && id != 0).then_some(id)
    }

    pub fn role(&self) -> Option<String> {
        self.string_attr("AXRole")
    }

    pub fn subrole(&self) -> Option<String> {
        self.string_attr("AXSubrole")
    }
}

type RemoteTokenFn = unsafe extern "C" fn(CFTypeRef) -> AXUIElementRef;

fn remote_token_fn() -> Option<RemoteTokenFn> {
    static F: OnceLock<Option<RemoteTokenFn>> = OnceLock::new();
    *F.get_or_init(|| {
        let p = find_symbol(c"_AXUIElementCreateWithRemoteToken");
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, RemoteTokenFn>(p) })
    })
}

/// Window elements of `pid` found by brute force through the private
/// `_AXUIElementCreateWithRemoteToken` (the technique AltTab uses):
/// `AXWindows` only reports windows on the current Space, but an element
/// can be built for any of the app's windows from its remote token
/// (`pid, 0, 'coco', element id`). Ids are small integers; we probe
/// `0..limit` and stop early once every wanted window id was found.
pub fn windows_by_remote_token(pid: i32, wanted: &[u32], limit: u64) -> Vec<(u32, AxElement)> {
    let Some(create) = remote_token_fn() else { return vec![] };
    let mut out: Vec<(u32, AxElement)> = Vec::new();
    let start = std::time::Instant::now();
    for id in 0..limit {
        if out.len() >= wanted.len() || start.elapsed().as_millis() > 120 {
            break;
        }
        let mut token = [0u8; 20];
        token[0..4].copy_from_slice(&pid.to_le_bytes());
        token[8..12].copy_from_slice(&0x636f_636fu32.to_le_bytes());
        token[12..20].copy_from_slice(&id.to_le_bytes());
        unsafe {
            let data = CFDataCreate(std::ptr::null(), token.as_ptr(), token.len() as isize);
            if data.is_null() {
                continue;
            }
            let el = create(data);
            CFRelease(data);
            let Some(el) = AxElement::from_create(el) else { continue };
            AXUIElementSetMessagingTimeout(el.0, AX_TIMEOUT);
            if let Some(wid) = el.window_id() {
                if wanted.contains(&wid) && !out.iter().any(|(w, _)| *w == wid) && el.role().as_deref() == Some("AXWindow") {
                    out.push((wid, el));
                }
            }
        }
    }
    out
}
