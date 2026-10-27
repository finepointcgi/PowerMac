//! The window-system interface the capture and launch logic runs against.
//! [`crate::platform::Native`] implements it for macOS (Accessibility,
//! NSWorkspace, CGWindowList) and Windows (EnumWindows, SetWindowPlacement,
//! ShellExecuteEx); the tests use a fake.

use std::time::Duration;

use anyhow::Result;

use crate::geometry::{Monitor, Rect};
use crate::model::WindowState;

/// Who owns a window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppIdentity {
    pub bundle_id: String,
    pub path: String,
    pub name: String,
}

/// A window that exists right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LiveWindow {
    /// Opaque, stable while the window exists (CGWindowID / HWND).
    pub id: u64,
    pub pid: i64,
    pub app: AppIdentity,
    pub title: String,
    pub frame: Rect,
    pub state: WindowState,
    /// 1-based Space index on its display, if known.
    pub space: Option<u32>,
    /// Open document (file path), if the app reports one.
    pub document: Option<String>,
    /// Front tab URL (browsers, AppleScript opt-in only).
    pub url: Option<String>,
    /// The app is hidden (Cmd+H).
    pub app_hidden: bool,
    /// Windows: the process runs elevated.
    pub elevated: bool,
}

/// What to start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchRequest {
    pub name: String,
    pub bundle_id: String,
    pub path: String,
    /// A document or URL to open with the app.
    pub open: Option<String>,
    pub args: String,
    pub new_instance: bool,
    pub as_admin: bool,
}

/// Where and how a window should end up.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub frame: Rect,
    pub state: WindowState,
    /// Move to this Space (1-based index on the target display) if possible.
    pub space: Option<u32>,
    pub monitor_id: Option<String>,
}

pub trait Backend {
    fn monitors(&self) -> Result<Vec<Monitor>>;
    /// Every regular app window (all Spaces when possible), front to back.
    fn windows(&self) -> Result<Vec<LiveWindow>>;
    /// Start an app (without activating it, where the OS allows).
    fn launch(&self, req: &LaunchRequest) -> Result<()>;
    /// Move / resize / set the state of a window. Returns a note for things
    /// that could not be done (e.g. "Space not restored").
    fn place(&self, w: &LiveWindow, p: &Placement) -> Result<Option<String>>;
    /// Current frame of a window (`None` if it is gone).
    fn window_frame(&self, w: &LiveWindow) -> Option<Rect>;
    fn minimize(&self, w: &LiveWindow) -> Result<()>;
    /// Hide an app (Cmd+H / minimize all on Windows). Never quits.
    fn hide_app(&self, pid: i64) -> Result<()>;
    /// Bring a window and its app to the front.
    fn activate(&self, w: &LiveWindow);
    /// This process (its windows are never captured or touched).
    fn own_pid(&self) -> i64 {
        std::process::id() as i64
    }
    /// Monotonic time (fake in tests).
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration);
}
