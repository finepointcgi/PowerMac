//! macOS live check of button swallowing: maps button 7 (the 8th button,
//! which nothing uses) to `Disabled`, posts a synthetic press/release at the
//! HID tap, and counts what reaches a listen-only session tap.
//!
//! ```text
//! cargo run -p mouse-tuning --example buttons_probe
//! ```

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use mouse_tuning::{ButtonAction, ButtonMapping, ButtonsConfig, Config, MouseTuning};

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
    #[repr(C)]
    struct P {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(t: u32, p: u32, o: u32, m: u64, cb: unsafe extern "C" fn(Ref, u32, Ref, *mut c_void) -> Ref, i: *mut c_void) -> Ref;
        fn CGEventGetIntegerValueField(e: Ref, f: u32) -> i64;
        fn CGEventCreate(s: Ref) -> Ref;
        fn CGEventGetLocation(e: Ref) -> P;
        fn CGEventCreateMouseEvent(s: Ref, ty: u32, p: P, b: u32) -> Ref;
        fn CGEventSetIntegerValueField(e: Ref, f: u32, v: i64);
        fn CGEventPost(t: u32, e: Ref);
    }
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn cb(_p: Ref, ty: u32, e: Ref, _i: *mut c_void) -> Ref {
        if (ty == 25 || ty == 26) && CGEventGetIntegerValueField(e, 3) == 7 {
            SEEN.fetch_add(1, Ordering::SeqCst);
        }
        e
    }
    struct S(Ref);
    unsafe impl Send for S {}
    let (tx, rx) = std::sync::mpsc::channel();
    let th = std::thread::spawn(move || unsafe {
        let tap = CGEventTapCreate(1, 1, 1, (1 << 25) | (1 << 26), cb, std::ptr::null_mut());
        assert!(!tap.is_null());
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
        tx.send(S(CFRunLoopGetCurrent())).unwrap();
        CFRunLoopRun();
    });
    let rl = rx.recv().unwrap();

    let cfg = Config {
        buttons: ButtonsConfig {
            side_buttons_navigation: false,
            mappings: vec![ButtonMapping { button: 7, action: ButtonAction::Disabled }],
        },
        ..Config::default()
    };
    let mut h = MouseTuning::start(cfg).expect("start");
    println!("button hook active: {}", h.buttons_active());
    std::thread::sleep(Duration::from_millis(200));
    unsafe {
        let here = CGEventCreate(std::ptr::null_mut());
        let pos = CGEventGetLocation(here);
        CFRelease(here as *const c_void);
        for ty in [25u32, 26] {
            let e = CGEventCreateMouseEvent(std::ptr::null_mut(), ty, P { x: pos.x, y: pos.y }, 7);
            CGEventSetIntegerValueField(e, 3, 7);
            CGEventPost(0, e);
            CFRelease(e as *const c_void);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("button-7 events delivered while mapped to Disabled: {} (expect 0)", SEEN.load(Ordering::SeqCst));
    h.update(Config::default());
    println!("after update to defaults, hook active: {} (side-button navigation)", h.buttons_active());
    h.stop();
    println!("after stop, hook active: {}", h.buttons_active());
    unsafe { CFRunLoopStop(rl.0) };
    let _ = th.join();
}
