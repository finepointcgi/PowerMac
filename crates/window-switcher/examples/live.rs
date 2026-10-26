//! Live smoke test on macOS: runs the switcher with an NSApplication main
//! loop and drives it with synthetic key events.
//!
//! ```sh
//! open -a TextEdit a.txt b.txt c.txt   # a few windows of one app
//! cargo run -p window-switcher --example live -- [--trigger ctrl-opt] [--out DIR] [--script]
//! ```
//! Without `--script` it just runs for 60 s so you can try it by hand.

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use window_switcher::{Config, Hotkey, WindowSwitcher};

    type CFTypeRef = *const c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(e: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: CFTypeRef;
        fn CFRunLoopRunInMode(mode: CFTypeRef, secs: f64, ret: u8) -> i32;
        fn CFRelease(r: CFTypeRef);
        fn CFStringCreateWithCString(a: CFTypeRef, s: *const i8, enc: u32) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, n: isize, enc: u32) -> u8;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: bool) -> CFTypeRef;
        fn CGEventSetFlags(e: CFTypeRef, f: u64);
        fn CGEventPost(tap: u32, e: CFTypeRef);
    }

    const ALT: u64 = 0x0008_0000;
    const CTRL: u64 = 0x0004_0000;
    const K_OPT: u16 = 0x3A;
    const K_CTRL: u16 = 0x3B;
    const K_TAB: u16 = 0x30;
    const K_GRAVE: u16 = 0x32;
    const K_ESC: u16 = 0x35;
    const K_RET: u16 = 0x24;
    const K_M: u16 = 0x2E;
    const K_W: u16 = 0x0D;

    fn post(key: u16, down: bool, flags: u64) {
        unsafe {
            let e = CGEventCreateKeyboardEvent(std::ptr::null(), key, down);
            CGEventSetFlags(e, flags);
            CGEventPost(0, e); // kCGHIDEventTap
            CFRelease(e);
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    fn cfstr(s: &str) -> CFTypeRef {
        let c = std::ffi::CString::new(s).unwrap();
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), 0x0800_0100) }
    }

    /// (app name, focused window title)
    fn front() -> (String, String) {
        unsafe {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            let app: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
            let Some(app) = app else { return Default::default() };
            let pid: i32 = msg_send![&*app, processIdentifier];
            let name: Retained<objc2_foundation::NSString> = msg_send![&*app, localizedName];
            let ax = AXUIElementCreateApplication(pid);
            let (fw, t) = (cfstr("AXFocusedWindow"), cfstr("AXTitle"));
            let mut win: CFTypeRef = std::ptr::null();
            let mut title = String::new();
            if AXUIElementCopyAttributeValue(ax, fw, &mut win) == 0 {
                let mut s: CFTypeRef = std::ptr::null();
                if AXUIElementCopyAttributeValue(win, t, &mut s) == 0 {
                    let mut buf = [0u8; 512];
                    CFStringGetCString(s, buf.as_mut_ptr(), 512, 0x0800_0100);
                    title = String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)]).into();
                    CFRelease(s);
                }
                CFRelease(win);
            }
            CFRelease(ax);
            CFRelease(fw);
            CFRelease(t);
            (name.to_string(), title)
        }
    }

    pub fn main() -> anyhow::Result<()> {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
        let args: Vec<String> = std::env::args().collect();
        let script = args.iter().any(|a| a == "--script");
        let ctrl_opt = args.iter().any(|a| a == "ctrl-opt");
        let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1).cloned()).unwrap_or_else(|| ".".into());
        let mtm = objc2::MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        let mut cfg = Config::default();
        let held = if ctrl_opt {
            cfg.trigger = Hotkey::new(false, true, true, false, 0x09);
            cfg.app_windows_trigger = Hotkey::new(false, true, true, false, 0xC0);
            ALT | CTRL
        } else {
            ALT
        };
        let handle = Arc::new(WindowSwitcher::start(cfg)?);
        println!("permissions: {:?}", window_switcher::permissions());

        let render = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        if script {
            let (h, render, done, out) = (handle.clone(), render.clone(), done.clone(), out.clone());
            std::thread::spawn(move || {
                let wait = |ms| std::thread::sleep(Duration::from_millis(ms));
                let down_mods = || {
                    post(K_OPT, true, ALT);
                    if held & CTRL != 0 {
                        post(K_CTRL, true, held);
                    }
                };
                let up_mods = || {
                    if held & CTRL != 0 {
                        post(K_CTRL, false, ALT);
                    }
                    post(K_OPT, false, 0);
                };
                wait(3000); // let the worker enumerate
                println!("input active: {}", h.input_active());
                for w in h.windows().iter().take(12) {
                    println!("  window {:>6} {:<12} {:?} min={} hidden={} space={}", w.id, w.app_name, w.title, w.minimized, w.app_hidden, w.other_space);
                }
                println!("start: {:?}", front());

                // 1. Fast switch (tap + release < show delay) → previous window.
                down_mods();
                post(K_TAB, true, held);
                post(K_TAB, false, held);
                up_mods();
                wait(700);
                println!("1 fast switch -> {:?} (shown during: no)", front());

                // 2. Back again (MRU: should return to the start window).
                down_mods();
                post(K_TAB, true, held);
                post(K_TAB, false, held);
                up_mods();
                wait(700);
                println!("2 fast switch back -> {:?}", front());

                // 3. App windows (`) held: overlay shows, Tab twice, render, release.
                down_mods();
                post(K_GRAVE, true, held);
                post(K_GRAVE, false, held);
                wait(400);
                println!("3 shown after hold: {}", h.is_shown());
                post(K_GRAVE, true, held);
                post(K_GRAVE, false, held);
                wait(300);
                render.store(true, Ordering::SeqCst);
                wait(600);
                up_mods();
                wait(700);
                println!("3 app-windows third window -> {:?}; shown after release: {}", front(), h.is_shown());

                // 4. Escape cancels.
                let before = front();
                down_mods();
                post(K_GRAVE, true, held);
                wait(300);
                post(K_ESC, true, held);
                post(K_ESC, false, held);
                post(K_GRAVE, false, held);
                up_mods();
                wait(500);
                println!("4 escape: unchanged = {} ({:?})", front() == before, front());

                // 5. M minimizes the selected window (previous TextEdit window), then switch to it.
                down_mods();
                post(K_GRAVE, true, held);
                post(K_GRAVE, false, held);
                wait(300);
                post(K_M, true, held);
                post(K_M, false, held);
                wait(1200);
                println!("5 after M: shown={}", h.is_shown());
                render.store(true, Ordering::SeqCst);
                wait(500);
                post(K_RET, true, held);
                post(K_RET, false, held);
                up_mods();
                wait(1500);
                println!("5 switched to minimized window -> {:?}", front());

                // 6. Sticky show() from the API, then Return.
                h.show_app_windows();
                wait(400);
                println!("6 show(): shown={}", h.is_shown());
                post(K_RET, true, 0);
                post(K_RET, false, 0);
                wait(700);
                println!("6 after Return -> {:?} shown={}", front(), h.is_shown());

                // 7. W closes the selected TextEdit window.
                let n_before = h.windows().iter().filter(|w| w.app_name == "TextEdit").count();
                down_mods();
                post(K_GRAVE, true, held);
                post(K_GRAVE, false, held);
                wait(300);
                post(K_W, true, held);
                post(K_W, false, held);
                wait(2800);
                let n_after = h.windows().iter().filter(|w| w.app_name == "TextEdit").count();
                println!("7 W: TextEdit windows {n_before} -> {n_after}");
                post(K_ESC, true, held);
                post(K_ESC, false, held);
                up_mods();
                wait(400);
                let _ = out;
                done.store(true, Ordering::SeqCst);
            });
        }
        let end = Instant::now() + Duration::from_secs(if script { 45 } else { 60 });
        let mut n = 0;
        while Instant::now() < end && !done.load(Ordering::SeqCst) {
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };
            if render.swap(false, Ordering::SeqCst) {
                n += 1;
                let path = format!("{out}/live-{n}.png");
                match window_switcher::preview::render_live_png(&path) {
                    Ok(true) => println!("rendered {path}"),
                    Ok(false) => println!("render: overlay not shown"),
                    Err(e) => println!("render failed: {e}"),
                }
            }
        }
        handle.stop();
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
