//! macOS backend: Accessibility API for windows, NSScreen for monitors,
//! CGEventTap for drag detection + hotkeys, AppKit overlay on the main thread.

mod ax;
mod ffi;
mod overlay;
mod runtime;
mod screens;
mod spaces;

use std::time::{Duration, Instant};

pub use ax::{has_permissions, AxElement, MacWindow, NativeWindowSystem};
pub(crate) use runtime::Runtime;
pub use runtime::monitor_under_cursor;
pub use screens::{cocoa_to_global, global_to_cocoa};

/// The macOS accent color (`NSColor.controlAccentColor`, sRGB). Cached for
/// two seconds since the overlay asks for it on every drag update.
pub fn system_accent_color() -> Option<(u8, u8, u8)> {
    use std::sync::Mutex;
    type Rgb = (u8, u8, u8);
    static CACHE: Mutex<Option<(Instant, Option<Rgb>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *c {
        if t.elapsed() < Duration::from_secs(2) {
            return v;
        }
    }
    let v = read_accent();
    *c = Some((Instant::now(), v));
    v
}

fn read_accent() -> Option<(u8, u8, u8)> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    let accent = NSColor::controlAccentColor();
    let rgb = accent.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let to = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some((
        to(rgb.redComponent()),
        to(rgb.greenComponent()),
        to(rgb.blueComponent()),
    ))
}

/// Show the zone overlay directly (any thread; drawn on the main thread).
pub fn show_overlay(state: crate::engine::OverlayState) {
    overlay::show(state)
}

/// Hide the zone overlay (any thread).
pub fn hide_overlay() {
    overlay::hide()
}

fn init_app(mtm: objc2::MainThreadMarker) -> objc2::rc::Retained<objc2_app_kit::NSApplication> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let app = NSApplication::sharedApplication(mtm);
    // Background agent: no Dock icon, no menu bar.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

/// Run the AppKit main loop forever. **Must be called on the main thread.**
/// Use this from the app binary if it has no NSApplication loop of its own.
pub fn run_main_loop() {
    let mtm = objc2::MainThreadMarker::new().expect("run_main_loop must be called on the main thread");
    init_app(mtm).run();
}

/// Pump the main run loop for `dur` (main thread only), e.g. for demos and
/// tests. Services the main dispatch queue so overlays are drawn.
pub fn run_main_loop_for(dur: Duration) {
    let mtm = objc2::MainThreadMarker::new().expect("run_main_loop_for must be called on the main thread");
    let app = init_app(mtm);
    app.finishLaunching();
    let end = Instant::now() + dur;
    while Instant::now() < end {
        let left = end.saturating_duration_since(Instant::now()).as_secs_f64().min(0.05);
        unsafe { ffi::CFRunLoopRunInMode(ffi::kCFRunLoopDefaultMode, left, 0) };
    }
}
