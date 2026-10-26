//! Accessibility (AXUIElement) window access.

use std::ffi::c_void;

use anyhow::{anyhow, bail, Result};

use super::ffi::*;
use super::screens;
use crate::engine::{Monitor, WindowKey, WindowSystem};
use crate::geometry::{Point, Rect};

/// `true` if this process is trusted for Accessibility. Never prompts.
pub fn has_permissions() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

fn ensure_trusted() -> Result<()> {
    if !has_permissions() {
        bail!(
            "Accessibility permission not granted to this process (System Settings → Privacy & Security → Accessibility)"
        );
    }
    Ok(())
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

fn ax_err(op: &str, attr: &str, e: AXError) -> anyhow::Error {
    let what = match e {
        -25200 => "failure",
        -25201 => "illegal argument",
        -25202 => "invalid element",
        -25204 => "cannot complete",
        -25205 => "attribute unsupported",
        -25211 => "API disabled (no Accessibility permission)",
        -25212 => "no value",
        _ => "error",
    };
    anyhow!("AX {op} {attr} failed: {what} ({e})")
}

impl AxElement {
    /// Takes ownership of a +1 reference. Returns `None` for null.
    ///
    /// # Safety
    /// `r` must be null or an owned (+1) `AXUIElementRef`.
    pub unsafe fn from_create(r: AXUIElementRef) -> Option<Self> {
        (!r.is_null()).then_some(AxElement(r))
    }

    pub fn system_wide() -> Self {
        unsafe { AxElement(AXUIElementCreateSystemWide()) }
    }

    pub fn application(pid: i32) -> Self {
        unsafe { AxElement(AXUIElementCreateApplication(pid)) }
    }

    pub fn raw(&self) -> AXUIElementRef {
        self.0
    }

    /// Copy an attribute value (+1 retained CFTypeRef, caller releases).
    fn copy_attr(&self, attr: &str) -> Result<CFTypeRef> {
        let name = CfString::new(attr);
        let mut v: CFTypeRef = std::ptr::null();
        let e = unsafe { AXUIElementCopyAttributeValue(self.0, name.0, &mut v) };
        if e != kAXErrorSuccess || v.is_null() {
            return Err(ax_err("get", attr, e));
        }
        Ok(v)
    }

    pub fn element_attr(&self, attr: &str) -> Result<AxElement> {
        let v = self.copy_attr(attr)?;
        unsafe {
            if CFGetTypeID(v) != AXUIElementGetTypeID() {
                CFRelease(v);
                bail!("AX attribute {attr} is not an element");
            }
            Ok(AxElement(v))
        }
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
            bail!("AX attribute {attr} has unexpected type");
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

    pub fn position(&self) -> Result<CGPoint> {
        self.value_attr("AXPosition", kAXValueCGPointType)
    }

    pub fn size(&self) -> Result<CGSize> {
        self.value_attr("AXSize", kAXValueCGSizeType)
    }

    pub fn set_position(&self, p: CGPoint) -> Result<()> {
        self.set_value_attr("AXPosition", kAXValueCGPointType, &p)
    }

    pub fn set_size(&self, s: CGSize) -> Result<()> {
        self.set_value_attr("AXSize", kAXValueCGSizeType, &s)
    }

    pub fn pid(&self) -> Option<i32> {
        let mut pid = 0;
        (unsafe { AXUIElementGetPid(self.0, &mut pid) } == kAXErrorSuccess).then_some(pid)
    }

    pub fn role(&self) -> Option<String> {
        self.string_attr("AXRole")
    }

    /// CGWindowID via the `_AXUIElementGetWindow` SPI.
    pub fn window_id(&self) -> Option<u32> {
        let mut id = 0u32;
        (unsafe { _AXUIElementGetWindow(self.0, &mut id) } == kAXErrorSuccess && id != 0).then_some(id)
    }

    pub fn frame(&self) -> Result<Rect> {
        let p = self.position()?;
        let s = self.size()?;
        Ok(Rect::from_xywh(p.x.round() as i32, p.y.round() as i32, s.width.round() as i32, s.height.round() as i32))
    }

    /// Element under a global point (top-left coordinates).
    pub fn at_position(p: Point) -> Result<AxElement> {
        let sys = Self::system_wide();
        let mut el: AXUIElementRef = std::ptr::null();
        let e = unsafe { AXUIElementCopyElementAtPosition(sys.0, p.x as f32, p.y as f32, &mut el) };
        if e != kAXErrorSuccess {
            return Err(ax_err("hit-test", "position", e));
        }
        unsafe { Self::from_create(el) }.ok_or_else(|| anyhow!("no element at point"))
    }

    /// Climb to the enclosing AXWindow.
    pub fn enclosing_window(&self) -> Option<AxElement> {
        if self.role().as_deref() == Some("AXWindow") {
            return Some(self.clone());
        }
        if let Ok(w) = self.element_attr("AXWindow") {
            if w.role().as_deref() == Some("AXWindow") {
                return Some(w);
            }
        }
        let mut cur = self.clone();
        for _ in 0..32 {
            let parent = cur.element_attr("AXParent").ok()?;
            if parent.role().as_deref() == Some("AXWindow") {
                return Some(parent);
            }
            cur = parent;
        }
        None
    }
}

/// A top-level window controlled through Accessibility.
#[derive(Debug, Clone)]
pub struct MacWindow {
    pub element: AxElement,
    pub pid: i32,
}

impl MacWindow {
    /// The standard window under the given global point, if any.
    pub fn at_point(p: Point) -> Option<MacWindow> {
        let el = AxElement::at_position(p).ok()?;
        let w = el.enclosing_window()?;
        let pid = w.pid()?;
        if pid == std::process::id() as i32 {
            return None;
        }
        // Skip sheets/popovers that are not resizable/movable standard windows.
        match w.string_attr("AXSubrole").as_deref() {
            Some("AXStandardWindow") | Some("AXDialog") | None => {}
            Some(_) => return None,
        }
        Some(MacWindow { element: w, pid })
    }

    pub fn is_fullscreen(&self) -> bool {
        self.element.bool_attr("AXFullScreen").unwrap_or(false)
    }

    pub fn key(&self) -> WindowKey {
        match self.element.window_id() {
            Some(id) => ((self.pid as u64) << 32) | id as u64,
            None => ((self.pid as u64) << 32) ^ (unsafe { CFHash(self.element.raw()) } as u64 & 0xFFFF_FFFF),
        }
    }

    /// Set frame the way most macOS window managers do: size, position, size
    /// (moving between displays may clamp the first resize), with
    /// AXEnhancedUserInterface temporarily off to avoid animated moves.
    pub fn set_frame(&self, r: Rect) -> Result<()> {
        let app = AxElement::application(self.pid);
        let enhanced = app.bool_attr("AXEnhancedUserInterface").unwrap_or(false);
        if enhanced {
            let _ = app.set_bool_attr("AXEnhancedUserInterface", false);
        }
        let size = CGSize { width: r.width() as f64, height: r.height() as f64 };
        let pos = CGPoint { x: r.left as f64, y: r.top as f64 };
        let _ = self.element.set_size(size);
        let res = self.element.set_position(pos);
        let res2 = self.element.set_size(size);
        if enhanced {
            let _ = app.set_bool_attr("AXEnhancedUserInterface", true);
        }
        res?;
        if let Err(e) = res2 {
            log::debug!("resize after move failed (window may not be resizable): {e}");
        }
        Ok(())
    }
}

/// Frontmost application's pid via NSWorkspace.
fn frontmost_pid() -> Option<i32> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    objc2::rc::autoreleasepool(|_| unsafe {
        let ws: Option<Retained<AnyObject>> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: Option<Retained<AnyObject>> = msg_send![&*ws?, frontmostApplication];
        let pid: i32 = msg_send![&*app?, processIdentifier];
        Some(pid)
    })
}

fn app_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    let s = String::from_utf8_lossy(&buf).into_owned();
    // Prefer the .app bundle path when inside one.
    match s.find(".app/") {
        Some(i) => Some(s[..i + 4].to_string()),
        None => Some(s),
    }
}

/// macOS [`WindowSystem`] (Accessibility + NSScreen).
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeWindowSystem;

impl NativeWindowSystem {
    pub fn new() -> Self {
        Self
    }

    /// The focused window of the frontmost application.
    pub fn focused(&self) -> Result<Option<MacWindow>> {
        ensure_trusted()?;
        let sys = AxElement::system_wide();
        let app = match sys.element_attr("AXFocusedApplication") {
            Ok(a) => a,
            Err(_) => match frontmost_pid() {
                Some(pid) => AxElement::application(pid),
                None => return Ok(None),
            },
        };
        let win = match app.element_attr("AXFocusedWindow").or_else(|_| app.element_attr("AXMainWindow")) {
            Ok(w) => w,
            Err(_) => return Ok(None),
        };
        let Some(pid) = win.pid().or_else(|| app.pid()) else { return Ok(None) };
        Ok(Some(MacWindow { element: win, pid }))
    }
}

impl WindowSystem for NativeWindowSystem {
    type Window = MacWindow;

    fn monitors(&self) -> Result<Vec<Monitor>> {
        screens::monitors()
    }

    fn focused_window(&self) -> Result<Option<MacWindow>> {
        self.focused()
    }

    fn window_key(&self, w: &MacWindow) -> WindowKey {
        w.key()
    }

    fn window_rect(&self, w: &MacWindow) -> Result<Rect> {
        w.element.frame()
    }

    fn set_window_rect(&self, w: &MacWindow, r: Rect) -> Result<()> {
        ensure_trusted()?;
        if w.is_fullscreen() {
            bail!("window is in native full screen");
        }
        w.set_frame(r)
    }

    fn window_app_id(&self, w: &MacWindow) -> Option<String> {
        app_path(w.pid)
    }
}
