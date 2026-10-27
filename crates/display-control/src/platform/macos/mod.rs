//! macOS backend: IOAVService DDC/CI, DisplayServices, gamma dimming, a
//! subscription to the shared event tap (`pm-hook`) for the media keys and
//! an AppKit OSD.

mod backend;
mod ffi;
mod keytap;
mod osd;

pub(crate) use backend::new_backend;
pub(crate) use keytap::KeyHook;

pub(crate) fn close_osd() {
    osd::close()
}

/// IORegistry / CoreDisplay diagnostics (used by the probe example).
pub fn debug_report() -> String {
    backend::debug_report()
}

/// Non-destructive write-path self-test (see the probe's `--noop-writes`).
pub fn debug_noop_writes() -> Vec<String> {
    backend::debug_noop_writes()
}

/// Show the OSD on a display (diagnostics; pump the main loop to see it).
pub fn debug_show_osd(bounds: crate::Rect, kind: crate::OsdKind, pct: u8) {
    osd::show(crate::engine::Feedback { bounds, kind, pct })
}

/// Pump the main run loop for `dur` (main thread only; for command-line
/// tools that want to see the OSD — the tray app runs NSApplication).
pub fn run_main_loop_for(dur: std::time::Duration) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        std::thread::sleep(dur);
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let end = std::time::Instant::now() + dur;
    while std::time::Instant::now() < end {
        let left = end.saturating_duration_since(std::time::Instant::now()).as_secs_f64().min(0.05);
        unsafe { ffi::CFRunLoopRunInMode(ffi::kCFRunLoopDefaultMode, left, 0) };
    }
}
