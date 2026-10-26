//! Manual checks:
//! * `cargo run -p text-extractor --example probe ocr <image>` — OCR a file.
//! * `... probe clipboard` — OCR the image on the clipboard.
//! * `... probe languages` — recognition languages.
//! * `... probe permission` — Screen Recording permission state (no prompt).
//! * `... probe hotkey` — register and unregister the default hotkey.
//! * `... probe render <dir>` — offscreen PNGs of the overlay and the toast.
//! * `... probe selftest` — show a window with known text, capture its
//!   region (only that window) and OCR it (needs Screen Recording).
//! * `... probe overlay` — open the real overlay and cancel it after 1.2 s.
//! * `... probe toast` — show the toast.
//! * `... probe select` — the real overlay + capture (needs Screen Recording).

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = text_extractor::Config::default();
    match args.first().map(String::as_str) {
        Some("ocr") => println!("{}", text_extractor::ocr_image(args.get(1).expect("image path"))?),
        Some("clipboard") => println!("{}", text_extractor::ocr_clipboard(&cfg)?),
        Some("languages") => println!("{}", text_extractor::supported_languages().join(", ")),
        Some("permission") => println!("{:?}", text_extractor::screen_recording_permission()),
        Some("hotkey") => {
            let h = text_extractor::TextExtractor::start(cfg.clone())?;
            println!("registered {}", cfg.hotkey);
            drop(h);
            println!("unregistered");
        }
        #[cfg(target_os = "macos")]
        Some("render") => {
            for p in text_extractor::debug_render(std::path::Path::new(args.get(1).expect("dir")))? {
                println!("{}", p.display());
            }
        }
        #[cfg(target_os = "macos")]
        Some("overlay") => println!("overlay opened and cancelled: {}", text_extractor::debug_overlay_smoke(1200)?),
        #[cfg(target_os = "macos")]
        Some("toast") => {
            text_extractor::debug_toast("Copied 42 words");
            let mtm = objc2::MainThreadMarker::new().unwrap();
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
            app.finishLaunching();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < end {
                let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
                if let Some(ev) = unsafe { app.nextEventMatchingMask_untilDate_inMode_dequeue(objc2_app_kit::NSEventMask::Any, Some(&until), objc2_foundation::NSDefaultRunLoopMode, true) } {
                    app.sendEvent(&ev);
                }
            }
            println!("toast shown");
        }
        #[cfg(target_os = "macos")]
        Some("selftest") => selftest::run()?,
        Some("select") => println!("{:?}", text_extractor::capture_once(&cfg)?),
        _ => eprintln!("usage: probe ocr <image> | clipboard | languages | permission | hotkey | render <dir> | select"),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod selftest {
    use objc2::rc::Retained;
    use objc2::MainThreadOnly;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont,
        NSScreen, NSTextField, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    pub fn run() -> anyhow::Result<()> {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        anyhow::ensure!(
            text_extractor::screen_recording_permission() == text_extractor::Permission::Granted,
            "Screen Recording is not granted"
        );
        let screen = NSScreen::screens(mtm).iter().next().expect("screen");
        let sf = screen.frame();
        let (w, h) = (640.0, 200.0);
        // Cocoa frame near the top-left of the main screen.
        let frame = NSRect::new(
            NSPoint::new(60.0, sf.size.height - 120.0 - h),
            NSSize::new(w, h),
        );
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setBackgroundColor(Some(&NSColor::whiteColor()));
        window.setLevel(1000);
        let content = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        );
        let lines = [
            "Screen capture self test",
            "The quick brown fox 1234",
            "Bottom marker line",
        ];
        for (i, l) in lines.iter().enumerate() {
            let t = NSTextField::labelWithString(&NSString::from_str(l), mtm);
            t.setFont(Some(&NSFont::systemFontOfSize(30.0)));
            t.setTextColor(Some(&NSColor::blackColor()));
            t.sizeToFit();
            let sz = t.frame().size;
            t.setFrame(NSRect::new(
                NSPoint::new(24.0, h - 60.0 - i as f64 * 60.0),
                sz,
            ));
            content.addSubview(&t);
        }
        window.setContentView(Some(&content));
        window.orderFrontRegardless();
        let _keep: Retained<NSWindow> = window.clone();
        // Global top-left coordinates of the window.
        let (gx, gy) = (frame.origin.x, sf.size.height - (frame.origin.y + h));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            let _ = tx.send(text_extractor::debug_capture_rect(
                gx + 4.0,
                gy + 4.0,
                w - 8.0,
                h - 8.0,
                &text_extractor::Config::default(),
            ));
        });
        let res = loop {
            if let Ok(r) = rx.try_recv() {
                break r;
            }
            let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            if let Some(ev) = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    objc2_app_kit::NSEventMask::Any,
                    Some(&until),
                    objc2_foundation::NSDefaultRunLoopMode,
                    true,
                )
            } {
                app.sendEvent(&ev);
            }
        };
        window.orderOut(None);
        window.close();
        let text = res?;
        // Only check for our own lines (other windows may float above ours);
        // don't print what was captured.
        let norm = |s: &str| {
            s.to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let got: Vec<String> = text.lines().map(norm).collect();
        for l in lines {
            anyhow::ensure!(
                got.iter().any(|g| g.starts_with(&norm(l))),
                "line {l:?} was not recognized"
            );
        }
        println!(
            "selftest OK: captured the test window and recognized all {} lines",
            lines.len()
        );
        Ok(())
    }
}
