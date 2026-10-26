//! macOS backend (AppKit + Core Animation, main thread).

mod app;
mod bluetooth;
mod calendar;
mod camera;
mod ffi;
mod gfx;
mod jobs;
mod media;
mod paint;
mod pboard;
mod providers;
mod reminders;
mod rt;
mod sysinfo;
mod view;

use std::sync::Arc;

use anyhow::Result;
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;

use crate::Shared;

/// True if a connected screen has a camera housing (exact on the main
/// thread via `NSScreen.safeAreaInsets`; a display-aspect heuristic
/// elsewhere).
pub fn has_notch() -> bool {
    if let Some(mtm) = MainThreadMarker::new() {
        return NSScreen::screens(mtm).iter().any(|s| s.safeAreaInsets().top > 0.0);
    }
    ffi::builtin_display_has_notch_heuristic()
}

/// Run a Script / xbar / Web instance once (settings preview).
pub fn run_widget_once(inst: &crate::widgets::instance::WidgetInstance) -> Result<crate::widgets::content::Content, String> {
    jobs::run_once(inst)
}

pub struct Backend;

fn on_main(f: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        f();
    } else {
        ffi::dispatch_main(f);
    }
}

impl Backend {
    pub fn start(shared: Arc<Shared>) -> Result<Backend> {
        providers::spawn(shared.clone());
        on_main(move || app::install(shared));
        Ok(Backend)
    }

    /// Process Handle changes promptly (the frame loop also polls).
    pub fn wake(&self) {
        on_main(|| app::tick(false));
    }

    pub fn stop(&self) {
        on_main(app::uninstall);
    }
}
