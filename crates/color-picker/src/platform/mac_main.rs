//! Main-thread helpers: run work on the main thread (through the main
//! dispatch queue) and pump AppKit events while waiting for something.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

extern "C" {
    // libdispatch (libSystem)
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// Run `f` on the main thread via the main dispatch queue (asynchronously).
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(ctx: *mut c_void) {
        let f: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(ctx as *mut Box<dyn FnOnce()>) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    }
    let b: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
    unsafe {
        dispatch_async_f(
            &_dispatch_main_q as *const c_void,
            Box::into_raw(b) as *mut c_void,
            trampoline,
        )
    };
}

/// Run `f` now if this is the main thread, otherwise post it there.
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        f();
    } else {
        dispatch_main(f);
    }
}

/// Pump AppKit events on the main thread until `done()` or `timeout`.
/// Sets up NSApplication as an accessory (no Dock icon) app if needed.
pub fn pump_until(
    mtm: MainThreadMarker,
    timeout: Option<Duration>,
    mut done: impl FnMut() -> bool,
) {
    let app = NSApplication::sharedApplication(mtm);
    if !app.isRunning() {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
    }
    let end = timeout.map(|t| Instant::now() + t);
    while !done() {
        if end.is_some_and(|e| Instant::now() >= e) {
            break;
        }
        let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let ev = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                NSDefaultRunLoopMode,
                true,
            )
        };
        if let Some(ev) = ev {
            app.sendEvent(&ev);
        }
    }
}
