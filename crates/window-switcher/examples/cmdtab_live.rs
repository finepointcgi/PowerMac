//! Live check of the Cmd+Tab take-over on macOS (drives TextEdit windows
//! you opened yourself; touches nothing else):
//!
//! ```sh
//! open -a TextEdit /tmp/x/a.txt /tmp/x/b.txt /tmp/x/c.txt
//! cargo run -p window-switcher --example cmdtab_live -- --out DIR
//! ```
//!
//! 1. crash recovery: a child process takes over Cmd+Tab and aborts; the
//!    system switcher stays off until the next start turns it back on;
//! 2. Cmd+` / Cmd+Tab fast switches, Cmd held (overlay + preview of the
//!    selected window), Cmd+Shift+Tab, release to switch, Esc, Cmd+M on a
//!    TextEdit window (centered "Minimized" preview), Return;
//! 3. a broken keyboard hook hands Cmd+Tab back to macOS;
//! 4. stopping restores the system switcher.
//!
//! Key presses are only posted while TextEdit is the front app. The system
//! hotkeys are put back to their initial state at the end, even on panic.

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
    use window_switcher::{guard, Config, Hotkey, WindowSwitcher};

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
    extern "C" {
        fn dlsym(h: *mut c_void, s: *const i8) -> *mut c_void;
    }

    const CMD: u64 = 0x0010_0000;
    const SHIFT: u64 = 0x0002_0000;
    const K_CMD: u16 = 0x37;
    const K_SHIFT: u16 = 0x38;
    const K_TAB: u16 = 0x30;
    const K_GRAVE: u16 = 0x32;
    const K_ESC: u16 = 0x35;
    const K_RET: u16 = 0x24;
    const K_M: u16 = 0x2E;
    const IDS: [i32; 4] = [1, 2, 6, 27];

    fn post(key: u16, down: bool, flags: u64) {
        unsafe {
            let e = CGEventCreateKeyboardEvent(std::ptr::null(), key, down);
            CGEventSetFlags(e, flags);
            CGEventPost(0, e);
            CFRelease(e);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    fn tap(key: u16, flags: u64) {
        post(key, true, flags);
        post(key, false, flags);
    }
    fn wait(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn set_hotkey(id: i32, on: bool) {
        type F = unsafe extern "C" fn(i32, bool) -> i32;
        let p = unsafe { dlsym(-2isize as *mut c_void, c"CGSSetSymbolicHotKeyEnabled".as_ptr()) };
        if !p.is_null() {
            let f: F = unsafe { std::mem::transmute(p) };
            unsafe { f(id, on) };
        }
    }

    fn states() -> Vec<bool> {
        IDS.iter().map(|&i| window_switcher::system_hotkey_enabled(i).unwrap_or(true)).collect()
    }

    /// Puts the system hotkeys back to how they were, whatever happens.
    struct Restore(Vec<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (i, &on) in IDS.iter().zip(&self.0) {
                if window_switcher::system_hotkey_enabled(*i) != Some(on) {
                    set_hotkey(*i, on);
                }
            }
            println!("restored system hotkeys: {:?} (initial {:?})", states(), self.0);
        }
    }

    fn cfstr(s: &str) -> CFTypeRef {
        let c = std::ffi::CString::new(s).unwrap();
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), 0x0800_0100) }
    }

    /// (bundle id, focused window title)
    fn front() -> (String, String) {
        unsafe {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            let app: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
            let Some(app) = app else { return Default::default() };
            let pid: i32 = msg_send![&*app, processIdentifier];
            let bid: Option<Retained<objc2_foundation::NSString>> = msg_send![&*app, bundleIdentifier];
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
            (bid.map(|b| b.to_string()).unwrap_or_default(), title)
        }
    }

    const TEXTEDIT: &str = "com.apple.TextEdit";

    fn textedit_front() -> bool {
        front().0 == TEXTEDIT
    }

    fn bring_textedit() -> bool {
        let _ = std::process::Command::new("open").args(["-a", "TextEdit"]).status();
        for _ in 0..30 {
            if textedit_front() {
                return true;
            }
            wait(100);
        }
        false
    }

    fn marker_exists(pid: u32) -> bool {
        std::env::temp_dir().join(guard::marker_file_name(pid)).exists()
    }

    fn config() -> Config {
        Config {
            replace_cmd_tab: true,
            replace_cmd_backtick: true,
            // Only the Cmd shortcuts: the user's own PowerMac owns Option+Tab.
            trigger: Hotkey::none(),
            app_windows_trigger: Hotkey::none(),
            ..Config::default()
        }
    }

    fn wait_for(what: &str, ms: u64, f: impl Fn() -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if f() {
                return true;
            }
            wait(50);
        }
        println!("  (timed out waiting for {what})");
        false
    }

    pub fn crash_child() {
        let h = WindowSwitcher::start(config()).unwrap();
        let ok = wait_for("take-over", 5000, || window_switcher::system_switcher_enabled() == Some(false));
        println!("child {}: took over Cmd+Tab = {ok}, marker = {}", std::process::id(), marker_exists(std::process::id()));
        std::mem::forget(h);
        std::process::abort();
    }

    pub fn main() -> anyhow::Result<()> {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--crash-child") {
            crash_child();
        }
        let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1).cloned()).unwrap_or_else(|| ".".into());
        let initial = states();
        println!("initial system hotkeys {IDS:?}: {initial:?}");
        let restore = Restore(initial.clone());
        let mut results: Vec<(String, bool)> = Vec::new();
        let mut check = |name: &str, ok: bool| {
            println!("[{}] {name}", if ok { "PASS" } else { "FAIL" });
            results.push((name.to_string(), ok));
        };

        // ---- 1. crash recovery
        let me = std::env::current_exe()?;
        let st = std::process::Command::new(&me).arg("--crash-child").status()?;
        println!("child exited: {st:?}");
        check("after a crash the system Cmd+Tab is still off (what recovery is for)", window_switcher::system_switcher_enabled() == Some(false));
        let leftover = std::fs::read_dir(std::env::temp_dir())?
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(guard::MARKER_PREFIX));
        check("the crashed run left a marker", leftover);

        let mtm = objc2::MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        // A fresh start (override off) turns it back on and cleans up.
        let handle = Arc::new(WindowSwitcher::start(Config { trigger: Hotkey::none(), app_windows_trigger: Hotkey::none(), ..Config::default() })?);
        check("next start re-enables the system Cmd+Tab", window_switcher::system_switcher_enabled() == Some(true));
        let leftover = std::fs::read_dir(std::env::temp_dir())?
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(guard::MARKER_PREFIX));
        check("stale marker removed", !leftover);

        let done = Arc::new(AtomicBool::new(false));
        let render = Arc::new(std::sync::Mutex::new(None::<String>));
        let results = Arc::new(std::sync::Mutex::new(results));
        {
            let (h, done, render, results) = (handle.clone(), done.clone(), render.clone(), results.clone());
            std::thread::spawn(move || {
                let check = |name: &str, ok: bool| {
                    println!("[{}] {name}", if ok { "PASS" } else { "FAIL" });
                    results.lock().unwrap().push((name.to_string(), ok));
                };
                let run = || -> Result<(), String> {
                    wait_for("event tap", 5000, || h.input_active());
                    h.update(config());
                    let off = wait_for("take-over", 3000, || window_switcher::system_switcher_enabled() == Some(false));
                    check("override on: system Cmd+Tab and Cmd+Shift+Tab off", off);
                    check("Cmd+` hotkeys (6, 27) off too", window_switcher::system_hotkey_enabled(27) == Some(false));
                    check("marker written", marker_exists(std::process::id()));
                    if !off {
                        return Err("no take-over".into());
                    }
                    wait(2500); // window list
                    let tes: Vec<_> = h.windows().into_iter().filter(|w| w.app_id == TEXTEDIT && !w.minimized).collect();
                    println!("TextEdit windows: {:?}", tes.iter().map(|w| (&w.title, w.id)).collect::<Vec<_>>());
                    if tes.len() < 2 {
                        return Err("open at least two TextEdit windows first".into());
                    }
                    let id_of = |title: &str| h.windows().iter().find(|w| w.app_id == TEXTEDIT && w.title == title).map(|w| w.id);
                    if !bring_textedit() {
                        return Err("TextEdit didn't come to the front".into());
                    }
                    let x = front().1;
                    println!("start: {x:?}");

                    // Cmd+` tap: next TextEdit window.
                    post(K_CMD, true, CMD);
                    tap(K_GRAVE, CMD);
                    post(K_CMD, false, 0);
                    wait(800);
                    let y = front().1;
                    check(&format!("Cmd+` fast switch: {x:?} -> {y:?}"), textedit_front() && y != x);

                    // Cmd+Tab tap: back to the previous window (MRU).
                    if !textedit_front() {
                        return Err("TextEdit lost focus".into());
                    }
                    post(K_CMD, true, CMD);
                    tap(K_TAB, CMD);
                    post(K_CMD, false, 0);
                    wait(800);
                    check(&format!("Cmd+Tab fast switch returns to {x:?}"), front().1 == x);
                    check("no overlay after a fast switch", !h.is_shown() && window_switcher::preview::shown_preview().is_none());

                    // Hold Cmd: overlay + preview of the selected window.
                    if !textedit_front() {
                        return Err("TextEdit lost focus".into());
                    }
                    post(K_CMD, true, CMD);
                    let t0 = Instant::now();
                    tap(K_TAB, CMD);
                    let shown = wait_for("overlay", 1500, || h.is_shown());
                    println!("overlay after {:?}", t0.elapsed());
                    check("holding Cmd keeps the switcher open (overlay shown)", shown);
                    let pv = wait_for("preview", 1000, || window_switcher::preview::shown_preview().is_some());
                    let p = window_switcher::preview::shown_preview();
                    println!("preview: {p:?}");
                    check(
                        &format!("preview shows the selected window {y:?} at its real position"),
                        pv && p.map(|p| p.0 as u64) == id_of(&y) && p.is_some_and(|p| p.1),
                    );
                    *render.lock().unwrap() = Some("hold-cmd-tab".into());
                    wait(600);
                    tap(K_TAB, CMD);
                    wait(250);
                    let p2 = window_switcher::preview::shown_preview();
                    check("Tab moves the preview to the next window", p2.is_some() && p2.map(|p| p.0) != p.map(|p| p.0));
                    post(K_SHIFT, true, CMD | SHIFT);
                    tap(K_TAB, CMD | SHIFT);
                    post(K_SHIFT, false, CMD);
                    wait(250);
                    check("Cmd+Shift+Tab goes back", window_switcher::preview::shown_preview().map(|p| p.0) == p.map(|p| p.0));
                    check("still open after Shift released", h.is_shown());
                    post(K_CMD, false, 0);
                    let hidden = wait_for("hide", 500, || !h.is_shown());
                    wait(700);
                    check(&format!("releasing Cmd switches to {y:?}"), front().1 == y);
                    check("overlay and preview gone after the switch", hidden && window_switcher::preview::shown_preview().is_none());

                    // Esc cancels.
                    if !textedit_front() {
                        return Err("TextEdit lost focus".into());
                    }
                    let before = front();
                    post(K_CMD, true, CMD);
                    tap(K_TAB, CMD);
                    wait_for("overlay", 1500, || h.is_shown());
                    tap(K_ESC, CMD);
                    let gone = wait_for("hide", 500, || !h.is_shown());
                    post(K_CMD, false, 0);
                    wait(600);
                    check("Esc cancels (nothing switched, overlay + preview gone)", gone && front() == before && window_switcher::preview::shown_preview().is_none());

                    // Cmd+M on a TextEdit window (Cmd+` list = TextEdit only).
                    if !textedit_front() {
                        return Err("TextEdit lost focus".into());
                    }
                    post(K_CMD, true, CMD);
                    tap(K_GRAVE, CMD);
                    wait_for("overlay", 1500, || h.is_shown());
                    wait(300);
                    let sel = window_switcher::preview::shown_preview();
                    let sel_is_textedit = sel.is_some_and(|s| h.windows().iter().any(|w| w.id == s.0 as u64 && w.app_id == TEXTEDIT));
                    if sel_is_textedit {
                        tap(K_M, CMD);
                        let min = wait_for("minimized", 3000, || {
                            h.windows().iter().any(|w| sel.is_some_and(|s| w.id == s.0 as u64) && w.minimized)
                        });
                        check("Cmd+M minimizes the selected TextEdit window; switcher stays open", min && h.is_shown());
                        let centered = wait_for("centered preview", 2000, || {
                            window_switcher::preview::shown_preview().is_some_and(|p| !p.1 && p.2 == Some("Minimized"))
                        });
                        check("minimized window: centered preview with a Minimized badge", centered);
                        *render.lock().unwrap() = Some("minimized".into());
                        wait(600);
                        tap(K_RET, CMD);
                        post(K_CMD, false, 0);
                        let restored = wait_for("restored", 3000, || {
                            h.windows().iter().any(|w| sel.is_some_and(|s| w.id == s.0 as u64) && !w.minimized)
                        });
                        check("Return switches to (and restores) the minimized window", restored);
                    } else {
                        tap(K_ESC, CMD);
                        post(K_CMD, false, 0);
                        check("Cmd+` selected a TextEdit window", false);
                    }
                    wait(800);

                    // A broken hook gives Cmd+Tab back to macOS.
                    h.debug_break_input(true);
                    let back = wait_for("system switcher back", 1000, || window_switcher::system_switcher_enabled() == Some(true));
                    check("hook broken -> system Cmd+Tab back on", back);
                    check("marker removed while nothing is taken over", !marker_exists(std::process::id()));
                    h.debug_break_input(false);
                    let again = wait_for("take-over", 1000, || window_switcher::system_switcher_enabled() == Some(false));
                    check("hook healthy again -> taken over again", again);

                    // Turning the option off restores it at once.
                    h.update(Config { replace_cmd_tab: false, ..config() });
                    check("option off -> system Cmd+Tab back on", window_switcher::system_switcher_enabled() == Some(true));
                    h.update(config());
                    wait_for("take-over", 1000, || window_switcher::system_switcher_enabled() == Some(false));
                    Ok(())
                };
                if let Err(e) = run() {
                    println!("[FAIL] aborted: {e}");
                    results.lock().unwrap().push((format!("aborted: {e}"), false));
                }
                // Never leave a modifier down.
                post(K_SHIFT, false, 0);
                post(K_CMD, false, 0);
                done.store(true, Ordering::SeqCst);
            });
        }
        let end = Instant::now() + Duration::from_secs(90);
        while Instant::now() < end && !done.load(Ordering::SeqCst) {
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };
            if let Some(name) = render.lock().unwrap().take() {
                let path = format!("{out}/live-{name}.png");
                match window_switcher::preview::render_live_png(&path) {
                    Ok(true) => println!("rendered {path}"),
                    Ok(false) => println!("render: overlay not shown"),
                    Err(e) => println!("render failed: {e}"),
                }
            }
        }
        handle.stop();
        let mut results = std::mem::take(&mut *results.lock().unwrap());
        let restored = window_switcher::system_switcher_enabled() == Some(true);
        println!("[{}] stop restores the system Cmd+Tab", if restored { "PASS" } else { "FAIL" });
        results.push(("stop restores the system Cmd+Tab".into(), restored));
        results.push(("marker removed on stop".into(), !marker_exists(std::process::id())));
        drop(restore);
        let failed: Vec<_> = results.iter().filter(|r| !r.1).collect();
        println!("\n{} checks, {} failed", results.len(), failed.len());
        for f in &failed {
            println!("  FAILED: {}", f.0);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
