//! Starts Mouse Utilities for N seconds (default 8), triggers Find My Mouse,
//! then toggles the crosshairs and highlighter programmatically.
//!
//! ```text
//! cargo run -p mouse-utils --example mouse_utils_demo -- 8
//! ```
//!
//! On macOS it also lists this process's on-screen windows (via
//! `CGWindowListCopyWindowInfo`) to show that the overlays were created and
//! later removed. Input hooks are optional: without Accessibility / Input
//! Monitoring the demo still works, only hotkeys and double-Ctrl are off.

use std::time::{Duration, Instant};

use mouse_utils::{run_main_loop_for, Config, MouseUtils};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    let total = Duration::from_secs(secs.max(4));

    let mut cfg = Config::default();
    cfg.highlighter.highlight_cursor = true;
    let mut handle = MouseUtils::start(cfg)?;
    println!("input mode: {:?}", handle.input_mode());
    let start = Instant::now();

    // 1) Find My Mouse.
    handle.find_mouse();
    run_main_loop_for(Duration::from_millis(1200));
    println!("[{:>5}ms] spotlight active: {}; overlay windows on screen: {}", start.elapsed().as_millis(), handle.spotlight_active(), overlay_windows());
    probe("spotlight");

    // 2) Crosshairs on (spotlight stays until the mouse moves + 1 s idle, a click or a key).
    println!("crosshairs on: {}", handle.toggle_crosshairs());
    run_main_loop_for(Duration::from_millis(1200));
    println!("[{:>5}ms] overlay windows on screen: {}", start.elapsed().as_millis(), overlay_windows());
    probe("spotlight+crosshairs");

    // 3) Highlighter on (with cursor highlight), then everything off.
    println!("highlighter on: {}", handle.toggle_highlighter());
    let rest = total.saturating_sub(start.elapsed()).saturating_sub(Duration::from_millis(1000));
    run_main_loop_for(rest);
    probe("spotlight+crosshairs+cursor highlight");
    println!("crosshairs on: {}", handle.toggle_crosshairs());
    println!("highlighter on: {}", handle.toggle_highlighter());
    run_main_loop_for(Duration::from_millis(700));
    println!("[{:>5}ms] spotlight active: {}; overlay windows on screen: {}", start.elapsed().as_millis(), handle.spotlight_active(), overlay_windows());

    // 4) Fade the spotlight out: the overlay windows are ordered out once nothing is visible.
    handle.hide_spotlight();
    run_main_loop_for(Duration::from_millis(800));
    println!("[{:>5}ms] after hide_spotlight: active {}; overlay windows on screen: {}", start.elapsed().as_millis(), handle.spotlight_active(), overlay_windows());

    handle.stop();
    run_main_loop_for(Duration::from_millis(300));
    println!("stopped; overlay windows on screen: {}", overlay_windows());
    Ok(())
}

/// Print rendered overlay pixels (premultiplied RGBA) around the cursor.
fn probe(label: &str) {
    let pts = [(0.0, 0.0), (10.0, 10.0), (150.0, 0.0), (150.0, 150.0)];
    let px = mouse_utils::debug_probe(&pts);
    let parts: Vec<String> = pts.iter().zip(px).map(|(p, v)| format!("{p:?}={v:?}")).collect();
    println!("  pixels [{label}] cursor+offset: {}", parts.join("  "));
}

/// Count this process's on-screen windows at the overlay level.
#[cfg(target_os = "macos")]
fn overlay_windows() -> String {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    use objc2_foundation::NSString;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut AnyObject;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }
    const ON_SCREEN_ONLY: u32 = 1;
    let pid = std::process::id() as i32;
    objc2::rc::autoreleasepool(|_| unsafe {
        let arr = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY, 0);
        if arr.is_null() {
            return "unavailable".into();
        }
        let n: usize = msg_send![&*arr, count];
        let (k_pid, k_layer, k_bounds) =
            (NSString::from_str("kCGWindowOwnerPID"), NSString::from_str("kCGWindowLayer"), NSString::from_str("kCGWindowBounds"));
        let mut found = Vec::new();
        for i in 0..n {
            let d: *mut AnyObject = msg_send![&*arr, objectAtIndex: i];
            let p: Option<Retained<AnyObject>> = msg_send![&*d, objectForKey: &*k_pid];
            let Some(p) = p else { continue };
            let wp: i32 = msg_send![&*p, intValue];
            if wp != pid {
                continue;
            }
            let l: Option<Retained<AnyObject>> = msg_send![&*d, objectForKey: &*k_layer];
            let layer: i32 = l.map(|l| msg_send![&*l, intValue]).unwrap_or(-1);
            let b: Option<Retained<AnyObject>> = msg_send![&*d, objectForKey: &*k_bounds];
            let desc = b
                .map(|b| {
                    let s: Retained<NSString> = msg_send![&*b, description];
                    s.to_string().split_whitespace().collect::<Vec<_>>().join(" ")
                })
                .unwrap_or_default();
            found.push(format!("layer {layer} {desc}"));
        }
        CFRelease(arr as *const _);
        format!("{} {:?}", found.len(), found)
    })
}

#[cfg(not(target_os = "macos"))]
fn overlay_windows() -> String {
    "(not checked on this OS)".into()
}
