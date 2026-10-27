//! Media-key takeover and hotkeys through the shared active event tap
//! (`pm-hook`, [`pm_hook::priority::DISPLAY_CONTROL`]; needs Accessibility). Brightness / volume keys arrive as `NSSystemDefined`
//! events (subtype 8); recent Apple keyboards also send the brightness keys
//! as key events 144 / 145. Keys aimed at a display we don't handle (the
//! built-in one, or one without control) pass through untouched.
#![allow(non_upper_case_globals)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

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

/// Returns true to swallow.
fn handle_event(ctx: &TapContext, etype: u32, event: CGEventRef) -> bool {
    if event.is_null() || !ctx.inner.lock().config.enabled {
        return false;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    .unwrap_or(false)
}

/// The subscription; `stop` (or dropping it) unsubscribes.
pub(crate) struct KeyHook {
    guard: Option<pm_hook::Guard>,
}

impl KeyHook {
    pub(crate) fn start(inner: Arc<Inner>) -> Result<Self> {
        use pm_hook::{EventMask, EventType, Subscription, Verdict};
        if unsafe { AXIsProcessTrusted() } == 0 {
            bail!("Accessibility permission is required to take over the brightness / volume keys");
        }
        let ctx = TapContext { inner, router: Mutex::new(KeyRouter::default()), hotkey_downs: Mutex::new(Vec::new()) };
        let mask = EventMask::of(&[EventType::KeyDown, EventType::KeyUp, EventType::SystemDefined]);
        let guard = pm_hook::subscribe(Subscription::new("display-control", pm_hook::priority::DISPLAY_CONTROL, mask, move |ev| {
            if handle_event(&ctx, ev.cg_type(), ev.cg_event()) {
                Verdict::Swallow
            } else {
                Verdict::Pass
            }
        }));
        if !pm_hook::status().is_active() {
            drop(guard);
            bail!("could not create the event tap (Accessibility / Input Monitoring permission?)");
        }
        log::info!("display-control: key takeover active");
        Ok(Self { guard: Some(guard) })
    }

    pub(crate) fn stop(&mut self) {
        self.guard.take();
    }
}
