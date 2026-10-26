//! Cross-platform global input capture and injection.
//!
//! This crate is the contract between the Mouse Without Borders engine (`mwb`)
//! and the OS. Events are expressed in *Windows semantics* because that is what
//! the MWB wire protocol carries (Windows virtual-key codes, `WM_*` mouse
//! messages). Each backend translates to/from its native representation.
//!
//! Backends:
//! - macOS: `CGEventTap` for capture, `CGEventPost` for injection.
//! - Windows: `WH_MOUSE_LL` / `WH_KEYBOARD_LL` hooks for capture, `SendInput` for injection.

use std::sync::Arc;

/// Rectangle in global desktop pixel coordinates (top-left origin, y grows down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// XBUTTON1 (back)
    X1,
    /// XBUTTON2 (forward)
    X2,
}

/// A mouse event in global desktop pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEvent {
    /// Absolute position after the move, plus the raw delta when the OS reports it.
    Move { x: i32, y: i32, dx: i32, dy: i32 },
    Button { button: MouseButton, down: bool, x: i32, y: i32 },
    /// Vertical wheel; positive = away from user. Units: Windows WHEEL_DELTA (120 per notch).
    Wheel { delta: i32 },
    /// Horizontal wheel; positive = right. Units: WHEEL_DELTA.
    HWheel { delta: i32 },
}

/// A keyboard event. `vk` is a Windows virtual-key code (VK_*), on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u32,
    pub down: bool,
    /// Windows "extended key" bit (right Ctrl/Alt, arrows, Ins/Del/Home/End/PgUp/PgDn, ...).
    pub extended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Mouse(MouseEvent),
    Key(KeyEvent),
}

/// What the capture callback wants done with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Deliver to the local OS as normal.
    Pass,
    /// Swallow it (used while input is being forwarded to a remote machine).
    Swallow,
}

/// Called on the capture thread for every global input event. Must be fast.
pub type CaptureCallback = Arc<dyn Fn(&InputEvent) -> Disposition + Send + Sync>;

/// Handle for an active capture; dropping it stops capture and removes hooks.
pub trait CaptureHandle: Send {}

pub trait InputBackend: Send + Sync {
    /// Install global hooks. Events this backend itself injected must NOT be
    /// reported (tag them on injection and filter on capture), otherwise the
    /// MWB engine would echo remote input back over the wire.
    fn start_capture(&self, cb: CaptureCallback) -> anyhow::Result<Box<dyn CaptureHandle>>;

    /// Inject a mouse event locally. `Move` uses absolute x/y (dx/dy ignored).
    fn inject_mouse(&self, ev: MouseEvent) -> anyhow::Result<()>;

    /// Inject a key event locally (vk is a Windows VK code).
    fn inject_key(&self, ev: KeyEvent) -> anyhow::Result<()>;

    /// Warp the cursor without generating a click.
    fn set_cursor_pos(&self, x: i32, y: i32) -> anyhow::Result<()>;
    fn cursor_pos(&self) -> anyhow::Result<(i32, i32)>;

    /// Hide/show the local cursor (used while controlling a remote machine).
    fn set_cursor_visible(&self, visible: bool) -> anyhow::Result<()>;

    /// Bounds of every display, global coordinates. Index 0 is the primary.
    fn screens(&self) -> anyhow::Result<Vec<Rect>>;

    /// Union of all display bounds.
    fn desktop_bounds(&self) -> anyhow::Result<Rect> {
        let s = self.screens()?;
        let mut r = *s.first().ok_or_else(|| anyhow::anyhow!("no screens"))?;
        for o in &s[1..] {
            r.left = r.left.min(o.left);
            r.top = r.top.min(o.top);
            r.right = r.right.max(o.right);
            r.bottom = r.bottom.max(o.bottom);
        }
        Ok(r)
    }

    /// Whether the process has the OS permission needed for capture/injection
    /// (macOS: Accessibility + Input Monitoring). Always true on Windows.
    fn has_permissions(&self, prompt: bool) -> bool;
}

/// Windows virtual-key code constants and mac keycode translation tables.
pub mod vk;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// The backend for the current platform.
pub fn backend() -> anyhow::Result<Arc<dyn InputBackend>> {
    #[cfg(target_os = "macos")]
    {
        let _ = macos::MacBackend::new()?; anyhow::bail!("todo");
    }
    #[cfg(target_os = "windows")]
    {
        let _ = windows::WinBackend::new()?; anyhow::bail!("todo");
    }
    #[allow(unreachable_code)]
    Err(anyhow::anyhow!("unsupported platform"))
}
