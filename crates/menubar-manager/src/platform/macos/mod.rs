//! macOS backend (AppKit on the main thread, CoreGraphics / Accessibility
//! for other apps' items).

mod app;
mod bar;
mod ffi;
mod items;

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use objc2::MainThreadMarker;

use crate::{Permissions, Shared, Snapshot};

pub use items::{cmd_drag, list_other_items};

pub fn scan_visible(screen: crate::Rect, menu_bar_h: f64) -> Vec<(f64, i32, Option<String>)> {
    if is_main_thread() {
        return vec![];
    }
    items::scan_visible(screen, menu_bar_h)
}

/// macOS 26+ lays status items out in MenuBarAgent and moves the ones that
/// don't fit into an overflow menu.
pub fn fitted_mode_default() -> bool {
    objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion().majorVersion >= 26
}

/// Does the menu bar really show `pid`'s item at `frame`? (hit test; `None`
/// when it can't tell: main thread or no Accessibility).
pub fn really_shown(frame: crate::Rect, pid: i32) -> Option<bool> {
    if is_main_thread() || !unsafe { ffi::AXIsProcessTrusted() } {
        return None;
    }
    let h = items::hit(frame.center_x(), frame.center_y()).or_else(|| items::hit(frame.center_x(), frame.center_y()));
    Some(h.is_some_and(|h| h.pid == pid))
}

pub struct Backend {
    shared: Arc<Shared>,
}

fn on_main(f: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        f();
    } else {
        ffi::dispatch_main(f);
    }
}

impl Backend {
    pub fn start(shared: Arc<Shared>) -> Result<Backend> {
        let s = shared.clone();
        on_main(move || app::install(s));
        Ok(Backend { shared })
    }

    pub fn wake(&self) {
        on_main(app::tick);
    }

    pub fn stop(&self) {
        let shared = self.shared.clone();
        on_main(move || app::uninstall_for(&shared));
    }
}

pub fn is_main_thread() -> bool {
    MainThreadMarker::new().is_some()
}

pub fn permissions() -> Permissions {
    unsafe { Permissions { accessibility: ffi::AXIsProcessTrusted(), screen_recording: ffi::CGPreflightScreenCaptureAccess() } }
}

pub fn refresh_on_main() {
    app::with(|a| a.refresh());
}

/// Screen / notch / own items without a manager (any thread; off the main
/// thread it asks the main queue and waits up to a second).
pub fn snapshot_now() -> Snapshot {
    if let Some(mtm) = MainThreadMarker::new() {
        return app::snapshot_without_manager(mtm);
    }
    let (tx, rx) = mpsc::channel();
    ffi::dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(app::snapshot_without_manager(mtm));
        }
    });
    rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default()
}

pub fn reset_positions() {
    app::reset_positions();
}
