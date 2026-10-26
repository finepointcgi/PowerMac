#![allow(non_upper_case_globals)]
//! Event tap thread + worker thread.
//!
//! * The **tap thread** owns a `CGEventTap` (session level, active so that
//!   snap hotkeys can be swallowed) on its own `CFRunLoop`. Its callback
//!   only classifies events and forwards them over a channel.
//! * The **worker thread** does all Accessibility calls and engine updates
//!   and posts overlay changes to the main queue.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use super::ax::{has_permissions, MacWindow, NativeWindowSystem};
use super::ffi::*;
use super::{overlay, screens};
use crate::config::Modifiers;
use crate::engine::{DragInput, DragResult, WorkArea};
use crate::geometry::{Point, Rect};
use crate::hotkey_bits::{self, Action};
use crate::navigation::Direction;
use crate::Shared;

enum Msg {
    MouseDown(Point),
    MouseDragged(Point, u64),
    MouseUp,
    RightDown,
    Flags(u64),
    Hotkey(Action, Direction),
    Stop,
}

struct TapContext {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    port: AtomicPtr<c_void>,
}

fn mods_from_flags(f: u64) -> Modifiers {
    Modifiers {
        ctrl: f & kCGEventFlagMaskControl != 0,
        alt: f & kCGEventFlagMaskAlternate != 0,
        shift: f & kCGEventFlagMaskShift != 0,
        meta: f & kCGEventFlagMaskCommand != 0,
    }
}

extern "C" fn tap_callback(_proxy: CGEventTapProxy, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const TapContext) };
    match etype {
        kCGEventTapDisabledByTimeout | kCGEventTapDisabledByUserInput => {
            let port = ctx.port.load(Ordering::SeqCst);
            if !port.is_null() {
                unsafe { CGEventTapEnable(port, true) };
            }
        }
        kCGEventLeftMouseDown => {
            let p = unsafe { CGEventGetLocation(event) };
            let _ = ctx.tx.send(Msg::MouseDown(Point::new(p.x as i32, p.y as i32)));
        }
        kCGEventLeftMouseDragged => {
            let p = unsafe { CGEventGetLocation(event) };
            let f = unsafe { CGEventGetFlags(event) };
            let _ = ctx.tx.send(Msg::MouseDragged(Point::new(p.x as i32, p.y as i32), f));
        }
        kCGEventLeftMouseUp => {
            let _ = ctx.tx.send(Msg::MouseUp);
        }
        kCGEventRightMouseDown => {
            let _ = ctx.tx.send(Msg::RightDown);
        }
        kCGEventFlagsChanged => {
            let f = unsafe { CGEventGetFlags(event) };
            let _ = ctx.tx.send(Msg::Flags(f));
        }
        kCGEventKeyDown => {
            let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) };
            let dir = match code {
                kVK_LeftArrow => Some(Direction::Left),
                kVK_RightArrow => Some(Direction::Right),
                kVK_UpArrow => Some(Direction::Up),
                kVK_DownArrow => Some(Direction::Down),
                _ => None,
            };
            if let Some(dir) = dir {
                let m = mods_from_flags(unsafe { CGEventGetFlags(event) });
                let bits = ctx.shared.hotkeys.load(Ordering::Relaxed);
                if let Some(a) = hotkey_bits::classify(bits, m, dir.is_horizontal()) {
                    let _ = ctx.tx.send(Msg::Hotkey(a, dir));
                    return std::ptr::null(); // swallow
                }
            }
        }
        _ => {}
    }
    event
}

pub(crate) struct Runtime {
    tap_runloop: usize,
    tap_thread: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
    tx: Sender<Msg>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        if !has_permissions() {
            bail!(
                "fancyzones needs the Accessibility permission (System Settings → Privacy & Security → Accessibility) \
                 for the process running it"
            );
        }
        // Don't let a hung app stall the worker for the default ~6 s.
        unsafe { AXUIElementSetMessagingTimeout(super::ax::AxElement::system_wide().raw(), 1.0) };
        let (tx, rx) = mpsc::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize, String>>();
        let ctx = Box::new(TapContext { tx: tx.clone(), shared: shared.clone(), port: AtomicPtr::new(std::ptr::null_mut()) });

        let tap_thread = std::thread::Builder::new().name("fancyzones-tap".into()).spawn(move || unsafe {
            let ctx_ptr = Box::into_raw(ctx);
            let mask = event_mask(kCGEventLeftMouseDown)
                | event_mask(kCGEventLeftMouseUp)
                | event_mask(kCGEventLeftMouseDragged)
                | event_mask(kCGEventRightMouseDown)
                | event_mask(kCGEventFlagsChanged)
                | event_mask(kCGEventKeyDown);
            let port = CGEventTapCreate(
                kCGSessionEventTap,
                kCGHeadInsertEventTap,
                kCGEventTapOptionDefault,
                mask,
                tap_callback,
                ctx_ptr as *mut c_void,
            );
            if port.is_null() {
                drop(Box::from_raw(ctx_ptr));
                let _ = ready_tx.send(Err("CGEventTapCreate failed (Accessibility / Input Monitoring permission missing?)".into()));
                return;
            }
            (*ctx_ptr).port.store(port as *mut c_void, Ordering::SeqCst);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = ready_tx.send(Ok(rl as usize));
            CFRunLoopRun();
            CGEventTapEnable(port, false);
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(port);
            CFRelease(src);
            CFRelease(port);
            drop(Box::from_raw(ctx_ptr));
        })?;

        let rl = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(rl)) => rl,
            Ok(Err(e)) => {
                let _ = tap_thread.join();
                bail!(e)
            }
            Err(_) => bail!("event tap thread did not start"),
        };

        let worker = std::thread::Builder::new()
            .name("fancyzones-worker".into())
            .spawn(move || worker_loop(rx, shared))
            .map_err(|e| anyhow!(e))?;

        Ok(Runtime { tap_runloop: rl, tap_thread: Some(tap_thread), worker: Some(worker), tx })
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
        let _ = self.tx.send(Msg::Stop);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        overlay::destroy();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Pending {
    window: MacWindow,
    rect: Rect,
    down: Point,
    checks: u32,
    /// Mouse went down on the title bar (away from the resize edges), so any
    /// frame change is a move. macOS itself resizes tiled windows when a drag
    /// starts, which would otherwise look like a resize.
    in_title_bar: bool,
}

/// Height of the strip at the top of a window treated as its title bar.
const TITLE_BAR_HEIGHT: i32 = 32;
/// Distance from the left/right edges where a press may be a resize.
const RESIZE_EDGE: i32 = 6;

fn in_title_bar(rect: &Rect, p: Point) -> bool {
    p.y >= rect.top + 2
        && p.y < rect.top + TITLE_BAR_HEIGHT
        && p.x >= rect.left + RESIZE_EDGE
        && p.x < rect.right - RESIZE_EDGE
}

struct Active {
    window: MacWindow,
    areas: Vec<WorkArea>,
    cursor: Point,
    flags: u64,
}

fn input(a: &Active) -> DragInput {
    let m = mods_from_flags(a.flags);
    DragInput { cursor: a.cursor, shift: m.shift, ctrl: m.ctrl }
}

fn worker_loop(rx: Receiver<Msg>, shared: Arc<Shared>) {
    let ws = NativeWindowSystem::new();
    let mut pending: Option<Pending> = None;
    let mut active: Option<Active> = None;

    let update = |shared: &Shared, a: &Active| {
        let st = shared.engine().drag_update(&a.areas, input(a));
        if let Some(st) = st {
            overlay::show(st);
        }
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Stop => break,
            Msg::MouseDown(p) => {
                active = None;
                shared.engine().drag_cancel();
                pending = MacWindow::at_point(p).and_then(|w| {
                    if w.is_fullscreen() {
                        return None;
                    }
                    let rect = w.element.frame().ok()?;
                    Some(Pending { window: w, rect, down: p, checks: 0, in_title_bar: in_title_bar(&rect, p) })
                });
            }
            Msg::MouseDragged(p, flags) => {
                if let Some(a) = &mut active {
                    a.cursor = p;
                    a.flags = flags;
                    update(&shared, a);
                    continue;
                }
                let Some(pd) = &mut pending else { continue };
                if (p.x - pd.down.x).abs() + (p.y - pd.down.y).abs() < 4 {
                    continue;
                }
                pd.checks += 1;
                let Ok(now) = pd.window.element.frame() else {
                    pending = None;
                    continue;
                };
                let resized = now.width() != pd.rect.width() || now.height() != pd.rect.height();
                let moved = now.left != pd.rect.left || now.top != pd.rect.top;
                if resized && !pd.in_title_bar {
                    pending = None; // resizing, not moving
                } else if moved || resized {
                    let pd = pending.take().unwrap();
                    let monitors = match screens::monitors() {
                        Ok(m) => m,
                        Err(e) => {
                            log::warn!("fancyzones: {e:#}");
                            continue;
                        }
                    };
                    let areas = {
                        let mut e = shared.engine();
                        let areas = e.work_areas(&monitors);
                        e.drag_start(pd.window.key(), pd.rect);
                        areas
                    };
                    let a = Active { window: pd.window, areas, cursor: p, flags };
                    update(&shared, &a);
                    active = Some(a);
                } else if pd.checks > 40 {
                    pending = None; // drag inside content, not a window move
                }
            }
            Msg::Flags(flags) => {
                if let Some(a) = &mut active {
                    a.flags = flags;
                    update(&shared, a);
                }
            }
            Msg::RightDown => {
                if let Some(a) = &active {
                    shared.engine().drag_toggle_secondary();
                    update(&shared, a);
                }
            }
            Msg::MouseUp => {
                pending = None;
                if let Some(a) = active.take() {
                    overlay::hide();
                    // Let the window server finish the user's move first.
                    std::thread::sleep(Duration::from_millis(40));
                    let mut e = shared.engine();
                    match e.drag_end(&ws, &a.window, &a.areas) {
                        Ok(DragResult::Zones(z)) => log::debug!("fancyzones: snapped to zones {z:?}"),
                        Ok(DragResult::Edge(a, r)) => log::debug!("fancyzones: edge snap {a:?} -> {r:?}"),
                        Ok(DragResult::Moved) => {}
                        Err(err) => log::warn!("fancyzones: snapping failed: {err:#}"),
                    }
                    shared.persist(&e.config);
                }
            }
            Msg::Hotkey(action, dir) => {
                let mut e = shared.engine();
                let r = match action {
                    Action::Snap => e.snap_focused(&ws, dir),
                    Action::Extend => e.extend_focused(&ws, dir),
                };
                match r {
                    Ok(true) => shared.persist(&e.config),
                    Ok(false) => {}
                    Err(err) => log::warn!("fancyzones: keyboard snap failed: {err:#}"),
                }
            }
        }
    }
}
