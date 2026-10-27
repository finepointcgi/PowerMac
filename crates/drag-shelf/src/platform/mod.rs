//! Platform backends and the input-hook subscriptions.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use pm_hook::{EventMask, EventType, Subscription, Verdict};

use crate::shake::ShakeDetector;
use crate::{Hotkey, Point, Shared};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{cursor, Backend};

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub(crate) use other::{cursor, Backend};

/// Watch left-button drags for shakes (never swallows anything). The
/// detector runs on the hook thread; a shake and every button release are
/// handed to the UI thread, which checks the drag pasteboard.
pub(crate) fn drag_watch(shared: Arc<Shared>) -> pm_hook::Guard {
    let mut det = ShakeDetector::default();
    let mut fired = false;
    let t0 = Instant::now();
    let mask = EventMask::of(&[EventType::LeftMouseDown, EventType::LeftMouseDragged, EventType::LeftMouseUp]);
    pm_hook::subscribe(
        Subscription::new("drag-shelf", pm_hook::priority::DRAG_SHELF, mask, move |ev| {
            match ev.kind() {
                EventType::LeftMouseDown => {
                    det.reset();
                    fired = false;
                }
                EventType::LeftMouseUp => {
                    det.reset();
                    fired = false;
                    Backend::button_up();
                }
                EventType::LeftMouseDragged if !fired && shared.shake_on.load(Ordering::Relaxed) => {
                    let Some((x, y)) = location(ev) else { return Verdict::Pass };
                    let th = *shared.thresholds.lock().unwrap_or_else(|e| e.into_inner());
                    if det.feed(x, y, t0.elapsed().as_secs_f64(), &th) {
                        // One shelf per drag.
                        fired = true;
                        Backend::shake(Point::new(x, y));
                    }
                }
                _ => {}
            }
            Verdict::Pass
        })
        .ignore_tags(&[pm_hook::tags::MWB]),
    )
}

/// The pointer position of a mouse event (global, top-left origin).
#[cfg(target_os = "macos")]
fn location(ev: &pm_hook::Event<'_>) -> Option<(f64, f64)> {
    Some(ev.location())
}

#[cfg(windows)]
fn location(ev: &pm_hook::Event<'_>) -> Option<(f64, f64)> {
    ev.mouse().map(|(_, m)| (m.pt.x as f64, m.pt.y as f64))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn location(_ev: &pm_hook::Event<'_>) -> Option<(f64, f64)> {
    None
}

/// The key of a key-down event as (VK, cmd/win, ctrl, alt, shift); `None`
/// for auto-repeats and keys without a VK.
#[cfg(target_os = "macos")]
fn key_of(ev: &pm_hook::Event<'_>) -> Option<(u32, bool, bool, bool, bool)> {
    const KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
    const AUTOREPEAT: u32 = 8; // kCGKeyboardEventAutorepeat
    if ev.integer_field(AUTOREPEAT) != 0 {
        return None;
    }
    let (vk, _) = pm_input::vk::mac_to_vk(ev.integer_field(KEYCODE) as u16)?;
    let f = ev.flags();
    Some((vk, f & 0x0010_0000 != 0, f & 0x0004_0000 != 0, f & 0x0008_0000 != 0, f & 0x0002_0000 != 0))
}

#[cfg(not(target_os = "macos"))]
fn key_of(_ev: &pm_hook::Event<'_>) -> Option<(u32, bool, bool, bool, bool)> {
    None
}

/// Listen for the "new shelf" hotkey (swallowed when it matches).
pub(crate) fn hotkey_watch(hk: Hotkey) -> pm_hook::Guard {
    pm_hook::subscribe(
        Subscription::new("drag-shelf-hotkey", pm_hook::priority::DRAG_SHELF_HOTKEY, EventMask::of(&[EventType::KeyDown]), move |ev| {
            if ev.kind() != EventType::KeyDown {
                return Verdict::Pass;
            }
            match key_of(ev) {
                Some((vk, w, c, a, s)) if hk.matches(vk, w, c, a, s) => {
                    Backend::hotkey();
                    Verdict::Swallow
                }
                _ => Verdict::Pass,
            }
        })
        .ignore_tags(&[pm_hook::tags::MWB]),
    )
}
