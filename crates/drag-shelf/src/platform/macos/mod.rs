//! macOS backend: AppKit panels and Core Animation layers on the main thread.

mod app;
mod view;

use std::ffi::c_void;
use std::sync::Arc;

use anyhow::Result;
use objc2::MainThreadMarker;
use pm_shelf_core::mac::ffi::dispatch_main;

use crate::{Point, Shared};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const c_void) -> *const c_void;
    fn CGEventGetLocation(e: *const c_void) -> pm_shelf_core::mac::ffi::CGPoint;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// The pointer position (global, top-left origin); any thread.
pub fn cursor() -> Option<Point> {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return None;
        }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        Some(Point::new(p.x, p.y))
    }
}

fn on_main(f: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        f();
    } else {
        dispatch_main(f);
    }
}

pub struct Backend;

impl Backend {
    pub fn start(shared: Arc<Shared>) -> Result<Backend> {
        on_main(move || app::install(shared));
        Ok(Backend)
    }

    /// Process Handle changes promptly (the frame loop also polls).
    pub fn wake(&self) {
        on_main(app::tick);
    }

    pub fn stop(&self) {
        on_main(app::uninstall);
    }

    /// From the input hook: the left button went up.
    pub fn button_up() {
        dispatch_main(|| {
            app::with(|a| a.button_up());
        });
    }

    /// From the input hook: a shake at `at` during a left-button drag.
    pub fn shake(at: Point) {
        dispatch_main(move || {
            app::with(|a| a.shake(at));
        });
    }

    /// From the input hook: the "new shelf" hotkey.
    pub fn hotkey() {
        dispatch_main(|| {
            app::with(|a| a.new_shelf_at_cursor());
        });
    }
}
