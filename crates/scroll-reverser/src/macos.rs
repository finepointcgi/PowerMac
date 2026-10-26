//! macOS backend: an active `CGEventTap` for `kCGEventScrollWheel` that
//! negates the scroll delta fields in place (no re-posting), and, with smooth
//! scrolling on, swallows discrete mouse-wheel notches and replays them as
//! eased pixel scroll events (tagged [`SELF_INJECTED_TAG`]) from a frame thread.
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

use crate::smooth::Animator;
use crate::{
    mac_notch_direction, Error, MacClassifier, MacDeltas, MacScrollFields, Observer,
    ScrollEventInfo, SharedConfig, PM_INPUT_INJECTED_TAG, SELF_INJECTED_TAG,
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
type CGEventSourceRef = *mut c_void;

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
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventSourceSetUserData(source: CGEventSourceRef, data: i64);
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, secs: f64);
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
const kCGScrollEventUnitPixel: u32 = 0;
const kCGEventSourceStateHIDSystemState: i32 = 1;
const kCGEventFlagMaskShift: u64 = 0x0002_0000;
const kCGEventFlagMaskControl: u64 = 0x0004_0000;
const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
const kCGEventFlagMaskCommand: u64 = 0x0010_0000;
const MODIFIER_MASK: u64 =
    kCGEventFlagMaskShift | kCGEventFlagMaskControl | kCGEventFlagMaskAlternate | kCGEventFlagMaskCommand;
/// Smooth-scrolling frame interval (120 Hz).
const FRAME: Duration = Duration::from_micros(8_333);
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
    /// Smooth-scrolling frame thread (posts at the tap's own location).
    animator: RefCell<Option<Animator>>,
}

/// An event source tagged with [`SELF_INJECTED_TAG`], owned by the frame thread.
struct PixelPoster {
    source: CGEventSourceRef,
    location: u32,
}
// SAFETY: the source is only used from the frame thread that owns the poster;
// CGEventSource/CGEventPost are thread-safe.
unsafe impl Send for PixelPoster {}

impl PixelPoster {
    fn new(location: u32) -> Option<Self> {
        unsafe {
            let source = CGEventSourceCreate(kCGEventSourceStateHIDSystemState);
            if source.is_null() {
                return None;
            }
            CGEventSourceSetUserData(source, SELF_INJECTED_TAG as i64);
            // Otherwise our posts would suppress real mouse/keyboard input for 0.25 s.
            CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0);
            Some(Self { source, location })
        }
    }

    fn post(&mut self, d: [i64; 2]) {
        let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        unsafe {
            let e = CGEventCreateScrollWheelEvent2(
                self.source,
                kCGScrollEventUnitPixel,
                2,
                clamp(d[0]),
                clamp(d[1]),
                0,
            );
            if e.is_null() {
                return;
            }
            CGEventPost(self.location, e);
            CFRelease(e as CFTypeRef);
        }
    }
}

impl Drop for PixelPoster {
    fn drop(&mut self) {
        unsafe { CFRelease(self.source as CFTypeRef) };
    }
}

fn noop() {}

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
        // Only once: after the join the run loop belongs to a dead thread, and
        // CFRunLoopStop on it traps (stop() followed by Drop used to do that).
        if let Some(t) = self.thread.take() {
            self.stop.store(true, Ordering::SeqCst);
            unsafe { CFRunLoopStop(self.run_loop.0) };
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
                animator: RefCell::new(None),
            }));
            let mut tap = std::ptr::null_mut();
            let mut location = kCGHIDEventTap;
            for loc in [kCGHIDEventTap, kCGSessionEventTap] {
                location = loc;
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
            if location != kCGHIDEventTap {
                log::warn!("scroll-reverser: HID event tap unavailable, using the session tap");
            }
            match PixelPoster::new(location) {
                Some(mut poster) => {
                    match Animator::start(Box::new(move |d| poster.post(d)), FRAME, noop, noop) {
                        Ok(a) => *(*ctx).animator.borrow_mut() = Some(a),
                        Err(e) => log::error!("scroll-reverser: smooth scrolling thread: {e}"),
                    }
                }
                None => log::error!("scroll-reverser: CGEventSourceCreate failed; no smooth scrolling"),
            }
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
    match catch_unwind(AssertUnwindSafe(|| handle_scroll(ctx, event))) {
        Ok(true) => std::ptr::null_mut(), // swallowed; replayed by the smoother
        Ok(false) => event,
        Err(_) => {
            log::error!("scroll-reverser: panic in tap callback");
            event
        }
    }
}

/// Returns true when the event is swallowed (smooth scrolling replays it).
unsafe fn handle_scroll(ctx: &TapCtx, ev: CGEventRef) -> bool {
    let tag = CGEventGetIntegerValueField(ev, kCGEventSourceUserData);
    if tag == PM_INPUT_INJECTED_TAG as i64 || tag == SELF_INJECTED_TAG as i64 {
        return false;
    }
    let fields = MacScrollFields {
        is_continuous: CGEventGetIntegerValueField(ev, kCGScrollWheelEventIsContinuous) != 0,
        scroll_phase: CGEventGetIntegerValueField(ev, kCGScrollWheelEventScrollPhase),
        momentum_phase: CGEventGetIntegerValueField(ev, kCGScrollWheelEventMomentumPhase),
    };
    let device = ctx.classifier.borrow_mut().classify(fields, CGEventGetTimestamp(ev));
    let cfg = ctx.shared.load();
    let rev = cfg.reverse_for(device);
    // Only discrete (line-based) wheel notches are smoothed; continuous
    // "mouse" events come from smooth-scrolling drivers and are smooth already.
    let mods = CGEventGetFlags(ev) & MODIFIER_MASK != 0;
    let smooth_v = !fields.is_continuous && cfg.smooth_for(device, false, mods);
    let smooth_h = !fields.is_continuous && cfg.smooth_for(device, true, mods);
    if !rev.any() && !smooth_v && !smooth_h && ctx.observer.is_none() {
        return false;
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

    let mut smoothed = false;
    if smooth_v || smooth_h {
        let n = d.reversed(rev);
        let dir = [
            if smooth_v { mac_notch_direction(n.delta[0], n.fixed[0]) } else { 0.0 },
            if smooth_h { mac_notch_direction(n.delta[1], n.fixed[1]) } else { 0.0 },
        ];
        // An axis that is not smoothed but moves must not be lost: then pass
        // the whole event through (reversed) instead.
        let unsmoothed_motion = (!smooth_v && (n.delta[0] != 0 || n.fixed[0] != 0.0))
            || (!smooth_h && (n.delta[1] != 0 || n.fixed[1] != 0.0));
        if !unsmoothed_motion && dir != [0.0, 0.0] {
            if let Some(anim) = ctx.animator.borrow().as_ref() {
                anim.push(dir, cfg.smooth_params(1.0));
                smoothed = true;
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
            smoothed,
        });
    }
    smoothed
}
