//! macOS backend.

mod apps;
mod ax;
mod focus;
mod ffi;
pub(crate) mod overlay;
mod runtime;
mod thumbs;
mod windows;

pub(crate) use runtime::{permissions, Runtime};

/// Offscreen rendering for previews / visual tests (main thread).
pub mod preview {
    pub use super::ffi::Image;
    pub use super::overlay::{fake_window_image, render_live_png, render_png, ItemView, Model};

    /// Icon of an app bundle path, as an `ItemView` would show it.
    pub fn app_icon_exists(path: &str) -> bool {
        super::apps::icon_for_path(path).is_some()
    }
}
