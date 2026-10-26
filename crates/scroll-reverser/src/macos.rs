//! macOS backend: an active `CGEventTap` for `kCGEventScrollWheel` that
//! negates the scroll delta fields in place (no re-posting).
//!
//! CoreGraphics / CoreFoundation / ApplicationServices are bound directly.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::{
    Error, MacClassifier, MacDeltas, MacScrollFields, Observer, ScrollEventInfo, SharedConfig,
    PM_INPUT_INJECTED_TAG,
};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;

type CGEventTapCallBack =
    unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFPreferencesAnyApplication: CFStringRef;

    fn CFRelease(cf: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFMachPortCreateRunLoopSource(
        alloc: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num: isize,
        key_cb: *const c_void,
        value_cb: *const c_void,
    ) -> CFDictionaryRef;
    fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const c_char, encoding: u32)
        -> CFStringRef;
    fn CFPreferencesAppSynchronize(app: CFStringRef) -> u8;
    fn CFPreferencesGetAppBooleanValue(key: CFStringRef, app: CFStringRef, valid: *mut u8) -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
}

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

const kCGHIDEventTap: u32 = 0;
const kCGSessionEventTap: u32 = 1;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;

const kCGEventScrollWheel: u32 = 22;
const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

const kCGScrollWheelEventDeltaAxis1: u32 = 11;
const kCGScrollWheelEventDeltaAxis2: u32 = 12;
const kCGEventSourceUserData: u32 = 42;
const kCGScrollWheelEventIsContinuous: u32 = 88;
const kCGScrollWheelEventFixedPtDeltaAxis1: u32 = 93;
const kCGScrollWheelEventFixedPtDeltaAxis2: u32 = 94;
const kCGScrollWheelEventPointDeltaAxis1: u32 = 96;
const kCGScrollWheelEventPointDeltaAxis2: u32 = 97;
const kCGScrollWheelEventScrollPhase: u32 = 99;
const kCGScrollWheelEventMomentumPhase: u32 = 123;

const DELTA: [u32; 2] = [kCGScrollWheelEventDeltaAxis1, kCGScrollWheelEventDeltaAxis2];
const POINT: [u32; 2] = [kCGScrollWheelEventPointDeltaAxis1, kCGScrollWheelEventPointDeltaAxis2];
const FIXED: [u32; 2] = [kCGScrollWheelEventFixedPtDeltaAxis1, kCGScrollWheelEventFixedPtDeltaAxis2];

/// Accessibility trust (required for an active event tap).
pub(crate) fn is_trusted(prompt: bool) -> bool {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let val = if prompt { kCFBooleanTrue } else { kCFBooleanFalse };
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &val,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks as *const c_void,
            &raw const kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        let ok = AXIsProcessTrustedWithOptions(dict) != 0;
        if !dict.is_null() {
            CFRelease(dict);
        }
        ok
    }
}

/// `com.apple.swipescrolldirection` from the global domain; true when unset.
pub(crate) fn natural_scrolling() -> bool {
    unsafe {
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"com.apple.swipescrolldirection".as_ptr(),
            kCFStringEncodingUTF8,
        );
        if key.is_null() {
            return true;
        }
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
        let mut valid = 0u8;
        let v = CFPreferencesGetAppBooleanValue(key, kCFPreferencesAnyApplication, &mut valid);
        CFRelease(key);
        if valid != 0 {
            v != 0
        } else {
            true
        }
    }
}

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
// SAFETY: only used to hand the run loop to CFRunLoopStop, which is thread-safe.
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

struct TapCtx {
    shared: Arc<SharedConfig>,
    observer: Option<Observer>,
    classifier: RefCell<MacClassifier>,
    tap: Cell<CFMachPortRef>,
}

pub(crate) struct Tap {
    stop: Arc<AtomicBool>,
    run_loop: SendPtr,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn start(shared: Arc<SharedConfig>, observer: Option<Observer>) -> Result<Tap> {
    if !is_trusted(false) {
        return Err(Error::PermissionDenied(
            "Accessibility is not granted to this process (or the terminal that launched it); \
             enable it in System Settings > Privacy & Security > Accessibility"
                .into(),
        )
        .into());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let (tx, rx) = mpsc::channel::<std::result::Result<SendPtr, String>>();

    let thread = std::thread::Builder::new()
        .name("scroll-reverser-tap".into())
        .spawn(move || unsafe {
            let ctx = Box::into_raw(Box::new(TapCtx {
                shared,
                observer,
                classifier: RefCell::new(MacClassifier::new()),
                tap: Cell::new(std::ptr::null_mut()),
            }));
            let mut tap = std::ptr::null_mut();
            for loc in [kCGHIDEventTap, kCGSessionEventTap] {
                tap = CGEventTapCreate(
                    loc,
                    kCGHeadInsertEventTap,
                    kCGEventTapOptionDefault,
                    1u64 << kCGEventScrollWheel,
                    tap_callback,
                    ctx as *mut c_void,
                );
                if !tap.is_null() {
                    break;
                }
            }
            if tap.is_null() {
                drop(Box::from_raw(ctx));
                let _ = tx.send(Err("CGEventTapCreate failed".into()));
                return;
            }
            (*ctx).tap.set(tap);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            let _ = tx.send(Ok(SendPtr(rl)));

            while !stop2.load(Ordering::SeqCst) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, 0);
            }

            CGEventTapEnable(tap, false);
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(tap);
            CFRelease(src as CFTypeRef);
            CFRelease(tap as CFTypeRef);
            drop(Box::from_raw(ctx));
        })?;

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(rl)) => Ok(Tap { stop, run_loop: rl, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            if is_trusted(false) {
                Err(anyhow!(e))
            } else {
                Err(Error::PermissionDenied(e).into())
            }
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err(anyhow!("event tap thread did not start"))
        }
    }
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    ty: u32,
    event: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    let ctx = &*(user as *const TapCtx);
    if ty == kCGEventTapDisabledByTimeout || ty == kCGEventTapDisabledByUserInput {
        let tap = ctx.tap.get();
        if !tap.is_null() {
            log::warn!("scroll-reverser: event tap disabled ({ty:#x}); re-enabling");
            CGEventTapEnable(tap, true);
        }
        return event;
    }
    if ty != kCGEventScrollWheel || event.is_null() {
        return event;
    }
    if catch_unwind(AssertUnwindSafe(|| handle_scroll(ctx, event))).is_err() {
        log::error!("scroll-reverser: panic in tap callback");
    }
    event
}

unsafe fn handle_scroll(ctx: &TapCtx, ev: CGEventRef) {
    if CGEventGetIntegerValueField(ev, kCGEventSourceUserData) == PM_INPUT_INJECTED_TAG as i64 {
        return;
    }
    let fields = MacScrollFields {
        is_continuous: CGEventGetIntegerValueField(ev, kCGScrollWheelEventIsContinuous) != 0,
        scroll_phase: CGEventGetIntegerValueField(ev, kCGScrollWheelEventScrollPhase),
        momentum_phase: CGEventGetIntegerValueField(ev, kCGScrollWheelEventMomentumPhase),
    };
    let device = ctx.classifier.borrow_mut().classify(fields, CGEventGetTimestamp(ev));
    let rev = ctx.shared.load().reverse_for(device);
    if !rev.any() && ctx.observer.is_none() {
        return;
    }

    let mut d = MacDeltas::default();
    for axis in 0..2 {
        d.delta[axis] = CGEventGetIntegerValueField(ev, DELTA[axis]);
        d.point[axis] = CGEventGetIntegerValueField(ev, POINT[axis]);
        d.fixed[axis] = CGEventGetDoubleValueField(ev, FIXED[axis]);
    }

    if rev.any() {
        let n = d.reversed(rev);
        for (axis, on) in [(0, rev.vertical), (1, rev.horizontal)] {
            if on {
                // Same order as Scroll Reverser: line delta, point delta, fixed-point.
                CGEventSetIntegerValueField(ev, DELTA[axis], n.delta[axis]);
                CGEventSetIntegerValueField(ev, POINT[axis], n.point[axis]);
                CGEventSetDoubleValueField(ev, FIXED[axis], n.fixed[axis]);
            }
        }
    }

    if let Some(obs) = &ctx.observer {
        let pick = |axis: usize| {
            if d.point[axis] != 0 {
                d.point[axis] as f64
            } else {
                d.fixed[axis]
            }
        };
        obs(&ScrollEventInfo {
            device,
            vertical: pick(0),
            horizontal: pick(1),
            reversed: rev,
            continuous: fields.is_continuous,
            scroll_phase: fields.scroll_phase,
            momentum_phase: fields.momentum_phase,
        });
    }
}
