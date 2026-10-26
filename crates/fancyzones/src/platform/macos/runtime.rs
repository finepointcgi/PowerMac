#![allow(non_upper_case_globals)]
//! Event tap thread + worker thread + window observer thread.
//!
//! * The **tap thread** owns a `CGEventTap` (session level, active so that
//!   hotkeys and the layout-switching wheel can be swallowed) on its own
//!   `CFRunLoop`. Its callback only classifies events and forwards them over
//!   a channel.
//! * The **worker thread** does all Accessibility calls and engine updates,
//!   posts overlay changes to the main queue, executes commands posted by the
//!   [`crate::Handle`] and polls (1 s) for display / work area / Space changes.
//! * The **observer thread** runs a `CFRunLoop` with one `AXObserver`
//!   (`kAXWindowCreatedNotification`) per regular app, refreshed every second
//!   from `NSWorkspace.runningApplications`, while "move new windows to their
//!   last zone" or "open windows on the active monitor" is enabled.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::ax::{has_permissions, regular_app_pids, AxElement, MacWindow, NativeWindowSystem};
use super::ffi::*;
use super::{overlay, screens};
use crate::config::Modifiers;
use crate::engine::{DragInput, DragResult, Engine, Monitor, SwitchFeedback, WorkArea, FLASH_ZONES_MS};
use crate::geometry::{Point, Rect};
use crate::hotkey_bits::{self, Action};
use crate::keys::{mac_keycode_digit, vk_to_mac_keycode};
use crate::navigation::Direction;
use crate::{Command, KeyAction, Shared, WindowSystem};

enum Msg {
    MouseDown(Point),
    MouseDragged(Point, u64),
    MouseUp,
    RightDown,
    MiddleDown,
    Wheel(bool),
    Flags(u64),
    Hotkey(Action, Direction),
    Key(KeyAction),
    RotationPreview(bool),
    Rotate(bool),
    WindowCreated(MacWindow),
    Wake,
    Stop,
}

struct TapContext {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    port: AtomicPtr<c_void>,
    rotation_held: Cell<bool>,
    wheel_acc: Cell<f64>,
}

fn mods_from_flags(f: u64) -> Modifiers {
    Modifiers {
        ctrl: f & kCGEventFlagMaskControl != 0,
        alt: f & kCGEventFlagMaskAlternate != 0,
        shift: f & kCGEventFlagMaskShift != 0,
        meta: f & kCGEventFlagMaskCommand != 0,
    }
}

/// Points of continuous (trackpad) scrolling that count as one wheel notch.
const CONTINUOUS_NOTCH: f64 = 40.0;

extern "C" fn tap_callback(_proxy: CGEventTapProxy, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const TapContext) };
    let field = |f: u32| unsafe { CGEventGetIntegerValueField(event, f) };
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
        kCGEventOtherMouseDown => {
            // 2 = middle; 3+ = "X" buttons, which FancyZones treats like the secondary button.
            let msg = if field(kCGMouseEventButtonNumber) == 2 { Msg::MiddleDown } else { Msg::RightDown };
            let _ = ctx.tx.send(msg);
        }
        kCGEventScrollWheel => {
            if ctx.shared.zones_shown.load(Ordering::Relaxed) && ctx.shared.keys().wheel {
                if field(kCGScrollWheelEventIsContinuous) == 0 {
                    let d = field(kCGScrollWheelEventDeltaAxis1);
                    if d != 0 {
                        let _ = ctx.tx.send(Msg::Wheel(d > 0));
                    }
                } else {
                    let mut acc = ctx.wheel_acc.get() + field(kCGScrollWheelEventPointDeltaAxis1) as f64;
                    while acc.abs() >= CONTINUOUS_NOTCH {
                        let up = acc > 0.0;
                        let _ = ctx.tx.send(Msg::Wheel(up));
                        acc -= if up { CONTINUOUS_NOTCH } else { -CONTINUOUS_NOTCH };
                    }
                    ctx.wheel_acc.set(acc);
                }
                return std::ptr::null(); // don't scroll the window below
            }
            ctx.wheel_acc.set(0.0);
        }
        kCGEventFlagsChanged => {
            let f = unsafe { CGEventGetFlags(event) };
            if ctx.rotation_held.get() {
                let keys = ctx.shared.keys();
                if keys.rotation.is_none_or(|h| h.modifiers != mods_from_flags(f)) {
                    ctx.rotation_held.set(false);
                    let _ = ctx.tx.send(Msg::RotationPreview(false));
                }
            }
            let _ = ctx.tx.send(Msg::Flags(f));
        }
        kCGEventKeyDown => {
            let code = field(kCGKeyboardEventKeycode);
            let m = mods_from_flags(unsafe { CGEventGetFlags(event) });
            let repeat = field(kCGKeyboardEventAutorepeat) != 0;
            let keys = ctx.shared.keys();
            let is_key = |vk: u16| vk_to_mac_keycode(vk) == Some(code as u16);
            // Monitor rotation: hold the chord, then Left / Right.
            if keys.is_rotation_chord(m, is_key) {
                if !ctx.rotation_held.get() {
                    ctx.rotation_held.set(true);
                    let _ = ctx.tx.send(Msg::RotationPreview(true));
                }
                return std::ptr::null();
            }
            if ctx.rotation_held.get() && (code == kVK_LeftArrow || code == kVK_RightArrow) {
                if !repeat {
                    let _ = ctx.tx.send(Msg::Rotate(code == kVK_LeftArrow));
                }
                return std::ptr::null();
            }
            let dir = match code {
                kVK_LeftArrow => Some(Direction::Left),
                kVK_RightArrow => Some(Direction::Right),
                kVK_UpArrow => Some(Direction::Up),
                kVK_DownArrow => Some(Direction::Down),
                _ => None,
            };
            if let Some(dir) = dir {
                let bits = ctx.shared.hotkeys.load(Ordering::Relaxed);
                if let Some(a) = hotkey_bits::classify(bits, m, dir.is_horizontal()) {
                    let _ = ctx.tx.send(Msg::Hotkey(a, dir));
                    return std::ptr::null(); // swallow
                }
            }
            if let Some(a) = keys.classify(m, is_key, mac_keycode_digit(code as u16)) {
                if !repeat {
                    let _ = ctx.tx.send(Msg::Key(a));
                }
                return std::ptr::null();
            }
        }
        kCGEventKeyUp if ctx.rotation_held.get() => {
            let code = field(kCGKeyboardEventKeycode);
            let keys = ctx.shared.keys();
            if keys.rotation.is_some_and(|h| vk_to_mac_keycode(h.code) == Some(code as u16)) {
                ctx.rotation_held.set(false);
                let _ = ctx.tx.send(Msg::RotationPreview(false));
                return std::ptr::null();
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
    observer: Option<JoinHandle<()>>,
    observer_stop: Arc<AtomicBool>,
    tx: Sender<Msg>,
    shared: Arc<Shared>,
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
        let ctx = Box::new(TapContext {
            tx: tx.clone(),
            shared: shared.clone(),
            port: AtomicPtr::new(std::ptr::null_mut()),
            rotation_held: Cell::new(false),
            wheel_acc: Cell::new(0.0),
        });

        let tap_thread = std::thread::Builder::new().name("fancyzones-tap".into()).spawn(move || unsafe {
            let ctx_ptr = Box::into_raw(ctx);
            let mask = event_mask(kCGEventLeftMouseDown)
                | event_mask(kCGEventLeftMouseUp)
                | event_mask(kCGEventLeftMouseDragged)
                | event_mask(kCGEventRightMouseDown)
                | event_mask(kCGEventOtherMouseDown)
                | event_mask(kCGEventScrollWheel)
                | event_mask(kCGEventFlagsChanged)
                | event_mask(kCGEventKeyDown)
                | event_mask(kCGEventKeyUp);
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

        {
            let wake_tx = std::sync::Mutex::new(tx.clone());
            *shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(move || {
                let _ = wake_tx.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::Wake);
            }));
        }

        let observer_stop = Arc::new(AtomicBool::new(false));
        let observer = {
            let (stop, tx, shared) = (observer_stop.clone(), tx.clone(), shared.clone());
            std::thread::Builder::new()
                .name("fancyzones-observer".into())
                .spawn(move || observer_loop(stop, tx, shared))
                .map_err(|e| anyhow!(e))?
        };

        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("fancyzones-worker".into())
                .spawn(move || worker_loop(rx, shared))
                .map_err(|e| anyhow!(e))?
        };

        Ok(Runtime {
            tap_runloop: rl,
            tap_thread: Some(tap_thread),
            worker: Some(worker),
            observer: Some(observer),
            observer_stop,
            tx,
            shared,
        })
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
        self.observer_stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.observer.take() {
            let _ = t.join();
        }
        *self.shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = self.tx.send(Msg::Stop);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        self.shared.zones_shown.store(false, Ordering::SeqCst);
        overlay::destroy();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

// --------------------------------------------------------------- observer

struct ObserverCtx {
    tx: Sender<Msg>,
    pid: i32,
}

struct AppObserver {
    observer: AXObserverRef,
    app: AxElement,
    ctx: *mut ObserverCtx,
}

const WINDOW_CREATED: &str = "AXWindowCreated";

impl Drop for AppObserver {
    fn drop(&mut self) {
        unsafe {
            let n = CfString::new(WINDOW_CREATED);
            AXObserverRemoveNotification(self.observer, self.app.raw(), n.0);
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(self.observer), kCFRunLoopDefaultMode);
            CFRelease(self.observer);
            drop(Box::from_raw(self.ctx));
        }
    }
}

extern "C" fn window_created_cb(_o: AXObserverRef, element: AXUIElementRef, _n: CFStringRef, refcon: *mut c_void) {
    let ctx = unsafe { &*(refcon as *const ObserverCtx) };
    if element.is_null() {
        return;
    }
    unsafe { CFRetain(element) };
    if let Some(el) = unsafe { AxElement::from_create(element) } {
        let _ = ctx.tx.send(Msg::WindowCreated(MacWindow { element: el, pid: ctx.pid }));
    }
}

fn observe(pid: i32, tx: &Sender<Msg>) -> Option<AppObserver> {
    unsafe {
        let mut obs: AXObserverRef = std::ptr::null();
        if AXObserverCreate(pid, window_created_cb, &mut obs) != kAXErrorSuccess || obs.is_null() {
            return None;
        }
        let app = AxElement::application(pid);
        let ctx = Box::into_raw(Box::new(ObserverCtx { tx: tx.clone(), pid }));
        let n = CfString::new(WINDOW_CREATED);
        if AXObserverAddNotification(obs, app.raw(), n.0, ctx as *mut c_void) != kAXErrorSuccess {
            CFRelease(obs);
            drop(Box::from_raw(ctx));
            return None;
        }
        CFRunLoopAddSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(obs), kCFRunLoopDefaultMode);
        Some(AppObserver { observer: obs, app, ctx })
    }
}

fn observer_loop(stop: Arc<AtomicBool>, tx: Sender<Msg>, shared: Arc<Shared>) {
    let mut observers: HashMap<i32, Option<AppObserver>> = HashMap::new();
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        let enabled = {
            let e = shared.engine();
            e.config.settings.app_last_zone_move_windows || e.config.settings.open_window_on_active_monitor
        };
        if enabled {
            let pids = regular_app_pids();
            observers.retain(|pid, _| pids.contains(pid));
            for pid in pids {
                if let std::collections::hash_map::Entry::Vacant(v) = observers.entry(pid) {
                    v.insert(observe(pid, &tx));
                    if !first {
                        // A newly launched app may have opened windows before
                        // we attached; report them (the engine ignores repeats).
                        for w in MacWindow::of_app(pid) {
                            let _ = tx.send(Msg::WindowCreated(w));
                        }
                    }
                }
            }
            first = false;
        } else {
            observers.clear();
            first = true;
        }
        let r = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0) };
        if r == 1 {
            // kCFRunLoopRunFinished: no sources yet
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    observers.clear();
}

// ----------------------------------------------------------------- worker

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
/// Delay before handling a newly created window (apps often place it first).
const NEW_WINDOW_DELAY: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

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

fn cursor() -> Option<Point> {
    cursor_location().map(|p| Point::new(p.x as i32, p.y as i32))
}

/// Monitor under the mouse cursor.
pub fn monitor_under_cursor() -> Option<String> {
    let c = cursor()?;
    let monitors = screens::monitors().ok()?;
    monitors.iter().find(|m| m.frame.contains(c)).or(monitors.first()).map(|m| m.id.clone())
}

fn show_feedback(fb: SwitchFeedback) {
    if let Some(f) = fb.flash {
        overlay::flash(f, fb.flash_ms.unwrap_or(FLASH_ZONES_MS));
    }
    if let Some(l) = fb.label {
        overlay::show_label(l);
    }
}

fn update_drag(shared: &Shared, e: &mut Engine, a: &Active) {
    let st = e.drag_update(&a.areas, input(a));
    shared.zones_shown.store(e.drag_zones_shown(), Ordering::Relaxed);
    if let Some(st) = st {
        overlay::show(st);
    }
}

/// Work areas changed under a running drag (layout switch): re-instantiate.
fn refresh_drag(shared: &Shared, ws: &NativeWindowSystem, e: &mut Engine, a: &mut Active) {
    if let Ok(areas) = e.areas(ws) {
        a.areas = areas;
    }
    e.drag_invalidate();
    update_drag(shared, e, a);
}

fn worker_loop(rx: Receiver<Msg>, shared: Arc<Shared>) {
    let ws = NativeWindowSystem::new();
    let mut pending: Option<Pending> = None;
    let mut active: Option<Active> = None;
    let mut new_windows: Vec<(Instant, MacWindow)> = Vec::new();
    let mut last_monitors: Vec<Monitor> = screens::monitors().unwrap_or_default();
    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut rotation_preview = false;
    let mut warned_transparency = false;

    loop {
        let now = Instant::now();
        let due = new_windows.iter().map(|(t, _)| *t).min().unwrap_or(next_poll).min(next_poll);
        let msg = match rx.recv_timeout(due.saturating_duration_since(now)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            None | Some(Msg::Wake) => {}
            Some(Msg::Stop) => break,
            Some(Msg::MouseDown(p)) => {
                active = None;
                shared.engine().drag_cancel();
                shared.zones_shown.store(false, Ordering::Relaxed);
                pending = MacWindow::at_point(p).and_then(|w| {
                    if w.is_fullscreen() {
                        return None;
                    }
                    let rect = w.element.frame().ok()?;
                    Some(Pending { window: w, rect, down: p, checks: 0, in_title_bar: in_title_bar(&rect, p) })
                });
            }
            Some(Msg::MouseDragged(p, flags)) => {
                if let Some(a) = &mut active {
                    a.cursor = p;
                    a.flags = flags;
                    update_drag(&shared, &mut shared.engine(), a);
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
                    let mut e = shared.engine();
                    if !e.is_processable(&ws, &pd.window) {
                        continue; // excluded app / dialog / panel
                    }
                    let areas = match e.areas(&ws) {
                        Ok(a) => a,
                        Err(err) => {
                            log::warn!("fancyzones: {err:#}");
                            continue;
                        }
                    };
                    e.drag_start(pd.window.key(), pd.rect);
                    if e.config.settings.make_dragged_window_transparent && !warned_transparency {
                        warned_transparency = true;
                        log::debug!("fancyzones: make_dragged_window_transparent is not supported on macOS");
                    }
                    let a = Active { window: pd.window, areas, cursor: p, flags };
                    update_drag(&shared, &mut e, &a);
                    active = Some(a);
                } else if pd.checks > 40 {
                    pending = None; // drag inside content, not a window move
                }
            }
            Some(Msg::Flags(flags)) => {
                if let Some(a) = &mut active {
                    a.flags = flags;
                    update_drag(&shared, &mut shared.engine(), a);
                }
            }
            Some(Msg::RightDown) => {
                if let Some(a) = &active {
                    let mut e = shared.engine();
                    e.drag_toggle_secondary();
                    update_drag(&shared, &mut e, a);
                }
            }
            Some(Msg::MiddleDown) => {
                if let Some(a) = &active {
                    let mut e = shared.engine();
                    e.drag_middle_click();
                    update_drag(&shared, &mut e, a);
                }
            }
            Some(Msg::Wheel(up)) => {
                if let Some(a) = &mut active {
                    let mut e = shared.engine();
                    match e.drag_wheel(&ws, &a.areas, up) {
                        Ok(Some(fb)) => {
                            refresh_drag(&shared, &ws, &mut e, a);
                            shared.persist(&e.config);
                            show_feedback(fb);
                        }
                        Ok(None) => {}
                        Err(err) => log::warn!("fancyzones: wheel layout switch failed: {err:#}"),
                    }
                }
            }
            Some(Msg::MouseUp) => {
                pending = None;
                shared.zones_shown.store(false, Ordering::Relaxed);
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
            Some(Msg::Hotkey(action, dir)) => {
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
            Some(Msg::Key(KeyAction::Editor)) => shared.open_editor(monitor_under_cursor()),
            Some(Msg::Key(k @ (KeyAction::NextTab | KeyAction::PrevTab))) => {
                if let Err(err) = shared.engine().cycle_windows(&ws, k == KeyAction::PrevTab) {
                    log::warn!("fancyzones: window switching failed: {err:#}");
                }
            }
            Some(Msg::Key(KeyAction::QuickLayout(k))) => {
                let fb = shared.run_command(&ws, Command::QuickLayout(k), cursor());
                if let Some(a) = &mut active {
                    refresh_drag(&shared, &ws, &mut shared.engine(), a);
                }
                show_feedback(fb);
            }
            Some(Msg::RotationPreview(on)) => {
                rotation_preview = on;
                if active.is_none() {
                    if on {
                        match shared.engine().rotation_preview(&ws) {
                            Ok(st) => overlay::show(st),
                            Err(err) => log::warn!("fancyzones: {err:#}"),
                        }
                    } else {
                        overlay::hide();
                    }
                }
            }
            Some(Msg::Rotate(reverse)) => {
                shared.run_command(&ws, Command::Rotate(reverse), None);
                if rotation_preview && active.is_none() {
                    // windows need a moment to settle before re-reading frames
                    std::thread::sleep(Duration::from_millis(60));
                    if let Ok(st) = shared.engine().rotation_preview(&ws) {
                        overlay::show(st);
                    }
                }
            }
            Some(Msg::WindowCreated(w)) => new_windows.push((Instant::now() + NEW_WINDOW_DELAY, w)),
        }
        // new windows whose delay elapsed
        let now = Instant::now();
        if new_windows.iter().any(|(t, _)| *t <= now) {
            let (ready, rest): (Vec<_>, Vec<_>) = new_windows.drain(..).partition(|(t, _)| *t <= now);
            new_windows = rest;
            let c = cursor();
            let mut e = shared.engine();
            for (_, w) in ready {
                match e.on_window_created(&ws, &w, c) {
                    Ok(true) => {
                        log::debug!("fancyzones: placed new window of pid {}", w.pid);
                        shared.persist(&e.config);
                    }
                    Ok(false) => {}
                    Err(err) => log::debug!("fancyzones: new window: {err:#}"),
                }
            }
        }

        // display / work area / Space changes
        if now >= next_poll {
            next_poll = now + POLL_INTERVAL;
            if active.is_none() && pending.is_none() {
                if let Ok(m) = screens::monitors() {
                    let mut e = shared.engine();
                    if m != last_monitors && !last_monitors.is_empty() {
                        log::info!("fancyzones: display configuration changed");
                        match e.on_display_change(&ws) {
                            Ok(n) if n > 0 => log::info!("fancyzones: moved {n} windows back into their zones"),
                            Ok(_) => {}
                            Err(err) => log::warn!("fancyzones: {err:#}"),
                        }
                    } else {
                        let vds = ws.virtual_desktops(&m);
                        e.set_virtual_desktops(vds, &m);
                    }
                    last_monitors = m;
                }
            }
        }

        // commands posted through the Handle
        for cmd in shared.take_commands() {
            let fb = shared.run_command(&ws, cmd, cursor());
            if let Some(a) = &mut active {
                refresh_drag(&shared, &ws, &mut shared.engine(), a);
            }
            show_feedback(fb);
        }
    }
}
