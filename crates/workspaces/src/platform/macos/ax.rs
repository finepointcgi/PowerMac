//! Accessibility (AXUIElement) helpers.

use std::ffi::c_void;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Result};

use super::ffi::*;
use crate::geometry::Rect;

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
const AX_TIMEOUT: f32 = 0.5;

fn ax_err(op: &str, attr: &str, e: AXError) -> anyhow::Error {
    let what = match e {
        -25200 => "failure",
        -25201 => "illegal argument",
        -25202 => "invalid element",
        -25204 => "cannot complete",
        -25205 => "attribute unsupported",
        -25211 => "no Accessibility permission",
        -25212 => "no value",
        _ => "error",
    };
    anyhow!("AX {op} {attr}: {what} ({e})")
}

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
            return Err(ax_err("get", attr, e));
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
        let s = unsafe {
            if CFGetTypeID(v) == CFURLGetTypeID() {
                // kCFURLPOSIXPathStyle = 0
                let p = CFURLCopyFileSystemPath(v, 0);
                let s = cfstring_to_string(p);
                if !p.is_null() {
                    CFRelease(p);
                }
                s
            } else {
                cfstring_to_string(v)
            }
        };
        unsafe { CFRelease(v) };
        s
    }

    pub fn bool_attr(&self, attr: &str) -> Option<bool> {
        let v = self.copy_attr(attr).ok()?;
        let r = unsafe { (CFGetTypeID(v) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(v) != 0) };
        unsafe { CFRelease(v) };
        r
    }

    pub fn set_bool_attr(&self, attr: &str, value: bool) -> Result<()> {
        let name = CfString::new(attr);
        let v = unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } };
        let e = unsafe { AXUIElementSetAttributeValue(self.0, name.0, v) };
        if e != kAXErrorSuccess {
            return Err(ax_err("set", attr, e));
        }
        Ok(())
    }

    fn value_attr<T: Default>(&self, attr: &str, ty: u32) -> Result<T> {
        let v = self.copy_attr(attr)?;
        let mut out = T::default();
        let ok = unsafe {
            let ok = CFGetTypeID(v) == AXValueGetTypeID() && AXValueGetValue(v, ty, &mut out as *mut T as *mut c_void) != 0;
            CFRelease(v);
            ok
        };
        if !ok {
            bail!("AX attribute {attr} has an unexpected type");
        }
        Ok(out)
    }

    fn set_value_attr<T>(&self, attr: &str, ty: u32, val: &T) -> Result<()> {
        let name = CfString::new(attr);
        unsafe {
            let v = AXValueCreate(ty, val as *const T as *const c_void);
            if v.is_null() {
                bail!("AXValueCreate failed");
            }
            let e = AXUIElementSetAttributeValue(self.0, name.0, v);
            CFRelease(v);
            if e != kAXErrorSuccess {
                return Err(ax_err("set", attr, e));
            }
        }
        Ok(())
    }

    pub fn perform(&self, action: &str) -> Result<()> {
        let a = CfString::new(action);
        let e = unsafe { AXUIElementPerformAction(self.0, a.0) };
        if e != kAXErrorSuccess {
            return Err(ax_err("perform", action, e));
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

    pub fn frame(&self) -> Result<Rect> {
        let p: CGPoint = self.value_attr("AXPosition", kAXValueCGPointType)?;
        let s: CGSize = self.value_attr("AXSize", kAXValueCGSizeType)?;
        Ok(Rect::new(p.x.round() as i32, p.y.round() as i32, s.width.round() as i32, s.height.round() as i32))
    }

    /// Set the frame the way most macOS window managers do: size, position,
    /// size (moving between displays may clamp the first resize), with the
    /// app's AXEnhancedUserInterface off (it animates moves).
    pub fn set_frame(&self, pid: i32, r: Rect) -> Result<()> {
        let app = AxElement::application(pid);
        let enhanced = app.bool_attr("AXEnhancedUserInterface").unwrap_or(false);
        if enhanced {
            let _ = app.set_bool_attr("AXEnhancedUserInterface", false);
        }
        let size = CGSize { width: r.width as f64, height: r.height as f64 };
        let pos = CGPoint { x: r.x as f64, y: r.y as f64 };
        let _ = self.set_value_attr("AXSize", kAXValueCGSizeType, &size);
        let res = self.set_value_attr("AXPosition", kAXValueCGPointType, &pos);
        let res2 = self.set_value_attr("AXSize", kAXValueCGSizeType, &size);
        if enhanced {
            let _ = app.set_bool_attr("AXEnhancedUserInterface", true);
        }
        res?;
        if let Err(e) = res2 {
            log::debug!("workspaces: resize after move failed (window may not be resizable): {e}");
        }
        Ok(())
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

/// Window elements of `pid` on other Spaces, found through the private
/// `_AXUIElementCreateWithRemoteToken` (AltTab's technique, as in the
/// window switcher): `AXWindows` only lists the current Space's windows,
/// but an element can be built for any window of the app from its remote
/// token (`pid, 0, 'coco', element id`). Ids are small integers; we probe
/// `0..limit` and stop once every wanted window id was found.
pub fn windows_by_remote_token(pid: i32, wanted: &[u32], limit: u64) -> Vec<(u32, AxElement)> {
    let Some(create) = remote_token_fn() else { return vec![] };
    let mut out: Vec<(u32, AxElement)> = Vec::new();
    let start = std::time::Instant::now();
    for id in 0..limit {
        if out.len() >= wanted.len() || start.elapsed().as_millis() > 150 {
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

/// `file:///Users/me/a%20b.txt` → `/Users/me/a b.txt`; other strings as is.
pub fn document_path(s: &str) -> String {
    let Some(rest) = s.strip_prefix("file://") else { return s.to_string() };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8_lossy(&out).into_owned();
    if s.len() > 1 {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_urls() {
        assert_eq!(document_path("file:///Users/me/a%20b.txt"), "/Users/me/a b.txt");
        assert_eq!(document_path("file:///Users/me/Folder/"), "/Users/me/Folder");
        assert_eq!(document_path("file://localhost/tmp/x"), "/tmp/x");
        assert_eq!(document_path("/plain/path"), "/plain/path");
        assert_eq!(document_path("file:///caf%C3%A9.txt"), "/café.txt");
        assert_eq!(document_path("file:///x%2"), "/x%2");
    }
}
