//! macOS backend: a subscriber of the shared event tap (`pm-hook`) for
//! `kCGEventScrollWheel` at [`pm_hook::priority::SCROLL_REVERSER`] that
//! negates the scroll delta fields in place (no re-posting), and, with smooth
//! scrolling on, swallows discrete mouse-wheel notches and replays them as
//! eased pixel scroll events (tagged [`SELF_INJECTED_TAG`]) from a frame thread.
//!
//! CoreGraphics / CoreFoundation / ApplicationServices are bound directly.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
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
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;


#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFPreferencesAnyApplication: CFStringRef;

    fn CFRelease(cf: CFTypeRef);
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

struct TapCtx {
    shared: Arc<SharedConfig>,
    observer: Option<Observer>,
    classifier: RefCell<MacClassifier>,
    /// Smooth-scrolling frame thread (posts at the HID level).
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

/// The subscription; dropping it (or `stop`) unsubscribes.
pub(crate) struct Tap {
    guard: Option<pm_hook::Guard>,
}

impl Tap {
    pub(crate) fn stop(mut self) {
        self.guard.take();
    }
}

pub(crate) fn start(shared: Arc<SharedConfig>, observer: Option<Observer>) -> Result<Tap> {
    use pm_hook::{EventMask, Subscription, Verdict};
    if !is_trusted(false) {
        return Err(Error::PermissionDenied(
            "Accessibility is not granted to this process (or the terminal that launched it); \
             enable it in System Settings > Privacy & Security > Accessibility"
                .into(),
        )
        .into());
    }
    let ctx = TapCtx { shared, observer, classifier: RefCell::new(MacClassifier::new()), animator: RefCell::new(None) };
    // Smooth scrolling posts at the HID level, like the HID tap it replaces.
    match PixelPoster::new(kCGHIDEventTap) {
        Some(mut poster) => match Animator::start(Box::new(move |d| poster.post(d)), FRAME, noop, noop) {
            Ok(a) => *ctx.animator.borrow_mut() = Some(a),
            Err(e) => log::error!("scroll-reverser: smooth scrolling thread: {e}"),
        },
        None => log::error!("scroll-reverser: CGEventSourceCreate failed; no smooth scrolling"),
    }
    let guard = pm_hook::subscribe(
        Subscription::new("scroll-reverser", pm_hook::priority::SCROLL_REVERSER, EventMask::SCROLL, move |ev| {
            let event = ev.cg_event();
            if ev.cg_type() != kCGEventScrollWheel || event.is_null() {
                return Verdict::Pass;
            }
            match catch_unwind(AssertUnwindSafe(|| unsafe { handle_scroll(&ctx, event) })) {
                Ok(true) => Verdict::Swallow, // replayed by the smoother
                Ok(false) => Verdict::Pass,   // possibly reversed in place
                Err(_) => {
                    log::error!("scroll-reverser: panic in tap callback");
                    Verdict::Pass
                }
            }
        })
        .ignore_tags(&[PM_INPUT_INJECTED_TAG, SELF_INJECTED_TAG]),
    );
    let st = pm_hook::status();
    if !st.is_active() {
        drop(guard);
        let e = format!("CGEventTapCreate failed ({:?})", st.state);
        return Err(if is_trusted(false) { anyhow!(e) } else { Error::PermissionDenied(e).into() });
    }
    Ok(Tap { guard: Some(guard) })
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
