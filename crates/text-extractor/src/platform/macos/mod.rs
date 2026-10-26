//! macOS: Carbon hotkey, AppKit selection overlay, ScreenCaptureKit capture,
//! Vision OCR, HUD toast.

mod capture;
mod cg;
mod hotkey;
pub(crate) mod main_thread;
mod overlay;
#[cfg(test)]
mod tests;
mod toast;
mod vision;

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use objc2::MainThreadMarker;

pub use hotkey::HotkeyReg;

use super::CaptureCallback;
use crate::text::Fragment;
use crate::Config;
use cg::CgImage;
use main_thread::on_main;

pub fn screen_recording_granted() -> bool {
    unsafe { cg::CGPreflightScreenCaptureAccess() }
}

pub fn request_screen_recording() -> bool {
    unsafe { cg::CGRequestScreenCaptureAccess() }
}

pub fn ocr_file(path: &Path, cfg: &Config) -> Result<Vec<Fragment>> {
    vision::recognize(vision::Source::File(path), cfg)
}

pub fn ocr_rgba(w: usize, h: usize, rgba: &[u8], cfg: &Config) -> Result<Vec<Fragment>> {
    let img = CgImage::from_rgba(w, h, rgba).ok_or_else(|| anyhow!("could not build an image"))?;
    vision::recognize(vision::Source::Image(img.as_ref()), cfg)
}

pub fn supported_languages() -> Vec<String> {
    vision::supported_languages()
}

pub fn toast(text: &str) {
    toast::show(text.to_string());
}

pub const PERMISSION_MESSAGE: &str =
    "Allow Screen Recording for PowerMac in System Settings → Privacy & Security";

/// Capture a region (global top-left points) and recognize it — the capture
/// half of [`capture_selection`] without the overlay. Call off the main thread.
pub fn capture_rect_text(x: f64, y: f64, w: f64, h: f64, cfg: &Config) -> Result<String> {
    let img = capture::capture(capture::GRect { x, y, w, h })?;
    let frags = vision::recognize(vision::Source::Image(img.as_ref()), cfg)?;
    Ok(crate::text::assemble(frags, cfg.single_line))
}

/// Selection overlay → capture → OCR. `cb` runs on a worker thread (or the
/// main thread when cancelled / denied).
pub fn capture_selection(cfg: Config, cb: CaptureCallback) {
    on_main(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if !screen_recording_granted() {
            // Shows the system prompt the first time; afterwards it only
            // reports the state.
            if !request_screen_recording() {
                let _ = std::process::Command::new("open")
                    .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                    .spawn();
                cb(Err(anyhow!(PERMISSION_MESSAGE)));
                return;
            }
        }
        overlay::select(
            mtm,
            Box::new(move |sel| match sel {
                None => cb(Ok(None)),
                Some(rect) => {
                    std::thread::spawn(move || {
                        // Let the overlay windows disappear before capturing.
                        std::thread::sleep(Duration::from_millis(120));
                        let res = capture::capture(rect).and_then(|img| {
                            let frags =
                                vision::recognize(vision::Source::Image(img.as_ref()), &cfg)?;
                            Ok(Some(crate::text::assemble(frags, cfg.single_line)))
                        });
                        cb(res);
                    });
                }
            }),
        );
    });
}

/// [`capture_selection`], blocking. On the main thread it pumps AppKit
/// events itself (CLI use).
pub fn capture_blocking(cfg: Config) -> Result<Option<String>> {
    use std::sync::{mpsc, Arc, Mutex};
    if let Some(mtm) = MainThreadMarker::new() {
        let slot: Arc<Mutex<Option<Result<Option<String>>>>> = Arc::new(Mutex::new(None));
        let s = slot.clone();
        capture_selection(cfg, Box::new(move |r| *s.lock().unwrap() = Some(r)));
        main_thread::pump_until(mtm, None, || slot.lock().unwrap().is_some());
        let r = slot.lock().unwrap().take().unwrap_or(Ok(None));
        r
    } else {
        let (tx, rx) = mpsc::channel();
        capture_selection(
            cfg,
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        );
        rx.recv().unwrap_or(Ok(None))
    }
}

/// Show the selection overlay for `ms` milliseconds, then cancel it
/// programmatically (smoke test of the real windows; main thread).
pub fn overlay_smoke(ms: u64) -> Result<bool> {
    use std::sync::{Arc, Mutex};
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow!("call on the main thread"))?;
    let slot: Arc<Mutex<Option<Option<GRectOut>>>> = Arc::new(Mutex::new(None));
    let s = slot.clone();
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    if !app.isRunning() {
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
    }
    overlay::select(
        mtm,
        Box::new(move |r| *s.lock().unwrap() = Some(r.map(|r| (r.x, r.y, r.w, r.h)))),
    );
    let shown = overlay::is_active();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        main_thread::dispatch_main(overlay::cancel);
    });
    main_thread::pump_until(mtm, Some(Duration::from_millis(ms + 3000)), || {
        slot.lock().unwrap().is_some()
    });
    let cancelled = matches!(slot.lock().unwrap().take(), Some(None));
    Ok(shown && cancelled && !overlay::is_active())
}

type GRectOut = (f64, f64, f64, f64);

/// Close a selection overlay that is still open.
pub fn cancel_selection() {
    on_main(overlay::cancel);
}

/// Offscreen renders of the overlay and the toast into PNGs in `dir`
/// (main thread only; no windows are shown and nothing is captured).
pub fn debug_render(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow!("call on the main thread"))?;
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    let mut save = |name: &str, f: &dyn Fn(&str) -> bool| -> Result<()> {
        let p = dir.join(name);
        anyhow::ensure!(f(&p.to_string_lossy()), "rendering {name} failed");
        out.push(p);
        Ok(())
    };
    save("overlay-idle.png", &|p| {
        overlay::debug_render_png(mtm, 1000.0, 620.0, None, p)
    })?;
    save("overlay-selecting.png", &|p| {
        overlay::debug_render_png(mtm, 1000.0, 620.0, Some((64.0, 380.0, 520.0, 160.0)), p)
    })?;
    save("toast-copied.png", &|p| {
        toast::debug_render_png(mtm, "Copied 42 words", p)
    })?;
    save("toast-permission.png", &|p| {
        toast::debug_render_png(mtm, PERMISSION_MESSAGE, p)
    })?;
    Ok(out)
}

/// Test hook: recognize a CGImage built in tests.
#[cfg(test)]
pub(crate) fn ocr_cgimage(img: &CgImage, cfg: &Config) -> Result<Vec<Fragment>> {
    vision::recognize(vision::Source::Image(img.as_ref()), cfg)
}
