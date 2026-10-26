//! macOS: `NSColorSampler` for picking (the system loupe; public API, no
//! Screen Recording permission) and Carbon hotkeys.

use std::cell::RefCell;
use std::sync::{mpsc, Arc, Mutex};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSColor, NSColorSampler, NSColorSpace};

pub use super::mac_hotkey::HotkeyReg;
use super::mac_main::{on_main, pump_until};
use crate::Rgb;

pub type PickCallback = Box<dyn FnOnce(Option<Rgb>) + Send>;

fn to_rgb(c: &NSColor) -> Option<Rgb> {
    // The sampler reports the screen's color space (often Display P3); use
    // sRGB values, which is what web / design tools expect.
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let f = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Rgb::new(
        f(c.redComponent()),
        f(c.greenComponent()),
        f(c.blueComponent()),
    ))
}

/// Show the sampler (main thread); `cb` gets the color, or `None` on Esc.
fn show_sampler(cb: PickCallback) {
    let cell = RefCell::new(Some(cb));
    let block = RcBlock::new(move |color: *mut NSColor| {
        let res = unsafe { color.as_ref() }.and_then(to_rgb);
        if let Some(cb) = cell.borrow_mut().take() {
            cb(res);
        }
    });
    let sampler = NSColorSampler::new();
    // The sampler retains itself until the session ends.
    unsafe { sampler.showSamplerWithSelectionHandler(&block) };
}

/// Start a pick; `cb` runs on the main thread when it ends.
pub fn pick_async(cb: PickCallback) {
    on_main(move || show_sampler(cb));
}

/// Pick one color, blocking. On the main thread this pumps AppKit events
/// itself (CLI use); elsewhere it waits for the main thread's loop.
pub fn pick_blocking() -> Option<Rgb> {
    if let Some(mtm) = MainThreadMarker::new() {
        let result: Arc<Mutex<Option<Option<Rgb>>>> = Arc::new(Mutex::new(None));
        let r = result.clone();
        show_sampler(Box::new(move |c| *r.lock().unwrap() = Some(c)));
        pump_until(mtm, None, || result.lock().unwrap().is_some());
        let v = result.lock().unwrap().take().flatten();
        v
    } else {
        let (tx, rx) = mpsc::channel();
        pick_async(Box::new(move |c| {
            let _ = tx.send(c);
        }));
        rx.recv().ok().flatten()
    }
}
