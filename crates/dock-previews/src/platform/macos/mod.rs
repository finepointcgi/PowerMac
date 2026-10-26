//! macOS backend.

mod apps;
mod ax;
mod dock;
mod ffi;
mod focus;
mod panel;
mod runtime;
mod thumbs;
mod windows;

pub(crate) use runtime::{permissions, Runtime};

/// Offscreen rendering and Dock inspection for previews / visual tests.
/// Rendering functions must run on the main thread.
pub mod preview {
    pub use super::ffi::Image;
    pub use super::panel::{current_model, fake_window_image, render_live_png, render_png, Model, TileView};

    /// The Dock as read through Accessibility right now (`None` without
    /// the permission).
    pub fn read_dock() -> Option<crate::geom::Dock> {
        super::dock::read()
    }

    /// Pid of the running app with this bundle path.
    pub fn pid_for_path(path: &str) -> Option<i32> {
        super::apps::app_for_path(path).map(|a| a.pid)
    }
}
