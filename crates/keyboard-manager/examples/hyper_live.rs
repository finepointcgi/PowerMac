//! Live check of the Caps Lock → Hyper key on macOS (needs Accessibility).
//!
//! ```text
//! cargo run -p keyboard-manager --example hyper_live -- /path/to/scratch.txt
//! ```
//!
//! Opens the scratch file in TextEdit and only posts keys while that
//! document is focused (checked through Accessibility before every event).
//! Starts Keyboard Manager with `hyperKey` on (which maps Caps Lock to F18
//! with hidutil), posts F18 / K events, watches what comes out with a
//! listen-only tap, then stops and checks that `hidutil property --get
//! UserKeyMapping` is exactly what it was before. Closes the document.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = std::env::args().nth(1).expect("scratch file path");
    std::process::exit(mac::run(&path));
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
mod mac {
    use std::ffi::{c_char, c_void};
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread::sleep;
    use std::time::Duration;

    use keyboard_manager::{Config, HyperKeyConfig, HyperMode, KeyboardManager, TapAction};

    type CFTypeRef = *const c_void;
    type CGEventRef = *mut c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFRunLoopCommonModes: *const c_void;
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRelease(cf: CFTypeRef);
        fn CFStringCreateWithCString(a: *const c_void, s: *const c_char, enc: u32) -> CFTypeRef;
        fn CFStringGetLength(s: CFTypeRef) -> isize;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
        fn CFRunLoopRunInMode(mode: *const c_void, secs: f64, ret: u8) -> i32;
        fn CFMachPortCreateRunLoopSource(a: *const c_void, p: *mut c_void, o: isize)
            -> *mut c_void;
        fn CFMachPortInvalidate(p: *mut c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, a: CFTypeRef, out: *mut CFTypeRef) -> i32;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(src: *mut c_void, key: u16, down: bool) -> CGEventRef;
        fn CGEventSetFlags(e: CGEventRef, f: u64);
        fn CGEventGetFlags(e: CGEventRef) -> u64;
        fn CGEventGetIntegerValueField(e: CGEventRef, f: u32) -> i64;
        fn CGEventPost(tap: u32, e: CGEventRef);
        fn CGEventSourceFlagsState(state: i32) -> u64;
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            mask: u64,
            cb: unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef,
            user: *mut c_void,
        ) -> *mut c_void;
        fn CGEventTapEnable(tap: *mut c_void, on: bool);
    }

    const UTF8: u32 = 0x0800_0100;
    const F18: u16 = 0x4F;
    const K: u16 = 0x28;
    const ESC: u16 = 0x35;
    const MODS: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    const ALPHA_SHIFT: u64 = 0x0001_0000;

    unsafe fn attr(el: CFTypeRef, name: &str) -> Option<CFTypeRef> {
        let c = std::ffi::CString::new(name).unwrap();
        let n = CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, n, &mut out);
        CFRelease(n);
        (err == 0 && !out.is_null()).then_some(out)
    }

    unsafe fn string(v: CFTypeRef) -> String {
        if CFGetTypeID(v) != CFStringGetTypeID() {
            return String::new();
        }
        let cap = CFStringGetLength(v) * 4 + 1;
        let mut buf = vec![0 as c_char; cap as usize];
        CFStringGetCString(v, buf.as_mut_ptr(), cap, UTF8);
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }

    fn textedit_pid() -> Option<i32> {
        let out = Command::new("pgrep")
            .args(["-x", "TextEdit"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// TextEdit frontmost, our window focused, a text area focused.
    fn focused(title: &str) -> bool {
        let Some(pid) = textedit_pid() else {
            return false;
        };
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            let front = attr(app, "AXFrontmost").is_some_and(|f| {
                let t = f == kCFBooleanTrue;
                CFRelease(f);
                t
            });
            let mut ok = front;
            if ok {
                ok = attr(app, "AXFocusedWindow").is_some_and(|w| {
                    let t = attr(w, "AXTitle").is_some_and(|t| {
                        let s = string(t);
                        CFRelease(t);
                        s == title
                    });
                    CFRelease(w);
                    t
                }) && attr(app, "AXFocusedUIElement").is_some_and(|e| {
                    let r = attr(e, "AXRole").is_some_and(|r| {
                        let s = string(r);
                        CFRelease(r);
                        s == "AXTextArea"
                    });
                    CFRelease(e);
                    r
                });
            }
            CFRelease(app);
            ok
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Seen {
        ty: u32,
        keycode: u16,
        mods: u64,
    }

    unsafe extern "C" fn listen_cb(
        _p: *mut c_void,
        ty: u32,
        e: CGEventRef,
        user: *mut c_void,
    ) -> CGEventRef {
        if matches!(ty, 10..=12) {
            let tx = &*(user as *const mpsc::Sender<Seen>);
            let _ = tx.send(Seen {
                ty,
                keycode: CGEventGetIntegerValueField(e, 9) as u16,
                mods: CGEventGetFlags(e) & MODS,
            });
        }
        e
    }

    fn listener() -> mpsc::Receiver<Seen> {
        let (tx, rx) = mpsc::channel();
        let (rtx, rrx) = mpsc::channel();
        std::thread::spawn(move || unsafe {
            let user = Box::into_raw(Box::new(tx)) as *mut c_void;
            // session level, appended, listen-only: sees what leaves KM's tap
            let tap = CGEventTapCreate(1, 1, 1, (1 << 10) | (1 << 11) | (1 << 12), listen_cb, user);
            assert!(!tap.is_null());
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            rtx.send(()).unwrap();
            for _ in 0..80 {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
            }
            CGEventTapEnable(tap, false);
            CFMachPortInvalidate(tap);
            CFRelease(src);
            CFRelease(tap);
        });
        rrx.recv().unwrap();
        rx
    }

    fn hidutil_get() -> String {
        let o = Command::new("hidutil")
            .args(["property", "--get", "UserKeyMapping"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn close_doc(name: &str) {
        let _ = Command::new("osascript")
            .args(["-e", &format!("tell application \"TextEdit\" to close (every document whose name is \"{name}\") saving no")])
            .status();
    }

    struct Poster<'a> {
        title: &'a str,
    }

    impl Poster<'_> {
        fn post(&self, keycode: u16, down: bool) -> bool {
            if !focused(self.title) {
                eprintln!("FOCUS LOST - not posting");
                return false;
            }
            unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
                let fnk = if keycode == F18 { 0x0080_0000 } else { 0 };
                CGEventSetFlags(e, fnk | 0x100);
                CGEventPost(0, e);
                CFRelease(e);
            }
            sleep(Duration::from_millis(20));
            true
        }
    }

    fn drain(rx: &mpsc::Receiver<Seen>) -> Vec<Seen> {
        sleep(Duration::from_millis(250));
        rx.try_iter().collect()
    }

    fn report(label: &str, pass: bool, detail: impl std::fmt::Debug) -> bool {
        println!("{} {label}: {detail:?}", if pass { "PASS" } else { "FAIL" });
        pass
    }

    pub fn run(path: &str) -> i32 {
        let before = hidutil_get();
        let name = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::fs::write(path, "").unwrap();
        Command::new("open")
            .args(["-a", "TextEdit", path])
            .status()
            .unwrap();
        let mut ready = false;
        for _ in 0..50 {
            sleep(Duration::from_millis(200));
            if focused(&name) {
                ready = true;
                break;
            }
        }
        if !ready {
            eprintln!("TextEdit document never got focus");
            close_doc(&name);
            return 2;
        }
        let p = Poster { title: &name };
        let rx = listener();
        let cfg = Config {
            hyper_key: HyperKeyConfig {
                mode: HyperMode::Hyper,
                tap_action: TapAction::Escape,
                tap_timeout_ms: 300,
            },
            ..Default::default()
        };
        let km = match KeyboardManager::start(cfg.clone()) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("start: {e:#}");
                close_doc(&name);
                return 3;
            }
        };
        let mut ok = true;
        let during = hidutil_get();
        ok &= report(
            "hidutil maps Caps Lock to F18 while on",
            during.contains("30064771129") && during.contains("30064771181"),
            during.lines().filter(|l| !l.contains("(null)")).count(),
        );
        sleep(Duration::from_millis(200));
        drain(&rx);

        // 1. tap: Escape, no modifiers
        let mut seq_ok = p.post(F18, true) && p.post(F18, false);
        let seen = drain(&rx);
        let keys: Vec<(u32, u16, u64)> = seen.iter().map(|s| (s.ty, s.keycode, s.mods)).collect();
        seq_ok &= report(
            "tap -> Escape only",
            keys == vec![(10, ESC, 0), (11, ESC, 0)],
            &keys,
        );
        ok &= seq_ok;

        // 2. hold + K: Ctrl+Option+Shift+Cmd+K, then modifiers released, no Escape
        if ok
            && p.post(F18, true)
            && p.post(F18, true)
            && p.post(K, true)
            && p.post(K, false)
            && p.post(F18, false)
        {
            let seen = drain(&rx);
            let k_down = seen.iter().find(|s| s.ty == 10 && s.keycode == K);
            let mods_down = seen.iter().filter(|s| s.ty == 12 && s.mods != 0).count();
            let last_flags = seen.iter().rev().find(|s| s.ty == 12).map(|s| s.mods);
            let pass = k_down.is_some_and(|s| s.mods == MODS)
                && mods_down >= 4
                && last_flags == Some(0)
                && !seen.iter().any(|s| s.keycode == ESC || s.keycode == F18);
            ok &= report("hold + K -> Ctrl+Opt+Shift+Cmd+K", pass, &seen);
        } else {
            ok = false;
        }

        // 3. slow tap: nothing
        if ok && p.post(F18, true) {
            sleep(Duration::from_millis(450));
            if p.post(F18, false) {
                let seen = drain(&rx);
                ok &= report("slow tap -> nothing", seen.is_empty(), &seen);
            }
        }

        // 4. tap action Caps Lock: toggles the lock, a second tap toggles back
        let caps = || unsafe { CGEventSourceFlagsState(1) & ALPHA_SHIFT != 0 };
        let caps_before = caps();
        let mut c2 = cfg.clone();
        c2.hyper_key.tap_action = TapAction::CapsLock;
        km.update(c2).unwrap();
        if ok && p.post(F18, true) && p.post(F18, false) {
            sleep(Duration::from_millis(200));
            let toggled = caps();
            if p.post(F18, true) && p.post(F18, false) {
                sleep(Duration::from_millis(200));
                let back = caps();
                ok &= report(
                    "tap action capsLock toggles and toggles back",
                    toggled != caps_before && back == caps_before,
                    (caps_before, toggled, back),
                );
            }
        }
        // make sure caps lock ends as it started
        if caps() != caps_before && p.post(F18, true) && p.post(F18, false) {
            sleep(Duration::from_millis(200));
        }

        // 5. mode off via update: mapping restored right away
        let mut c3 = cfg.clone();
        c3.hyper_key.mode = HyperMode::Off;
        km.update(c3).unwrap();
        ok &= report(
            "update to off restores hidutil",
            hidutil_get() == before,
            (),
        );
        km.update(cfg).unwrap();
        ok &= report("update back on re-applies", hidutil_get() != before, ());

        km.stop();
        let after = hidutil_get();
        ok &= report(
            "stop restores the exact previous UserKeyMapping",
            after == before,
            (),
        );
        close_doc(&name);
        if ok {
            0
        } else {
            1
        }
    }
}
