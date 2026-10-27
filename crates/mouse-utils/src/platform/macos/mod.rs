//! macOS backend: a subscriber of the shared event tap (`pm-hook`,
//! [`pm_hook::priority::MOUSE_UTILS`], last: it only observes and swallows
//! nothing but its own hotkeys) for input, Core Animation overlays on the
//! main thread.
//!
//! The input mode follows the shared tap's state:
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

use std::sync::Arc;
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

/// Returns true to swallow (only our own hotkeys, in active mode).
fn handle_event(sh: &Arc<Shared>, etype: u32, event: CGEventRef) -> bool {
    if event.is_null() {
        return false;
    }
    let loc = || {
        let p = unsafe { CGEventGetLocation(event) };
        Point::new(p.x, p.y)
    };
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
            let Some((vk, _)) = pm_input::vk::mac_to_vk(code) else { return false };
            let down = etype == kCGEventKeyDown;
            let repeat = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventAutorepeat) } != 0;
            let mods = mods_from_flags(unsafe { CGEventGetFlags(event) });
            let swallow = if down && repeat {
                sh.lock().engine.swallowing(vk)
            } else {
                sh.with(|e, now| e.on_key(vk, down, mods, now))
            };
            return swallow;
        }
        kCGEventFlagsChanged => {
            let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
            let (Some(bit), Some((vk, _))) = (modifier_bit(code), pm_input::vk::mac_to_vk(code)) else { return false };
            let flags = unsafe { CGEventGetFlags(event) };
            let down = flags & bit != 0;
            let mods = mods_from_flags(flags);
            sh.with(|e, now| e.on_key(vk, down, mods, now));
        }
        _ => {}
    }
    false
}

/// The input mode the shared tap's state allows, capped by
/// `MOUSE_UTILS_MAX_INPUT=listen|mouse|none` (diagnostics: exercises the
/// fallback paths on a machine that has permissions).
fn input_mode(st: &pm_hook::Status, cap: u8) -> InputMode {
    let mode = match st.state {
        pm_hook::TapState::Active => InputMode::Full,
        pm_hook::TapState::ListenOnly { keyboard: true } => InputMode::ListenOnly,
        pm_hook::TapState::ListenOnly { keyboard: false } => InputMode::MouseOnly,
        _ => InputMode::None,
    };
    let rank = |m: InputMode| match m {
        InputMode::Full => 0,
        InputMode::ListenOnly => 1,
        InputMode::MouseOnly => 2,
        _ => 3,
    };
    let capped = [InputMode::Full, InputMode::ListenOnly, InputMode::MouseOnly, InputMode::None][cap.min(3) as usize];
    if rank(mode) >= rank(capped) {
        mode
    } else {
        capped
    }
}

fn log_mode(mode: InputMode) {
    match mode {
        InputMode::Full => {}
        InputMode::ListenOnly => log::warn!("mouse-utils: no Accessibility permission; hotkeys work but also reach the focused app"),
        InputMode::MouseOnly => log::warn!(
            "mouse-utils: no Accessibility / Input Monitoring permission; hotkeys and double-Ctrl are disabled \
             (click highlights and programmatic triggers still work)"
        ),
        _ => log::warn!("mouse-utils: could not create any event tap; only programmatic triggers work"),
    }
}

pub(crate) struct Runtime {
    guard: Option<pm_hook::Guard>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        use pm_hook::{EventMask, EventType, Subscription, Verdict};
        overlay::install(shared.clone());
        shared.set_kick(overlay::kick);

        let cap: u8 = match std::env::var("MOUSE_UTILS_MAX_INPUT").as_deref() {
            Ok("listen") => 1,
            Ok("mouse") => 2,
            Ok("none") => 3,
            _ => 0,
        };
        if cap >= 3 {
            shared.set_input_mode(InputMode::None);
            log_mode(InputMode::None);
            return Ok(Runtime { guard: None });
        }
        let mut mask = EventMask::of(&[
            EventType::LeftMouseDown,
            EventType::LeftMouseUp,
            EventType::RightMouseDown,
            EventType::RightMouseUp,
            EventType::MouseMoved,
            EventType::LeftMouseDragged,
            EventType::RightMouseDragged,
            EventType::OtherMouseDown,
            EventType::OtherMouseDragged,
        ]);
        if cap < 2 {
            mask = mask | EventMask::KEYS;
        }
        let sh = shared.clone();
        let sh_status = shared.clone();
        let guard = pm_hook::subscribe(
            Subscription::new("mouse-utils", pm_hook::priority::MOUSE_UTILS, mask, move |ev| {
                let swallow = handle_event(&sh, ev.cg_type(), ev.cg_event());
                // Capped to listen-only: hotkeys also reach the focused app.
                if swallow && cap == 0 {
                    Verdict::Swallow
                } else {
                    Verdict::Pass
                }
            })
            // Skip events injected by our own Mouse Without Borders.
            .ignore_tags(&[pm_input::INJECTED_TAG])
            // Without Accessibility: observe through a listen-only tap.
            .allow_listen_only()
            .on_status(move |st| {
                let mode = input_mode(st, cap);
                if sh_status.current_input_mode() != mode {
                    sh_status.set_input_mode(mode);
                    log_mode(mode);
                }
            }),
        );
        Ok(Runtime { guard: Some(guard) })
    }

    pub(crate) fn stop(&mut self) {
        self.guard.take();
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
