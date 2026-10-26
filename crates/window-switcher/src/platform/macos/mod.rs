//! macOS backend.

pub(crate) mod apps;
mod ax;
pub(crate) mod cmdtab;
mod focus;
mod ffi;
pub(crate) mod overlay;
mod runtime;
pub(crate) mod spotlight;
mod thumbs;
mod windows;

pub(crate) use runtime::{permissions, Runtime};

/// Offscreen rendering for previews / visual tests (main thread).
pub mod preview {
    pub use super::ffi::Image;
    pub use super::overlay::{fake_window_image, render_live_png, render_png, ItemView, Model};
    pub use super::spotlight::{render_scene_png, shown as shown_preview, DesktopWindow, PreviewView};

    /// Icon of an app bundle path, as an `ItemView` would show it.
    pub fn app_icon_exists(path: &str) -> bool {
        super::apps::icon_for_path(path).is_some()
    }
}
