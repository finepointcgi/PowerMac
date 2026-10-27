//! macOS backend: one `CGEventTap` on the `pm-hook` thread.
//!
//! The thread is started by the first subscriber and stays (idle, without a
//! tap) when the last one leaves. Each time the subscriber set changes it
//! (re)creates the tap with the union of their masks:
//!
//! 1. active at `kCGHIDEventTap`, then at `kCGSessionEventTap` (needs
//!    Accessibility);
//! 2. otherwise listen-only at `kCGSessionEventTap` — keys too when Input
//!    Monitoring is granted, mouse only otherwise — and the active tap is
//!    retried every 2 s;
//! 3. otherwise none, retried every 2 s.
//!
//! The run loop runs in 0.5 s slices; between them a watchdog re-enables a
//! silently disabled tap and polls Secure Event Input. Subscriber changes
//! wake the run loop through a custom run-loop source.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::hub::{mark_dispatch_thread, Backend, Hub, Out};
use crate::{Event, EventMask, EventType, Raw, Synthetic, TapState};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
pub(crate) type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventTapCallBack = unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
struct CFRunLoopSourceContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<extern "C" fn(*const c_void) -> usize>,
    schedule: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    cancel: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    perform: Option<extern "C" fn(*mut c_void)>,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    fn CFRunLoopSourceCreate(alloc: *const c_void, order: isize, ctx: *mut CFRunLoopSourceContext) -> CFRunLoopSourceRef;
    fn CFRunLoopSourceSignal(src: CFRunLoopSourceRef);
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
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
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    fn CGEventTapPostEvent(proxy: CGEventTapProxy, event: CGEventRef);
    fn CGEventGetType(event: CGEventRef) -> u32;
    pub(crate) fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub(crate) fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateCopy(event: CGEventRef) -> CGEventRef;
    fn CGPreflightListenEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "objc")]
extern "C" {
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

const kCGHIDEventTap: u32 = 0;
const kCGSessionEventTap: u32 = 1;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;
const kCGEventTapOptionListenOnly: u32 = 1;
const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;
const kCGEventSourceUserData: u32 = 42;

const SLICE: f64 = 0.5;
const RETRY: Duration = Duration::from_secs(2);

pub(crate) fn secure_input() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// `CGEventType` of an [`EventType`].
pub(crate) fn cg_type(t: EventType) -> u32 {
    match t {
        EventType::LeftMouseDown => 1,
        EventType::LeftMouseUp => 2,
        EventType::RightMouseDown => 3,
        EventType::RightMouseUp => 4,
        EventType::MouseMoved => 5,
        EventType::LeftMouseDragged => 6,
        EventType::RightMouseDragged => 7,
        EventType::KeyDown => 10,
        EventType::KeyUp => 11,
        EventType::FlagsChanged => 12,
        EventType::SystemDefined => 14,
        EventType::ScrollWheel => 22,
        EventType::OtherMouseDown => 25,
        EventType::OtherMouseUp => 26,
        EventType::OtherMouseDragged => 27,
    }
}

pub(crate) fn kind_of(ty: u32) -> Option<EventType> {
    EventType::ALL.into_iter().find(|t| cg_type(*t) == ty)
}

pub(crate) fn cg_mask(m: EventMask) -> u64 {
    m.types().fold(0u64, |acc, t| acc | 1u64 << cg_type(t))
}

/// A +1 retained `CGEventRef`.
pub struct OwnedEvent(CGEventRef);

impl OwnedEvent {
    pub(crate) unsafe fn from_raw(ev: CGEventRef) -> Option<Self> {
        (!ev.is_null()).then_some(OwnedEvent(ev))
    }
    pub fn as_ptr(&self) -> CGEventRef {
        self.0
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

pub(crate) fn event_view<'a>(ev: CGEventRef, synthetic: bool) -> Option<Event<'a>> {
    if ev.is_null() {
        return None;
    }
    let ty = unsafe { CGEventGetType(ev) };
    let kind = kind_of(ty)?;
    let tag = unsafe { CGEventGetIntegerValueField(ev, kCGEventSourceUserData) } as usize;
    Some(Event::new(kind, tag, synthetic, Raw::Mac { ev, ty, _l: std::marker::PhantomData }))
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Ctl {
    started: bool,
    wanted: u64,
    applied: u64,
    run_loop: usize,
    wake: usize,
    /// The current tap's port while it is an active tap (for `is_healthy`).
    port: usize,
}

struct Shared {
    ctl: Mutex<Ctl>,
    cv: Condvar,
}

fn lock(s: &Shared) -> MutexGuard<'_, Ctl> {
    s.ctl.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct MacBackend {
    shared: Arc<Shared>,
}

impl MacBackend {
    pub(crate) fn new() -> Self {
        MacBackend { shared: Arc::new(Shared { ctl: Mutex::new(Ctl::default()), cv: Condvar::new() }) }
    }
}

impl Backend for MacBackend {
    fn reconcile(&self, hub: &Arc<Hub>, wait: bool) {
        let (gen, start) = {
            let mut c = lock(&self.shared);
            c.wanted += 1;
            let start = !c.started;
            c.started = true;
            if c.wake != 0 {
                unsafe {
                    CFRunLoopSourceSignal(c.wake as CFRunLoopSourceRef);
                    CFRunLoopWakeUp(c.run_loop as CFRunLoopRef);
                }
            }
            (c.wanted, start)
        };
        // No status updates under the lock: status callbacks may call
        // `is_healthy`, which takes it.
        if start {
            hub.update_status(|s| s.state = TapState::Starting);
            let (h, s) = (hub.clone(), self.shared.clone());
            if let Err(e) = std::thread::Builder::new().name("pm-hook".into()).spawn(move || thread_main(h, s)) {
                lock(&self.shared).started = false;
                hub.update_status(|s| s.state = TapState::Unavailable(format!("could not start the tap thread: {e}")));
                return;
            }
        }
        self.shared.cv.notify_all();
        if wait {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut c = lock(&self.shared);
            while c.applied < gen {
                let now = Instant::now();
                if now >= deadline {
                    log::warn!("pm-hook: the tap thread did not apply a subscriber change within 5 s");
                    break;
                }
                c = self.shared.cv.wait_timeout(c, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }

    fn is_healthy(&self, hub: &Hub) -> bool {
        let c = lock(&self.shared);
        if c.port == 0 {
            return false;
        }
        let port = c.port as CFMachPortRef;
        unsafe {
            if !CGEventTapIsEnabled(port) {
                // Disabled without a "disabled" event reaching us: try once.
                CGEventTapEnable(port, true);
            }
            CGEventTapIsEnabled(port) && hub.status().is_active()
        }
    }

    fn recurse_synthetics(&self) -> bool {
        true
    }

    fn debug_disable(&self) -> bool {
        let c = lock(&self.shared);
        if c.port == 0 {
            return false;
        }
        unsafe { CGEventTapEnable(c.port as CFMachPortRef, false) };
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Active,
    Listen { keyboard: bool },
}

struct Tap {
    port: CFMachPortRef,
    src: CFRunLoopSourceRef,
    /// The CG mask the tap was created with (requested, before filtering).
    want: u64,
    mode: Mode,
}

struct TapCtx {
    hub: Arc<Hub>,
    port: Cell<CFMachPortRef>,
    listen_only: Cell<bool>,
}

extern "C" fn wake_perform(_info: *mut c_void) {
    unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
}

fn thread_main(hub: Arc<Hub>, shared: Arc<Shared>) {
    mark_dispatch_thread(true);
    unsafe {
        let rl = CFRunLoopGetCurrent();
        let mut sctx = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(wake_perform),
        };
        let wake = CFRunLoopSourceCreate(std::ptr::null(), 0, &mut sctx);
        if !wake.is_null() {
            CFRunLoopAddSource(rl, wake, kCFRunLoopCommonModes);
        }
        {
            let mut c = lock(&shared);
            c.run_loop = rl as usize;
            c.wake = wake as usize;
        }
        let ctx: &'static TapCtx =
            Box::leak(Box::new(TapCtx { hub: hub.clone(), port: Cell::new(std::ptr::null_mut()), listen_only: Cell::new(false) }));
        let mut tap: Option<Tap> = None;
        let mut next_retry = Instant::now();
        let mut warned = false;
        loop {
            let gen = lock(&shared).wanted;
            let want = cg_mask(hub.union_mask());
            if want == 0 {
                if let Some(t) = tap.take() {
                    destroy(&shared, ctx, rl, t);
                }
                hub.update_status(|s| s.state = TapState::Idle);
                warned = false;
            } else {
                let mask_changed = tap.as_ref().is_none_or(|t| t.want != want);
                let retry_due = tap.as_ref().is_none_or(|t| t.mode != Mode::Active) && Instant::now() >= next_retry;
                if mask_changed || retry_due {
                    // A new mask replaces the tap (never two taps at once, so
                    // no event is dispatched twice); a retry from listen-only
                    // keeps the old one until the active tap exists.
                    if mask_changed {
                        if let Some(t) = tap.take() {
                            destroy(&shared, ctx, rl, t);
                        }
                    }
                    let keep_listen = !mask_changed && tap.is_some();
                    match create_active(want, ctx) {
                        Some(port) => {
                            if let Some(t) = tap.take() {
                                destroy(&shared, ctx, rl, t);
                            }
                            tap = Some(install(&shared, ctx, rl, port, want, Mode::Active));
                            hub.update_status(|s| s.state = TapState::Active);
                            if warned {
                                log::info!("pm-hook: event tap active");
                            }
                            warned = false;
                        }
                        None if keep_listen => next_retry = Instant::now() + RETRY,
                        None => {
                            if let Some(t) = tap.take() {
                                destroy(&shared, ctx, rl, t);
                            }
                            next_retry = Instant::now() + RETRY;
                            match create_listen(want, ctx) {
                                Some((port, keyboard)) => {
                                    tap = Some(install(&shared, ctx, rl, port, want, Mode::Listen { keyboard }));
                                    hub.update_status(|s| s.state = TapState::ListenOnly { keyboard });
                                }
                                None => hub.update_status(|s| {
                                    s.state = TapState::Unavailable(
                                        "could not create the event tap: this process (or the terminal running it) \
                                         is not trusted for Accessibility (System Settings > Privacy & Security > \
                                         Accessibility)"
                                            .into(),
                                    )
                                }),
                            }
                            if !warned {
                                log::warn!("pm-hook: no active event tap (Accessibility permission missing?); retrying every 2 s");
                                warned = true;
                            }
                        }
                    }
                }
            }
            {
                let mut c = lock(&shared);
                c.applied = c.applied.max(gen);
            }
            shared.cv.notify_all();

            if tap.is_some() {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, SLICE, 0);
            } else {
                let mut c = lock(&shared);
                if c.wanted == gen {
                    let wait = if want == 0 { Duration::from_secs(30) } else { next_retry.saturating_duration_since(Instant::now()).max(Duration::from_millis(50)) };
                    c = shared.cv.wait_timeout(c, wait).unwrap_or_else(|e| e.into_inner()).0;
                }
                drop(c);
            }
            watchdog(&hub, tap.as_ref());
        }
    }
}

unsafe fn create_active(want: u64, ctx: &TapCtx) -> Option<CFMachPortRef> {
    if AXIsProcessTrusted() == 0 {
        return None;
    }
    for loc in [kCGHIDEventTap, kCGSessionEventTap] {
        let port = CGEventTapCreate(loc, kCGHeadInsertEventTap, kCGEventTapOptionDefault, want, tap_cb, ctx as *const TapCtx as *mut c_void);
        if !port.is_null() {
            if loc != kCGHIDEventTap {
                log::warn!("pm-hook: HID event tap unavailable; using the session tap");
            }
            return Some(port);
        }
    }
    None
}

unsafe fn create_listen(want: u64, ctx: &TapCtx) -> Option<(CFMachPortRef, bool)> {
    let keys = cg_mask(EventMask::KEYS.with(EventType::SystemDefined));
    let keyboard = CGPreflightListenEventAccess();
    let mask = if keyboard { want } else { want & !keys };
    if mask == 0 {
        return None;
    }
    let port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionListenOnly, mask, tap_cb, ctx as *const TapCtx as *mut c_void);
    (!port.is_null()).then_some((port, keyboard))
}

unsafe fn install(shared: &Shared, ctx: &TapCtx, rl: CFRunLoopRef, port: CFMachPortRef, want: u64, mode: Mode) -> Tap {
    let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
    CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
    CGEventTapEnable(port, true);
    ctx.port.set(port);
    ctx.listen_only.set(mode != Mode::Active);
    lock(shared).port = if mode == Mode::Active { port as usize } else { 0 };
    Tap { port, src, want, mode }
}

unsafe fn destroy(shared: &Shared, ctx: &TapCtx, rl: CFRunLoopRef, t: Tap) {
    lock(shared).port = 0;
    ctx.port.set(std::ptr::null_mut());
    CGEventTapEnable(t.port, false);
    CFRunLoopRemoveSource(rl, t.src, kCFRunLoopCommonModes);
    CFMachPortInvalidate(t.port);
    CFRelease(t.src as CFTypeRef);
    CFRelease(t.port as CFTypeRef);
}

fn watchdog(hub: &Hub, tap: Option<&Tap>) {
    let secure = secure_input();
    if hub.status().secure_input != secure {
        hub.update_status(|s| s.secure_input = secure);
    }
    let Some(t) = tap else { return };
    if t.mode != Mode::Active {
        return;
    }
    unsafe {
        if CGEventTapIsEnabled(t.port) {
            if hub.status().state == TapState::Disabled {
                hub.update_status(|s| s.state = TapState::Active);
            }
            return;
        }
        CGEventTapEnable(t.port, true);
        let ok = CGEventTapIsEnabled(t.port);
        log::warn!("pm-hook: event tap was disabled without notice; re-enabled: {ok}");
        hub.update_status(|s| {
            s.reenabled += 1;
            s.state = if ok { TapState::Active } else { TapState::Disabled };
        });
    }
}

unsafe extern "C" fn tap_cb(proxy: CGEventTapProxy, ty: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = &*(info as *const TapCtx);
    if ty == kCGEventTapDisabledByTimeout || ty == kCGEventTapDisabledByUserInput {
        let port = ctx.port.get();
        let mut ok = false;
        if !port.is_null() {
            CGEventTapEnable(port, true);
            ok = CGEventTapIsEnabled(port);
        }
        log::warn!(
            "pm-hook: event tap disabled ({}); re-enabled: {ok}",
            if ty == kCGEventTapDisabledByTimeout { "timeout" } else { "user input" }
        );
        if !ctx.listen_only.get() {
            ctx.hub.update_status(|s| {
                s.reenabled += 1;
                s.state = if ok { TapState::Active } else { TapState::Disabled };
            });
        }
        return event;
    }
    let Some(mut ev) = event_view(event, false) else { return event };
    let t0 = Instant::now();
    let pool = objc_autoreleasePoolPush();
    let out = if ctx.listen_only.get() {
        ctx.hub.dispatch_listen_only(&mut ev);
        vec![Out::Original]
    } else {
        ctx.hub.dispatch(&mut ev)
    };
    objc_autoreleasePoolPop(pool);
    let dt = t0.elapsed();
    if dt > Duration::from_millis(250) {
        log::warn!("pm-hook: dispatching {:?} took {} ms (the system disables taps that take too long)", ev.kind(), dt.as_millis());
    }
    deliver(proxy, event, out)
}

/// Turn a dispatch result into the tap's return value, posting synthetic
/// events (and copies of the original that aren't last) after the tap.
unsafe fn deliver(proxy: CGEventTapProxy, event: CGEventRef, out: Vec<Out>) -> CGEventRef {
    match out.as_slice() {
        [Out::Original] => return event,
        [] => return std::ptr::null_mut(),
        _ => {}
    }
    let n = out.len();
    let mut ret = std::ptr::null_mut();
    for (i, o) in out.into_iter().enumerate() {
        match o {
            Out::Original if i + 1 == n => ret = event,
            Out::Original => {
                let c = CGEventCreateCopy(event);
                if !c.is_null() {
                    CGEventTapPostEvent(proxy, c);
                    CFRelease(c as CFTypeRef);
                }
            }
            Out::Synth(Synthetic::Mac(e)) => CGEventTapPostEvent(proxy, e.as_ptr()),
            Out::Synth(_) => {}
        }
    }
    ret
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_mapping_round_trips() {
        for t in EventType::ALL {
            assert_eq!(kind_of(cg_type(t)), Some(t));
        }
        assert_eq!(kind_of(0), None);
        assert_eq!(kind_of(kCGEventTapDisabledByTimeout), None);
        let m = cg_mask(EventMask::KEYS);
        assert_eq!(m, 1 << 10 | 1 << 11 | 1 << 12);
        assert_eq!(cg_mask(EventMask::SCROLL | EventType::SystemDefined), 1 << 22 | 1 << 14);
    }
}
