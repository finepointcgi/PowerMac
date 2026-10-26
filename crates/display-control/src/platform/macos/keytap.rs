//! Media-key takeover and hotkeys through an active CGEventTap (needs
//! Accessibility). Brightness / volume keys arrive as `NSSystemDefined`
//! events (subtype 8); recent Apple keyboards also send the brightness keys
//! as key events 144 / 145. Keys aimed at a display we don't handle (the
//! built-in one, or one without control) pass through untouched.
#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use objc2::encode::{Encode, Encoding};
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use super::ffi::*;
use crate::config::Mods;
use crate::engine::Inner;
use crate::keys::{hotkey_action, mac_keycode_media, parse_system_defined, Action, KeyRouter, KeySource, MediaKeyEvent, Route};

/// `CGEventRef` with the Objective-C type encoding `^{__CGEvent=}`.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CgEventPtr(CGEventRef);

unsafe impl Encode for CgEventPtr {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
}

struct TapContext {
    inner: Arc<Inner>,
    port: AtomicPtr<c_void>,
    router: Mutex<KeyRouter>,
    /// Mac keycodes whose hotkey key-down we swallowed (swallow the up too).
    hotkey_downs: Mutex<Vec<u16>>,
}

fn mods_from_flags(f: u64) -> Mods {
    Mods { win: f & kCGEventFlagMaskCommand != 0, ctrl: f & kCGEventFlagMaskControl != 0, alt: f & kCGEventFlagMaskAlternate != 0, shift: f & kCGEventFlagMaskShift != 0 }
}

/// `(subtype, data1)` of a system-defined event, via NSEvent.
fn system_defined_data(event: CGEventRef) -> Option<(i16, i64)> {
    autoreleasepool(|_| unsafe {
        let ev: *mut AnyObject = msg_send![class!(NSEvent), eventWithCGEvent: CgEventPtr(event)];
        if ev.is_null() {
            return None;
        }
        let subtype: i16 = msg_send![ev, subtype];
        let data1: isize = msg_send![ev, data1];
        Some((subtype, data1 as i64))
    })
}

fn cursor_display(inner: &Inner) -> Option<String> {
    let p = cursor_location()?;
    inner.display_at(p.x, p.y)
}

fn act(ctx: &TapContext, target: &str, action: Action) {
    if let Some(fb) = ctx.inner.perform(target, action) {
        if ctx.inner.lock().config.show_osd {
            super::osd::show(fb);
        }
    }
}

fn handle_media(ctx: &TapContext, ev: MediaKeyEvent, source: KeySource, mods: Mods) -> bool {
    let target = if ev.down { cursor_display(&ctx.inner) } else { None };
    let route = {
        let mut r = ctx.router.lock().unwrap_or_else(|e| e.into_inner());
        r.route(ev, source, || target.as_deref().is_some_and(|t| ctx.inner.takes_over(t, ev.key)), Instant::now())
    };
    match route {
        Route::Pass => false,
        Route::Swallow => true,
        Route::Act => {
            if let Some(t) = target {
                act(ctx, &t, Action::for_media_key(ev.key, mods));
            }
            true
        }
    }
}

extern "C" fn tap_callback(_proxy: CGEventTapProxy, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const TapContext) };
    if etype == kCGEventTapDisabledByTimeout || etype == kCGEventTapDisabledByUserInput {
        let port = ctx.port.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }
    if event.is_null() || !ctx.inner.lock().config.enabled {
        return event;
    }
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mods = mods_from_flags(unsafe { CGEventGetFlags(event) });
        match etype {
            kCGEventSystemDefined => {
                let Some((subtype, data1)) = system_defined_data(event) else { return false };
                let Some(ev) = parse_system_defined(subtype, data1) else { return false };
                handle_media(ctx, ev, KeySource::SystemDefined, mods)
            }
            kCGEventKeyDown | kCGEventKeyUp => {
                let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
                let down = etype == kCGEventKeyDown;
                let repeat = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventAutorepeat) } != 0;
                if let Some(key) = mac_keycode_media(code) {
                    return handle_media(ctx, MediaKeyEvent { key, down, repeat }, KeySource::KeyCode, mods);
                }
                let mut downs = ctx.hotkey_downs.lock().unwrap_or_else(|e| e.into_inner());
                if !down {
                    if let Some(i) = downs.iter().position(|&c| c == code) {
                        downs.remove(i);
                        return true;
                    }
                    return false;
                }
                let Some((vk, _)) = pm_input::vk::mac_to_vk(code) else { return false };
                let action = hotkey_action(&ctx.inner.lock().config, vk, mods);
                let Some(action) = action else { return false };
                if !downs.contains(&code) {
                    downs.push(code);
                }
                drop(downs);
                if let Some(t) = cursor_display(&ctx.inner) {
                    act(ctx, &t, action);
                }
                true
            }
            _ => false,
        }
    }))
    .unwrap_or(false);
    if swallow {
        std::ptr::null()
    } else {
        event
    }
}

pub(crate) struct KeyHook {
    runloop: usize,
    thread: Option<JoinHandle<()>>,
}

impl KeyHook {
    pub(crate) fn start(inner: Arc<Inner>) -> Result<Self> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            bail!("Accessibility permission is required to take over the brightness / volume keys");
        }
        let (ready_tx, ready_rx) = mpsc::channel::<Option<usize>>();
        let ctx = Box::new(TapContext { inner, port: AtomicPtr::new(std::ptr::null_mut()), router: Mutex::new(KeyRouter::default()), hotkey_downs: Mutex::new(Vec::new()) });
        let thread = std::thread::Builder::new().name("display-control-keys".into()).spawn(move || unsafe {
            let ctx_ptr = Box::into_raw(ctx);
            let mask = event_mask(kCGEventKeyDown) | event_mask(kCGEventKeyUp) | event_mask(kCGEventSystemDefined);
            let port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, tap_callback, ctx_ptr as *mut c_void);
            if port.is_null() {
                drop(Box::from_raw(ctx_ptr));
                let _ = ready_tx.send(None);
                return;
            }
            (*ctx_ptr).port.store(port as *mut c_void, Ordering::SeqCst);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = ready_tx.send(Some(rl as usize));
            CFRunLoopRun();
            CGEventTapEnable(port, false);
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(port);
            CFRelease(src);
            CFRelease(port);
            drop(Box::from_raw(ctx_ptr));
        })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Some(rl)) => {
                log::info!("display-control: key takeover active");
                Ok(Self { runloop: rl, thread: Some(thread) })
            }
            Ok(None) => {
                let _ = thread.join();
                bail!("could not create the event tap (Accessibility / Input Monitoring permission?)")
            }
            Err(_) => bail!("event tap thread did not start"),
        }
    }

    pub(crate) fn stop(&mut self) {
        if self.runloop != 0 {
            unsafe {
                CFRunLoopStop(self.runloop as CFRunLoopRef);
                CFRunLoopWakeUp(self.runloop as CFRunLoopRef);
            }
            self.runloop = 0;
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for KeyHook {
    fn drop(&mut self) {
        self.stop();
    }
}
