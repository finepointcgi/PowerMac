//! macOS engine: a pointer thread that keeps the HID properties applied
//! (and restores them on stop), plus an optional `CGEventTap` that remaps the
//! middle / side buttons.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::actions::Executor;
use crate::applier::{apply_device, restore_device, PropStore, Touched};
use crate::mac_hid::{Client, Service, KEY_MOUSE_ACCEL, KEY_TRACKPAD_ACCEL};
use crate::{ButtonAction, Config, PointerSettings, PM_INPUT_INJECTED_TAG, REAPPLY_INTERVAL, SELF_INJECTED_TAG};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventTapCallBack = unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;

    fn CFRelease(cf: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFMachPortCreateRunLoopSource(alloc: CFAllocatorRef, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num: isize,
        key_cb: *const c_void,
        value_cb: *const c_void,
    ) -> CFDictionaryRef;
    fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const c_char, encoding: u32) -> CFStringRef;
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
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
}

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
const kCFStringEncodingUTF8: u32 = 0x0800_0100;

const kCGHIDEventTap: u32 = 0;
const kCGSessionEventTap: u32 = 1;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;
const kCGEventOtherMouseDown: u32 = 25;
const kCGEventOtherMouseUp: u32 = 26;
const kCGEventOtherMouseDragged: u32 = 27;
const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;
const kCGMouseEventButtonNumber: u32 = 3;
const kCGEventSourceUserData: u32 = 42;

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

/// Whether `CoreDockSendNotification` resolves (diagnostics/tests).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn dock_available() -> bool {
    unsafe { !dlsym(RTLD_DEFAULT, c"CoreDockSendNotification".as_ptr()).is_null() }
}

/// `CoreDockSendNotification(name, 0)` (HIServices, private). False when the
/// symbol is missing.
pub(crate) fn dock_notification(name: &str) -> bool {
    type F = unsafe extern "C" fn(CFStringRef, *mut c_void);
    unsafe {
        let sym = dlsym(RTLD_DEFAULT, c"CoreDockSendNotification".as_ptr());
        if sym.is_null() {
            return false;
        }
        let Ok(c) = std::ffi::CString::new(name) else { return false };
        let s = CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8);
        if s.is_null() {
            return false;
        }
        let f: F = std::mem::transmute::<*mut c_void, F>(sym);
        f(s, std::ptr::null_mut());
        CFRelease(s);
        true
    }
}

// ---------------------------------------------------------------------------
// Pointer settings thread
// ---------------------------------------------------------------------------

/// A service's properties, falling back to the system-wide value.
struct Props<'a> {
    svc: &'a Service,
    client: &'a Client,
}

impl PropStore for Props<'_> {
    fn get(&self, key: &str) -> Option<i64> {
        self.svc.get_i64(key).or_else(|| self.client.get_i64(key))
    }
    fn set(&self, key: &str, value: i64) -> bool {
        self.svc.set_i64(key, value)
    }
}

enum Msg {
    Update(Config),
    Stop,
}

/// Re-set every applied value this often, in case the system pushed its
/// own values to a device without the global preference changing.
const REASSERT_EVERY: u32 = 5; // x REAPPLY_INTERVAL

#[derive(Default)]
struct Applier {
    touched: HashMap<u64, Touched>,
    /// Last seen system-wide acceleration values.
    system: HashMap<&'static str, Option<i64>>,
    polls: u32,
}

impl Applier {
    fn apply(&mut self, client: &Client, cfg: &Config, config_changed: bool) {
        let all_noop = !cfg.enabled
            || (cfg.mouse.is_noop()
                && cfg.trackpad.is_noop()
                && cfg.devices.iter().all(|d| d.disable_acceleration.is_none() && d.speed.is_none()));
        if all_noop && self.touched.is_empty() {
            return;
        }
        self.polls = self.polls.wrapping_add(1);
        let reassert = self.polls.is_multiple_of(REASSERT_EVERY);

        // The user (System Settings) or the system changed a global value:
        // it is the new original, and ours has to go back on.
        for key in [KEY_MOUSE_ACCEL, KEY_TRACKPAD_ACCEL] {
            let now = client.get_i64(key);
            let prev = self.system.insert(key, now);
            if let (Some(Some(prev)), Some(now)) = (prev, now) {
                if prev != now {
                    log::info!("mouse-tuning: system {key} changed {prev} -> {now}");
                    for t in self.touched.values_mut() {
                        t.system_changed(key, now);
                    }
                }
            }
        }

        let mut seen = Vec::new();
        for (svc, info) in client.pointers() {
            let Some(id) = svc.registry_id() else { continue };
            seen.push(id);
            let s = if cfg.enabled { cfg.effective(&info) } else { PointerSettings::default() };
            if s.is_noop() && !self.touched.contains_key(&id) {
                continue;
            }
            let key = svc.accel_key(info.kind);
            let t = self.touched.entry(id).or_default();
            let props = Props { svc: &svc, client };
            if apply_device(&props, &key, &s, t, reassert) && (config_changed || !reassert) {
                log::info!("mouse-tuning: {} ({:?}) {:?}: set {:?}, originals {:?}", info.name, info.kind, s, t.last, t.orig);
            }
            if t.is_empty() {
                self.touched.remove(&id);
            }
        }
        // Devices that went away take their state with them.
        self.touched.retain(|id, _| seen.contains(id));
    }

    fn restore(&mut self, client: &Client) {
        for (svc, info) in client.pointers() {
            let Some(id) = svc.registry_id() else { continue };
            if let Some(mut t) = self.touched.remove(&id) {
                restore_device(&Props { svc: &svc, client }, &mut t);
                log::info!("mouse-tuning: restored {} ({:?})", info.name, info.kind);
            }
        }
        self.touched.clear();
    }
}

fn pointer_thread(rx: mpsc::Receiver<Msg>, mut cfg: Config) {
    let Some(client) = Client::new() else {
        log::error!("mouse-tuning: IOHIDEventSystemClientCreateSimpleClient failed; pointer settings unavailable");
        while let Ok(m) = rx.recv() {
            if matches!(m, Msg::Stop) {
                break;
            }
        }
        return;
    };
    let mut applier = Applier::default();
    let mut changed = true;
    loop {
        applier.apply(&client, &cfg, changed);
        changed = false;
        match rx.recv_timeout(REAPPLY_INTERVAL) {
            Ok(Msg::Update(c)) => {
                changed = c != cfg;
                cfg = c;
            }
            Ok(Msg::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    applier.restore(&client);
}

// ---------------------------------------------------------------------------
// Button tap
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
// SAFETY: only handed to CFRunLoopStop, which is thread-safe.
unsafe impl Send for SendPtr {}

struct TapCtx {
    table: Arc<Mutex<Vec<ButtonAction>>>,
    actions: mpsc::Sender<ButtonAction>,
    /// Buttons whose down event we swallowed (so their drag/up go too).
    swallowed: Cell<u32>,
    tap: Cell<CFMachPortRef>,
}

struct ButtonTap {
    stop: Arc<AtomicBool>,
    run_loop: SendPtr,
    thread: Option<JoinHandle<()>>,
}

impl ButtonTap {
    fn start(table: Arc<Mutex<Vec<ButtonAction>>>, actions: mpsc::Sender<ButtonAction>) -> Result<Self> {
        if !is_trusted(false) {
            return Err(anyhow!("Accessibility is not granted; mouse buttons cannot be remapped"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let (tx, rx) = mpsc::channel::<std::result::Result<SendPtr, String>>();
        let thread = std::thread::Builder::new().name("mouse-tuning-buttons".into()).spawn(move || unsafe {
            let ctx = Box::into_raw(Box::new(TapCtx {
                table,
                actions,
                swallowed: Cell::new(0),
                tap: Cell::new(std::ptr::null_mut()),
            }));
            let mask = (1u64 << kCGEventOtherMouseDown) | (1u64 << kCGEventOtherMouseUp) | (1u64 << kCGEventOtherMouseDragged);
            let mut tap = std::ptr::null_mut();
            for loc in [kCGHIDEventTap, kCGSessionEventTap] {
                tap = CGEventTapCreate(loc, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, tap_callback, ctx as *mut c_void);
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
            Ok(Ok(rl)) => Ok(Self { stop, run_loop: rl, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(anyhow!(e))
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(anyhow!("button tap thread did not start"))
            }
        }
    }

    fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.stop.store(true, Ordering::SeqCst);
            unsafe { CFRunLoopStop(self.run_loop.0) };
            let _ = t.join();
        }
    }
}

impl Drop for ButtonTap {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "C" fn tap_callback(_proxy: CGEventTapProxy, ty: u32, event: CGEventRef, user: *mut c_void) -> CGEventRef {
    let ctx = &*(user as *const TapCtx);
    if ty == kCGEventTapDisabledByTimeout || ty == kCGEventTapDisabledByUserInput {
        let tap = ctx.tap.get();
        if !tap.is_null() {
            log::warn!("mouse-tuning: button tap disabled ({ty:#x}); re-enabling");
            CGEventTapEnable(tap, true);
        }
        return event;
    }
    if event.is_null() {
        return event;
    }
    let swallow = catch_unwind(AssertUnwindSafe(|| handle_button(ctx, ty, event))).unwrap_or(false);
    if swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

unsafe fn handle_button(ctx: &TapCtx, ty: u32, ev: CGEventRef) -> bool {
    let tag = CGEventGetIntegerValueField(ev, kCGEventSourceUserData);
    if tag == PM_INPUT_INJECTED_TAG as i64 || tag == SELF_INJECTED_TAG as i64 {
        return false;
    }
    let button = CGEventGetIntegerValueField(ev, kCGMouseEventButtonNumber);
    if !(0..32).contains(&button) {
        return false;
    }
    let bit = 1u32 << button;
    match ty {
        kCGEventOtherMouseDown => {
            let action = ctx
                .table
                .lock()
                .ok()
                .and_then(|t| t.get(button as usize).cloned())
                .unwrap_or_default();
            if action == ButtonAction::Default {
                return false;
            }
            ctx.swallowed.set(ctx.swallowed.get() | bit);
            if action != ButtonAction::Disabled {
                let _ = ctx.actions.send(action);
            }
            true
        }
        kCGEventOtherMouseDragged => ctx.swallowed.get() & bit != 0,
        kCGEventOtherMouseUp => {
            let was = ctx.swallowed.get() & bit != 0;
            ctx.swallowed.set(ctx.swallowed.get() & !bit);
            was
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub(crate) struct Engine {
    pointer_tx: mpsc::Sender<Msg>,
    pointer_thread: Option<JoinHandle<()>>,
    table: Arc<Mutex<Vec<ButtonAction>>>,
    executor: Executor,
    tap: Option<ButtonTap>,
    tap_error_logged: bool,
}

impl Engine {
    pub(crate) fn start(config: Config) -> Result<Self> {
        let (pointer_tx, rx) = mpsc::channel();
        let cfg = config.clone();
        let pointer_thread = std::thread::Builder::new()
            .name("mouse-tuning-pointer".into())
            .spawn(move || pointer_thread(rx, cfg))?;
        let executor = Executor::start()?;
        let mut e = Self {
            pointer_tx,
            pointer_thread: Some(pointer_thread),
            table: Arc::new(Mutex::new(config.button_table())),
            executor,
            tap: None,
            tap_error_logged: false,
        };
        e.sync_tap(&config);
        Ok(e)
    }

    fn sync_tap(&mut self, config: &Config) {
        if let Ok(mut t) = self.table.lock() {
            *t = config.button_table();
        }
        let need = config.needs_button_hook();
        match (need, self.tap.is_some()) {
            (true, false) => match self
                .executor
                .sender()
                .ok_or_else(|| anyhow!("action executor stopped"))
                .and_then(|tx| ButtonTap::start(self.table.clone(), tx))
            {
                Ok(t) => {
                    log::info!("mouse-tuning: button remapping active");
                    self.tap = Some(t);
                    self.tap_error_logged = false;
                }
                Err(e) => {
                    if !self.tap_error_logged {
                        log::warn!("mouse-tuning: {e:#}");
                        self.tap_error_logged = true;
                    }
                }
            },
            (false, true) => {
                if let Some(mut t) = self.tap.take() {
                    t.stop();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn update(&mut self, config: Config) {
        self.sync_tap(&config);
        let _ = self.pointer_tx.send(Msg::Update(config));
    }

    pub(crate) fn buttons_active(&self) -> bool {
        self.tap.is_some()
    }

    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // The tap holds a sender of the executor: stop it first.
        if let Some(mut t) = self.tap.take() {
            t.stop();
        }
        if let Some(th) = self.pointer_thread.take() {
            let _ = self.pointer_tx.send(Msg::Stop);
            let _ = th.join();
        }
        self.executor.stop();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
