//! macOS backend: CGEventTap thread for input, Core Animation overlays on
//! the main thread.
//!
//! The tap is created with the best mode the process is allowed:
//! 1. active (mouse + keyboard; hotkeys are swallowed) — needs Accessibility;
//! 2. listen-only mouse + keyboard — needs Input Monitoring;
//! 3. listen-only mouse only — no permission needed.
//!
//! The overlays don't need any permission: the cursor is polled each frame
//! with `CGEventCreate(NULL)`, so the spotlight and crosshairs follow it even
//! with no tap at all.
#![allow(non_upper_case_globals)]

mod ffi;
mod overlay;

pub use overlay::debug_probe;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use objc2::{class, msg_send};

use self::ffi::*;
use crate::config::Mods;
use crate::engine::{Button, Point};
use crate::{InputMode, Shared};

pub fn double_click_time() -> Duration {
    let secs: f64 = unsafe { msg_send![class!(NSEvent), doubleClickInterval] };
    if secs.is_finite() && secs > 0.0 {
        Duration::from_secs_f64(secs)
    } else {
        Duration::from_millis(500)
    }
}

struct TapContext {
    shared: Arc<Shared>,
    port: AtomicPtr<c_void>,
}

fn mods_from_flags(f: u64) -> Mods {
    Mods {
        win: f & kCGEventFlagMaskCommand != 0,
        ctrl: f & kCGEventFlagMaskControl != 0,
        alt: f & kCGEventFlagMaskAlternate != 0,
        shift: f & kCGEventFlagMaskShift != 0,
    }
}

/// Device-dependent flag bit for a modifier keycode (to tell down from up
/// in `kCGEventFlagsChanged`, including left vs right).
fn modifier_bit(keycode: u16) -> Option<u64> {
    Some(match keycode {
        0x3B => NX_DEVICELCTLKEYMASK,
        0x3E => NX_DEVICERCTLKEYMASK,
        0x38 => NX_DEVICELSHIFTKEYMASK,
        0x3C => NX_DEVICERSHIFTKEYMASK,
        0x37 => NX_DEVICELCMDKEYMASK,
        0x36 => NX_DEVICERCMDKEYMASK,
        0x3A => NX_DEVICELALTKEYMASK,
        0x3D => NX_DEVICERALTKEYMASK,
        _ => return None,
    })
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
    if event.is_null() {
        return event;
    }
    // Skip events injected by our own Mouse Without Borders.
    if unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUserData) } == pm_input::INJECTED_TAG as i64 {
        return event;
    }
    let loc = || {
        let p = unsafe { CGEventGetLocation(event) };
        Point::new(p.x, p.y)
    };
    let sh = &ctx.shared;
    match etype {
        kCGEventMouseMoved | kCGEventLeftMouseDragged | kCGEventRightMouseDragged | kCGEventOtherMouseDragged => {
            let (dx, dy) = unsafe {
                (
                    CGEventGetIntegerValueField(event, kCGMouseEventDeltaX) as f64,
                    CGEventGetIntegerValueField(event, kCGMouseEventDeltaY) as f64,
                )
            };
            let p = loc();
            sh.with(|e, now| e.on_mouse_move(p, Some((dx, dy)), now));
        }
        kCGEventLeftMouseDown | kCGEventLeftMouseUp | kCGEventRightMouseDown | kCGEventRightMouseUp => {
            let button = if etype <= kCGEventLeftMouseUp { Button::Left } else { Button::Right };
            let down = etype == kCGEventLeftMouseDown || etype == kCGEventRightMouseDown;
            let p = loc();
            sh.with(|e, now| e.on_button(button, down, p, now));
        }
        kCGEventOtherMouseDown => {
            sh.with(|e, now| e.dismiss_spotlight(now));
        }
        kCGEventKeyDown | kCGEventKeyUp => {
            let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
            let Some((vk, _)) = pm_input::vk::mac_to_vk(code) else { return event };
            let down = etype == kCGEventKeyDown;
            let repeat = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventAutorepeat) } != 0;
            let mods = mods_from_flags(unsafe { CGEventGetFlags(event) });
            let swallow = if down && repeat {
                sh.lock().engine.swallowing(vk)
            } else {
                sh.with(|e, now| e.on_key(vk, down, mods, now))
            };
            if swallow {
                return std::ptr::null();
            }
        }
        kCGEventFlagsChanged => {
            let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
            let (Some(bit), Some((vk, _))) = (modifier_bit(code), pm_input::vk::mac_to_vk(code)) else { return event };
            let flags = unsafe { CGEventGetFlags(event) };
            let down = flags & bit != 0;
            let mods = mods_from_flags(flags);
            sh.with(|e, now| e.on_key(vk, down, mods, now));
        }
        _ => {}
    }
    event
}

const MOUSE_MASK: u64 = event_mask(kCGEventLeftMouseDown)
    | event_mask(kCGEventLeftMouseUp)
    | event_mask(kCGEventRightMouseDown)
    | event_mask(kCGEventRightMouseUp)
    | event_mask(kCGEventMouseMoved)
    | event_mask(kCGEventLeftMouseDragged)
    | event_mask(kCGEventRightMouseDragged)
    | event_mask(kCGEventOtherMouseDown)
    | event_mask(kCGEventOtherMouseDragged);
const KEY_MASK: u64 = event_mask(kCGEventKeyDown) | event_mask(kCGEventKeyUp) | event_mask(kCGEventFlagsChanged);

pub(crate) struct Runtime {
    tap_runloop: usize,
    tap_thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        overlay::install(shared.clone());
        shared.set_kick(overlay::kick);

        let (ready_tx, ready_rx) = mpsc::channel::<Option<(usize, InputMode)>>();
        let ctx = Box::new(TapContext { shared: shared.clone(), port: AtomicPtr::new(std::ptr::null_mut()) });
        let tap_thread = std::thread::Builder::new().name("mouse-utils-tap".into()).spawn(move || unsafe {
            let ctx_ptr = Box::into_raw(ctx);
            let attempts = [
                (kCGEventTapOptionDefault, MOUSE_MASK | KEY_MASK, InputMode::Full),
                (kCGEventTapOptionListenOnly, MOUSE_MASK | KEY_MASK, InputMode::ListenOnly),
                (kCGEventTapOptionListenOnly, MOUSE_MASK, InputMode::MouseOnly),
            ];
            // Diagnostics: MOUSE_UTILS_MAX_INPUT=listen|mouse|none caps the tap mode
            // (to exercise the fallback paths on a machine that has permissions).
            let skip = match std::env::var("MOUSE_UTILS_MAX_INPUT").as_deref() {
                Ok("listen") => 1,
                Ok("mouse") => 2,
                Ok("none") => 3,
                _ => 0,
            };
            let mut created = None;
            for (opt, mask, mode) in attempts.into_iter().skip(skip) {
                // Without Accessibility an active tap can't be created; don't bother asking.
                if opt == kCGEventTapOptionDefault && AXIsProcessTrusted() == 0 {
                    continue;
                }
                if mask & KEY_MASK != 0 && opt == kCGEventTapOptionListenOnly && !CGPreflightListenEventAccess() {
                    continue;
                }
                let port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, opt, mask, tap_callback, ctx_ptr as *mut c_void);
                if !port.is_null() {
                    created = Some((port, mode));
                    break;
                }
            }
            let Some((port, mode)) = created else {
                drop(Box::from_raw(ctx_ptr));
                let _ = ready_tx.send(None);
                return;
            };
            (*ctx_ptr).port.store(port as *mut c_void, Ordering::SeqCst);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = ready_tx.send(Some((rl as usize, mode)));
            CFRunLoopRun();
            CGEventTapEnable(port, false);
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(port);
            CFRelease(src);
            CFRelease(port);
            drop(Box::from_raw(ctx_ptr));
        })?;

        let (rl, tap_thread) = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Some((rl, mode))) => {
                shared.set_input_mode(mode);
                match mode {
                    InputMode::Full => {}
                    InputMode::ListenOnly => log::warn!(
                        "mouse-utils: no Accessibility permission; hotkeys work but also reach the focused app"
                    ),
                    _ => log::warn!(
                        "mouse-utils: no Accessibility / Input Monitoring permission; hotkeys and double-Ctrl are disabled \
                         (click highlights and programmatic triggers still work)"
                    ),
                }
                (rl, Some(tap_thread))
            }
            Ok(None) => {
                let _ = tap_thread.join();
                shared.set_input_mode(InputMode::None);
                log::warn!("mouse-utils: could not create any event tap; only programmatic triggers work");
                (0, None)
            }
            Err(_) => {
                shared.set_input_mode(InputMode::None);
                log::warn!("mouse-utils: event tap thread did not start");
                (0, None)
            }
        };
        Ok(Runtime { tap_runloop: rl, tap_thread })
    }

    pub(crate) fn stop(&mut self) {
        if self.tap_runloop != 0 {
            unsafe {
                CFRunLoopStop(self.tap_runloop as CFRunLoopRef);
                CFRunLoopWakeUp(self.tap_runloop as CFRunLoopRef);
            }
            self.tap_runloop = 0;
        }
        if let Some(t) = self.tap_thread.take() {
            let _ = t.join();
        }
        overlay::uninstall();
    }
}

/// Pump the main run loop for `dur` (main thread only).
pub fn run_main_loop_for(dur: Duration) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        std::thread::sleep(dur);
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let end = std::time::Instant::now() + dur;
    while std::time::Instant::now() < end {
        let left = end.saturating_duration_since(std::time::Instant::now()).as_secs_f64().min(0.05);
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left, 0) };
    }
}
