//! Live end-to-end check of the macOS event tap. Needs Accessibility
//! permission, so it is ignored by default:
//!
//! ```text
//! cargo test -p keyboard-manager --test live_macos -- --ignored --test-threads=1
//! ```
//!
//! It posts synthetic F13-F19 key events (keys that do nothing in normal
//! apps), lets Keyboard Manager remap them, and watches the result with a
//! listen-only tap placed after ours at the session level.
#![cfg(target_os = "macos")]
#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use keyboard_manager::{Config, KeyboardManager};

type CGEventRef = *mut c_void;
type CFMachPortRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, secs: f64, ret: u8) -> i32;
    fn CFMachPortCreateRunLoopSource(a: *const c_void, p: CFMachPortRef, o: isize) -> *mut c_void;
    fn CFMachPortInvalidate(p: CFMachPortRef);
    fn CFRelease(p: *const c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        cb: unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef,
        user: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, on: bool);
    fn CGEventGetIntegerValueField(e: CGEventRef, f: u32) -> i64;
    fn CGEventGetFlags(e: CGEventRef) -> u64;
    fn CGEventCreateKeyboardEvent(src: *mut c_void, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(e: CGEventRef, f: u64);
    fn CGEventPost(tap: u32, e: CGEventRef);
}

// mac keycodes
const F13: u16 = 0x69;
const F15: u16 = 0x71;
const F16: u16 = 0x6A;
const F17: u16 = 0x40;
const F18: u16 = 0x4F;
const F19: u16 = 0x50;
const SHIFT: u16 = 0x38;
const MARK: u16 = 0x5A; // F20: end-of-batch marker, not remapped

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    ty: u32,
    keycode: u16,
    shift: bool,
}

unsafe extern "C" fn listen_cb(
    _p: *mut c_void,
    ty: u32,
    e: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    if matches!(ty, 10..=12) {
        let tx = &*(user as *const mpsc::Sender<Seen>);
        let keycode = CGEventGetIntegerValueField(e, 9) as u16;
        let _ = tx.send(Seen {
            ty,
            keycode,
            shift: CGEventGetFlags(e) & 0x0002_0000 != 0,
        });
    }
    e
}

fn start_listener() -> mpsc::Receiver<Seen> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        let user = Box::into_raw(Box::new(tx)) as *mut c_void;
        // session tap, tail-append, listen-only: sees what comes out of ours
        let tap = CGEventTapCreate(1, 1, 1, (1 << 10) | (1 << 11) | (1 << 12), listen_cb, user);
        assert!(!tap.is_null(), "listen tap failed (permission?)");
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        ready_tx.send(()).unwrap();
        for _ in 0..40 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
        }
        CGEventTapEnable(tap, false);
        CFMachPortInvalidate(tap);
        CFRelease(src);
        CFRelease(tap);
    });
    ready_rx.recv().unwrap();
    rx
}

fn post(keycode: u16, down: bool) {
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
        CGEventSetFlags(e, 0x0080_0100); // fn + non-coalesced, like real F-keys
        CGEventPost(0, e);
        CFRelease(e);
    }
    std::thread::sleep(Duration::from_millis(15));
}

/// Collect events up to (not including) the F20 marker.
fn batch(rx: &mpsc::Receiver<Seen>) -> Vec<Seen> {
    post(MARK, true);
    post(MARK, false);
    let mut v = Vec::new();
    loop {
        let s = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("listener timed out");
        if s.keycode == MARK {
            let _ = rx.recv_timeout(Duration::from_secs(2));
            return v;
        }
        v.push(s);
    }
}

fn kd(k: u16) -> Seen {
    Seen {
        ty: 10,
        keycode: k,
        shift: false,
    }
}
fn ku(k: u16) -> Seen {
    Seen {
        ty: 11,
        keycode: k,
        shift: false,
    }
}

#[test]
#[ignore]
fn live_remaps_through_event_tap() {
    if !keyboard_manager::has_permission(false) {
        eprintln!("skipping: no Accessibility permission");
        return;
    }
    let cfg = Config::from_json(
        r#"{
          "remapKeys": [
            {"from": "F19", "to": "F18"},
            {"from": "F17", "to": "Shift+F16"},
            {"from": "F13", "to": "disabled"}
          ],
          "remapShortcuts": [ {"from": "Shift+F15", "to": "F16"} ],
          "pastePlainText": {"enabled": false}
        }"#,
    )
    .unwrap();
    let rx = start_listener();
    // Baseline without Keyboard Manager
    post(F19, true);
    post(F19, false);
    assert_eq!(batch(&rx), vec![kd(F19), ku(F19)]);

    let km = KeyboardManager::start(cfg).expect("start");

    // key -> key: rewritten in place
    post(F19, true);
    post(F19, false);
    assert_eq!(batch(&rx), vec![kd(F18), ku(F18)], "F19 -> F18");

    // key -> shortcut: swallowed, Shift+F16 posted
    post(F17, true);
    post(F17, false);
    let got = batch(&rx);
    assert_eq!(
        got,
        vec![
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: true
            },
            Seen {
                ty: 10,
                keycode: F16,
                shift: true
            },
            Seen {
                ty: 11,
                keycode: F16,
                shift: true
            },
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: false
            },
        ],
        "F17 -> Shift+F16"
    );

    // disabled
    post(F13, true);
    post(F13, false);
    assert_eq!(batch(&rx), vec![], "F13 disabled");

    // shortcut -> key: Shift (synthetic flagsChanged) + F15 -> F16 without Shift
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), SHIFT, true);
        CGEventSetFlags(e, 0x0002_0002);
        CGEventPost(0, e);
        CFRelease(e);
    }
    std::thread::sleep(Duration::from_millis(15));
    post(F15, true); // flags say no shift, but the engine knows Shift is held
    post(F15, false);
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), SHIFT, false);
        CGEventSetFlags(e, 0);
        CGEventPost(0, e);
        CFRelease(e);
    }
    let got = batch(&rx);
    assert_eq!(
        got,
        vec![
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: true
            },
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: false
            }, // suppressed for the remap
            kd(F16),
            ku(F16),
        ],
        "Shift+F15 -> F16 (shift released for the target, its physical release swallowed)"
    );

    // Hot update: F19 back to normal
    km.update(Config {
        paste_plain_text: Default::default(),
        ..Config::default()
    })
    .unwrap();
    post(F19, true);
    post(F19, false);
    assert_eq!(batch(&rx), vec![kd(F19), ku(F19)], "after update");

    // Stop while a remapped key is held: injected keys are released.
    km.update(
        Config::from_json(
            r#"{"remapKeys":[{"from":"F17","to":"Shift+F16"}],"pastePlainText":{"enabled":false}}"#,
        )
        .unwrap(),
    )
    .unwrap();
    post(F17, true);
    std::thread::sleep(Duration::from_millis(50));
    km.stop();
    let got = batch(&rx);
    assert_eq!(
        got,
        vec![
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: true
            },
            Seen {
                ty: 10,
                keycode: F16,
                shift: true
            },
            Seen {
                ty: 11,
                keycode: F16,
                shift: true
            },
            Seen {
                ty: 12,
                keycode: SHIFT,
                shift: false
            },
        ],
        "stop releases Shift+F16"
    );
    post(F17, false); // the physical release now passes untouched
    assert_eq!(batch(&rx), vec![ku(F17)]);
}

/// Like `post`, but a flagsChanged event for a modifier with explicit flags.
fn post_mod(keycode: u16, flags: u64) {
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, flags != 0);
        CGEventSetFlags(e, flags);
        CGEventSetType(e, 12);
        CGEventPost(0, e);
        CFRelease(e);
    }
    std::thread::sleep(Duration::from_millis(15));
}

fn post_flags(keycode: u16, down: bool, flags: u64) {
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
        CGEventSetFlags(e, flags);
        CGEventPost(0, e);
        CFRelease(e);
    }
    std::thread::sleep(Duration::from_millis(15));
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSetType(e: CGEventRef, ty: u32);
}

const CTRL: u64 = 0x0004_0000;
const CMD: u64 = 0x0010_0000;
const RCTL_DEV: u64 = 0x2000;
const LCTL_DEV: u64 = 0x0001;
const RIGHT_CONTROL: u16 = 0x3E;
const RIGHT_COMMAND: u16 = 0x36;
const CONTROL: u16 = 0x3B;
const COMMAND: u16 = 0x37;

/// Events with their Control/Command flags.
fn batch_mods(rx: &mpsc::Receiver<(u32, u16, u64)>) -> Vec<(u32, u16, u64)> {
    post(MARK, true);
    post(MARK, false);
    let mut v = Vec::new();
    loop {
        let s = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("listener timed out");
        if s.1 == MARK {
            let _ = rx.recv_timeout(Duration::from_secs(2));
            return v;
        }
        v.push(s);
    }
}

unsafe extern "C" fn listen_mods_cb(
    _p: *mut c_void,
    ty: u32,
    e: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    if matches!(ty, 10..=12) {
        let tx = &*(user as *const mpsc::Sender<(u32, u16, u64)>);
        let _ = tx.send((
            ty,
            CGEventGetIntegerValueField(e, 9) as u16,
            CGEventGetFlags(e) & (CTRL | CMD),
        ));
    }
    e
}

fn start_mods_listener() -> mpsc::Receiver<(u32, u16, u64)> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        let user = Box::into_raw(Box::new(tx)) as *mut c_void;
        let tap = CGEventTapCreate(
            1,
            1,
            1,
            (1 << 10) | (1 << 11) | (1 << 12),
            listen_mods_cb,
            user,
        );
        assert!(!tap.is_null());
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        ready_tx.send(()).unwrap();
        for _ in 0..40 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
        }
        CGEventTapEnable(tap, false);
        CFMachPortInvalidate(tap);
        CFRelease(src);
        CFRelease(tap);
    });
    ready_rx.recv().unwrap();
    rx
}

#[test]
#[ignore]
fn live_modifier_remaps() {
    if !keyboard_manager::has_permission(false) {
        eprintln!("skipping: no Accessibility permission");
        return;
    }
    let cfg = Config::from_json(
        r#"{
          "remapKeys": [ {"from": "RCtrl", "to": "RCmd"} ],
          "remapShortcuts": [ {"from": "LCtrl+F18", "to": "Cmd+F19"} ],
          "pastePlainText": {"enabled": false}
        }"#,
    )
    .unwrap();
    let rx = start_mods_listener();
    let km = KeyboardManager::start(cfg).expect("start");

    // Modifier -> modifier: the flagsChanged is rewritten in place and the
    // key in between carries Command instead of Control.
    post_mod(RIGHT_CONTROL, CTRL | RCTL_DEV);
    post_flags(F18, true, CTRL | RCTL_DEV);
    post_flags(F18, false, CTRL | RCTL_DEV);
    post_mod(RIGHT_CONTROL, 0);
    assert_eq!(
        batch_mods(&rx),
        vec![
            (12, RIGHT_COMMAND, CMD),
            (10, F18, CMD),
            (11, F18, CMD),
            (12, RIGHT_COMMAND, 0)
        ],
        "RCtrl -> RCmd"
    );

    // Shortcut -> shortcut: Ctrl suppressed, Cmd pressed, F19 with Cmd only;
    // releasing Ctrl releases Cmd.
    post_mod(CONTROL, CTRL | LCTL_DEV);
    post_flags(F18, true, CTRL | LCTL_DEV);
    post_flags(F18, false, CTRL | LCTL_DEV);
    post_flags(F18, true, CTRL | LCTL_DEV); // again while Ctrl held
    post_flags(F18, false, CTRL | LCTL_DEV);
    post_mod(CONTROL, 0);
    assert_eq!(
        batch_mods(&rx),
        vec![
            (12, CONTROL, CTRL),
            (12, CONTROL, 0),
            (12, COMMAND, CMD),
            (10, F19, CMD),
            (11, F19, CMD),
            (10, F19, CMD),
            (11, F19, CMD),
            (12, COMMAND, 0),
        ],
        "LCtrl+F18 -> Cmd+F19"
    );
    km.stop();
}
