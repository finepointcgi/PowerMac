//! macOS live check of smooth scrolling with synthetic events.
//!
//! Posts discrete line-based scroll events at the HID tap (as a wheel would)
//! and records, with a listen-only session-level tap, what applications
//! receive: with smoothing off one line event; with it on a glide of
//! continuous pixel events tagged `SELF_INJECTED_TAG`. Every scroll is undone
//! with one in the opposite direction, so the window under the cursor ends
//! where it started.
//!
//! ```text
//! cargo run -p scroll-reverser --example smooth_probe
//! ```

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use scroll_reverser::{Config, ScrollReverser, SELF_INJECTED_TAG};

    type Ref = *mut c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        fn CFRunLoopGetCurrent() -> Ref;
        fn CFRunLoopAddSource(rl: Ref, src: Ref, mode: *const c_void);
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: Ref);
        fn CFMachPortCreateRunLoopSource(a: *const c_void, port: Ref, order: isize) -> Ref;
        fn CFRelease(r: *const c_void);
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            mask: u64,
            cb: unsafe extern "C" fn(Ref, u32, Ref, *mut c_void) -> Ref,
            info: *mut c_void,
        ) -> Ref;
        fn CGEventGetIntegerValueField(e: Ref, f: u32) -> i64;
        fn CGEventCreateScrollWheelEvent2(s: Ref, units: u32, n: u32, w1: i32, w2: i32, w3: i32) -> Ref;
        fn CGEventPost(tap: u32, e: Ref);
    }

    #[derive(Clone, Copy, Debug)]
    struct Seen {
        t: Instant,
        tag: i64,
        cont: bool,
        line: i64,
        point: i64,
    }

    static mut LOG: Option<Arc<Mutex<Vec<Seen>>>> = None;

    unsafe extern "C" fn cb(_p: Ref, ty: u32, e: Ref, _i: *mut c_void) -> Ref {
        if ty == 22 {
            #[allow(static_mut_refs)]
            if let Some(l) = LOG.as_ref() {
                l.lock().unwrap().push(Seen {
                    t: Instant::now(),
                    tag: CGEventGetIntegerValueField(e, 42),
                    cont: CGEventGetIntegerValueField(e, 88) != 0,
                    line: CGEventGetIntegerValueField(e, 11),
                    point: CGEventGetIntegerValueField(e, 96),
                });
            }
        }
        e
    }

    struct SendRef(Ref);
    unsafe impl Send for SendRef {}

    fn post_line(lines: i32) {
        unsafe {
            let e = CGEventCreateScrollWheelEvent2(std::ptr::null_mut(), 1, 1, lines, 0, 0);
            CGEventPost(0, e);
            CFRelease(e as *const c_void);
        }
    }

    fn summarize(label: &str, log: &Arc<Mutex<Vec<Seen>>>, start: Instant) {
        let v = std::mem::take(&mut *log.lock().unwrap());
        let ours: Vec<_> = v.iter().filter(|s| s.tag == SELF_INJECTED_TAG as i64).collect();
        let other: Vec<_> = v.iter().filter(|s| s.tag != SELF_INJECTED_TAG as i64).collect();
        let sum: i64 = ours.iter().map(|s| s.point).sum();
        let span = match (ours.first(), ours.last()) {
            (Some(a), Some(b)) => b.t.duration_since(a.t).as_millis(),
            _ => 0,
        };
        println!(
            "{label}: {} untagged event(s) {:?}; {} smoothed event(s), point sum {sum}, all continuous={}, \
             first after {:?} ms, glide {span} ms",
            other.len(),
            other.iter().map(|s| (s.cont, s.line, s.point)).collect::<Vec<_>>(),
            ours.len(),
            ours.iter().all(|s| s.cont),
            ours.first().map(|s| s.t.duration_since(start).as_millis()),
        );
        if !ours.is_empty() {
            let deltas: Vec<i64> = ours.iter().map(|s| s.point).collect();
            println!("    deltas: {deltas:?}");
        }
    }

    pub fn main() {
        let log = Arc::new(Mutex::new(Vec::new()));
        unsafe { LOG = Some(log.clone()) };
        let (tx, rx) = std::sync::mpsc::channel();
        let th = std::thread::spawn(move || unsafe {
            // Listen-only session tap: sees what applications receive.
            let tap = CGEventTapCreate(1, 1, 1, 1 << 22, cb, std::ptr::null_mut());
            assert!(!tap.is_null(), "listen tap failed");
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
            tx.send(SendRef(CFRunLoopGetCurrent())).unwrap();
            CFRunLoopRun();
        });
        let rl = rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(200));

        // No reversal, so signs are easy to compare.
        let base = Config {
            reverse_mouse_vertical: false,
            smooth_scrolling: false,
            ..Config::default()
        };
        let mut h = ScrollReverser::start(base).expect("start");
        std::thread::sleep(Duration::from_millis(100));

        for lines in [1, -1] {
            log.lock().unwrap().clear();
            let t = Instant::now();
            post_line(lines);
            std::thread::sleep(Duration::from_millis(500));
            summarize(&format!("smoothing off, line {lines:+}"), &log, t);
        }

        h.update(Config { smooth_scrolling: true, ..base });
        for lines in [1, -1] {
            log.lock().unwrap().clear();
            let t = Instant::now();
            post_line(lines);
            std::thread::sleep(Duration::from_millis(600));
            summarize(&format!("smoothing on,  line {lines:+}"), &log, t);
        }

        // Fast spin: 6 notches 25 ms apart (acceleration), then undo it.
        for dir in [1, -1] {
            log.lock().unwrap().clear();
            let t = Instant::now();
            for _ in 0..6 {
                post_line(dir);
                std::thread::sleep(Duration::from_millis(25));
            }
            std::thread::sleep(Duration::from_millis(700));
            summarize(&format!("smoothing on,  fast spin {dir:+}x6"), &log, t);
        }

        // Reversal first, then smoothing: the glide must go the other way.
        h.update(Config { smooth_scrolling: true, reverse_mouse_vertical: true, ..base });
        for lines in [1, -1] {
            log.lock().unwrap().clear();
            let t = Instant::now();
            post_line(lines);
            std::thread::sleep(Duration::from_millis(600));
            summarize(&format!("reverse+smooth, line {lines:+}"), &log, t);
        }

        h.stop();
        eprintln!("reverser stopped");
        unsafe { CFRunLoopStop(rl.0) };
        let _ = th.join();
        eprintln!("listener stopped");
        println!("done");
    }
}
