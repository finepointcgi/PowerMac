#![allow(non_upper_case_globals)]
//! macOS runtime: an event-tap thread, a worker thread and the panel on the
//! main thread.
//!
//! * **Tap thread** — an active session `CGEventTap` (mouse buttons,
//!   moves, scroll wheel, key-downs) on its own run loop. It never calls
//!   Accessibility: it decides from state the worker publishes whether an
//!   event is aimed at the panel (swallowed and forwarded to the worker) or
//!   is a Dock click that click-to-cycle replaces. Everything else passes.
//! * **Worker thread** — reads the Dock's AX tree (every 80 ms while the
//!   pointer is near the Dock or the panel is open, every second
//!   otherwise), polls the pointer at ~30 Hz near the Dock, runs the
//!   [`Hover`] state machine, lists the hovered app's windows, captures
//!   thumbnails and performs focus / close / minimize.
//! * **Main thread** — the panel ([`super::panel`]), reached through the
//!   main dispatch queue; the host app must run `NSApplication`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use objc2::MainThreadMarker;

use super::apps;
use super::ax;
use super::dock;
use super::ffi::*;
use super::focus::{self, Action};
use super::panel::{self, Model, TileView};
use super::thumbs;
use super::windows::{self, AppWindow, Elements};
use crate::cycle::{self, CycleAction, Win};
use crate::geom::{Dock, Orientation, RectF};
use crate::hover::{Effect, Hover, Target, Timing};
use crate::layout::{Hit, Layout};
use crate::placement::{self, place};
use crate::Config;

/// Space kept free for the menu bar (notched screens have a 37-pt bar).
const MENU_BAR: f64 = 40.0;
/// Fast polling while the pointer is this close to the Dock.
const ZONE_MARGIN: f64 = 80.0;
const FAST: Duration = Duration::from_millis(33);
const SLOW: Duration = Duration::from_millis(250);
const DOCK_FAST: Duration = Duration::from_millis(80);
const DOCK_SLOW: Duration = Duration::from_millis(1000);
/// Live-ish refresh of the open panel (window list + thumbnails).
const REFRESH: Duration = Duration::from_millis(1000);
/// Window lists cached for click-to-cycle stay valid this long.
const CACHE_TTL: Duration = Duration::from_secs(5);

const KEY_ESC: i64 = 53;
const KEY_W: i64 = 13;
const KEY_M: i64 = 46;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Esc,
    W,
    M,
}

enum Msg {
    Wake,
    Click(f64, f64),
    Middle(f64, f64),
    Scroll { delta: f64, continuous: bool },
    Key(Key),
    Dismiss,
    Cycle { path: String, pid: i32, action: CycleAction },
    Config,
    Stop,
}

/// What the open panel shows (shared with the tap).
#[derive(Debug, Clone)]
struct PanelInfo {
    key: String,
    frame: RectF,
    bridge: RectF,
    icon: RectF,
    layout: Layout,
    windows: Vec<AppWindow>,
    hover: Hit,
    selected: Option<usize>,
}

/// An app's windows, for click-to-cycle.
#[derive(Debug, Clone)]
struct AppCache {
    pid: i32,
    windows: Vec<AppWindow>,
    focused: Option<u32>,
    at: Instant,
}

struct Shared {
    cfg: Mutex<Config>,
    dock: Mutex<Option<Dock>>,
    /// Where the pointer is "near the Dock".
    zone: Mutex<RectF>,
    panel: Mutex<Option<PanelInfo>>,
    apps: Mutex<HashMap<String, AppCache>>,
    tx: Mutex<Sender<Msg>>,
    port: AtomicPtr<c_void>,
    tap_active: AtomicBool,
    stopping: AtomicBool,
    was_near: AtomicBool,
    swallow_left: AtomicBool,
    swallow_right: AtomicBool,
    swallow_other: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn send(&self, m: Msg) {
        let _ = lock(&self.tx).send(m);
    }
    fn cfg(&self) -> Config {
        lock(&self.cfg).clone()
    }
    fn panel_frame(&self) -> Option<(RectF, RectF, Hit)> {
        lock(&self.panel).as_ref().map(|p| (p.frame, p.icon, p.hover))
    }
}

fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            f(mtm)
        }
    });
}

// ------------------------------------------------------------ event tap

struct TapCtx {
    shared: Arc<Shared>,
}

extern "C" fn tap_callback(_proxy: CGEventTapProxy, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const TapCtx) };
    let shared = &ctx.shared;
    if etype == kCGEventTapDisabledByTimeout || etype == kCGEventTapDisabledByUserInput {
        let port = shared.port.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }
    if unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUserData) } as usize == pm_input::INJECTED_TAG {
        return event;
    }
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle_event(shared, etype, event))).unwrap_or(false);
    if swallow {
        std::ptr::null()
    } else {
        event
    }
}

/// Click-to-cycle decision for a left click at (x, y), from cached state.
fn cycle_decision(shared: &Shared, x: f64, y: f64) -> Option<(String, i32, CycleAction)> {
    let path = lock(&shared.dock).as_ref()?.app_at(x, y)?.path.clone();
    let entry = lock(&shared.apps).get(&path).cloned()?;
    if entry.at.elapsed() > CACHE_TTL {
        return None;
    }
    let front = apps::frontmost_pid() == Some(entry.pid);
    let wins: Vec<Win> = entry.windows.iter().filter(|w| !w.other_space).map(|w| Win { id: w.wid as u64, minimized: w.minimized }).collect();
    let action = cycle::decide(front, &wins, entry.focused.map(|f| f as u64))?;
    Some((path, entry.pid, action))
}

fn handle_event(shared: &Arc<Shared>, etype: u32, event: CGEventRef) -> bool {
    let p = unsafe { CGEventGetLocation(event) };
    let (x, y) = (p.x, p.y);
    let field = |f: u32| unsafe { CGEventGetIntegerValueField(event, f) };
    match etype {
        kCGEventMouseMoved | kCGEventLeftMouseDragged => {
            let near = shared.panel_frame().is_some() || lock(&shared.zone).contains(x, y);
            // Wake the worker near the Dock, and once more on leaving.
            if near || shared.was_near.load(Ordering::Relaxed) {
                shared.send(Msg::Wake);
            }
            shared.was_near.store(near, Ordering::Relaxed);
            false
        }
        kCGEventLeftMouseDown => {
            let panel = shared.panel_frame();
            if panel.is_some_and(|(f, _, _)| f.contains(x, y)) {
                shared.swallow_left.store(true, Ordering::SeqCst);
                shared.send(Msg::Click(x, y));
                return true;
            }
            if shared.cfg().click_to_cycle {
                if let Some((path, pid, action)) = cycle_decision(shared, x, y) {
                    shared.swallow_left.store(true, Ordering::SeqCst);
                    shared.send(Msg::Cycle { path, pid, action });
                    return true;
                }
            }
            shared.send(Msg::Dismiss);
            false
        }
        kCGEventLeftMouseUp => shared.swallow_left.swap(false, Ordering::SeqCst),
        kCGEventRightMouseDown => {
            if shared.panel_frame().is_some_and(|(f, _, _)| f.contains(x, y)) {
                shared.swallow_right.store(true, Ordering::SeqCst);
                return true;
            }
            shared.send(Msg::Dismiss);
            false
        }
        kCGEventRightMouseUp => shared.swallow_right.swap(false, Ordering::SeqCst),
        kCGEventOtherMouseDown => {
            if shared.panel_frame().is_some_and(|(f, _, _)| f.contains(x, y)) {
                shared.swallow_other.store(true, Ordering::SeqCst);
                if field(kCGMouseEventButtonNumber) == 2 {
                    shared.send(Msg::Middle(x, y));
                }
                return true;
            }
            shared.send(Msg::Dismiss);
            false
        }
        kCGEventOtherMouseUp => shared.swallow_other.swap(false, Ordering::SeqCst),
        kCGEventScrollWheel => {
            let Some((frame, icon, _)) = shared.panel_frame() else { return false };
            if !frame.contains(x, y) && !icon.contains(x, y) {
                return false;
            }
            let continuous = field(kCGScrollWheelEventIsContinuous) != 0;
            let delta = if continuous { field(kCGScrollWheelEventPointDeltaAxis1) } else { field(kCGScrollWheelEventDeltaAxis1) } as f64;
            if delta != 0.0 {
                shared.send(Msg::Scroll { delta, continuous });
            }
            true
        }
        kCGEventKeyDown => {
            let Some((_, _, hover)) = shared.panel_frame() else { return false };
            let code = field(kCGKeyboardEventKeycode);
            let flags = unsafe { CGEventGetFlags(event) };
            let plain = flags & (kCGEventFlagMaskCommand | kCGEventFlagMaskControl | kCGEventFlagMaskAlternate) == 0;
            let on_tile = matches!(hover, Hit::Tile(_) | Hit::Close(_) | Hit::Minimize(_));
            let key = match code {
                KEY_ESC => Some(Key::Esc),
                KEY_W if plain && on_tile => Some(Key::W),
                KEY_M if plain && on_tile => Some(Key::M),
                _ => None,
            };
            match key {
                Some(k) => {
                    shared.send(Msg::Key(k));
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

fn tap_loop(shared: Arc<Shared>, runloop_out: Arc<Mutex<usize>>) {
    let ctx = Box::into_raw(Box::new(TapCtx { shared: shared.clone() }));
    let mask = event_mask(kCGEventMouseMoved)
        | event_mask(kCGEventLeftMouseDragged)
        | event_mask(kCGEventLeftMouseDown)
        | event_mask(kCGEventLeftMouseUp)
        | event_mask(kCGEventRightMouseDown)
        | event_mask(kCGEventRightMouseUp)
        | event_mask(kCGEventOtherMouseDown)
        | event_mask(kCGEventOtherMouseUp)
        | event_mask(kCGEventScrollWheel)
        | event_mask(kCGEventKeyDown);
    let mut warned = false;
    while !shared.stopping.load(Ordering::SeqCst) {
        let port = unsafe {
            CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, tap_callback, ctx as *mut c_void)
        };
        if port.is_null() {
            if !warned {
                log::warn!("dock-previews: can't create the event tap (Accessibility permission missing?); retrying");
                warned = true;
            }
            for _ in 0..20 {
                if shared.stopping.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            continue;
        }
        unsafe {
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            *lock(&runloop_out) = rl as usize;
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            shared.port.store(port as *mut c_void, Ordering::SeqCst);
            shared.tap_active.store(true, Ordering::SeqCst);
            log::info!("dock-previews: event tap active");
            while !shared.stopping.load(Ordering::SeqCst) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0);
            }
            shared.tap_active.store(false, Ordering::SeqCst);
            shared.port.store(std::ptr::null_mut(), Ordering::SeqCst);
            CGEventTapEnable(port, false);
            CFMachPortInvalidate(port);
            CFRelease(src);
            CFRelease(port);
        }
        break;
    }
    drop(unsafe { Box::from_raw(ctx) });
}

// ------------------------------------------------------------- worker

struct Worker {
    shared: Arc<Shared>,
    hover: Hover<String>,
    el: Elements,
    dock: Option<Dock>,
    dock_at: Option<Instant>,
    last_refresh: Instant,
    prefetched: Option<(String, Instant)>,
    scroll_acc: f64,
    pointer: (f64, f64),
}

fn badge(w: &AppWindow) -> Option<&'static str> {
    if w.minimized {
        Some("Minimized")
    } else if w.app_hidden {
        Some("Hidden")
    } else if w.fullscreen && w.other_space {
        Some("Full Screen")
    } else if w.other_space {
        Some("Other Space")
    } else {
        None
    }
}

/// The usable area of the Dock's screen and the size the panel may take.
fn bounds_for(d: &Dock) -> (RectF, f64, f64) {
    let s = d.screen;
    let area = RectF::new(s.x, s.y + MENU_BAR, s.w, (s.h - MENU_BAR).max(100.0));
    let band = d.band();
    let m = placement::MARGIN;
    let g = placement::GAP;
    let (mw, mh) = match d.orientation {
        Orientation::Bottom => (area.w - 2.0 * m, band.y - g - area.y - m),
        Orientation::Left => (area.right() - band.right() - g - m, area.h - 2.0 * m),
        Orientation::Right => (band.x - g - area.x - m, area.h - 2.0 * m),
    };
    (area, mw.max(200.0), mh.max(150.0))
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        let cfg = shared.cfg();
        Worker {
            shared,
            hover: Hover::new(Timing::new(cfg.hover_delay())),
            el: Elements::default(),
            dock: None,
            dock_at: None,
            last_refresh: Instant::now(),
            prefetched: None,
            scroll_acc: 0.0,
            pointer: (0.0, 0.0),
        }
    }

    fn shown(&self) -> bool {
        lock(&self.shared.panel).is_some()
    }

    fn refresh_dock(&mut self) {
        self.dock = dock::read();
        self.dock_at = Some(Instant::now());
        *lock(&self.shared.zone) = self.dock.as_ref().map(|d| d.zone(ZONE_MARGIN)).unwrap_or_default();
        *lock(&self.shared.dock) = self.dock.clone();
    }

    fn icon_under_pointer(&self) -> Option<String> {
        let (x, y) = self.pointer;
        self.dock.as_ref()?.app_at(x, y).map(|i| i.path.clone())
    }

    fn target(&self) -> Target<String> {
        let (x, y) = self.pointer;
        let panel = lock(&self.shared.panel).as_ref().map(|p| (p.frame, p.bridge));
        if panel.is_some_and(|(f, _)| f.contains(x, y)) {
            return Target::Panel;
        }
        if let Some(path) = self.icon_under_pointer() {
            return Target::Icon(path);
        }
        if panel.is_some_and(|(_, b)| b.contains(x, y)) {
            return Target::Bridge;
        }
        Target::Outside
    }

    fn apply(&mut self, fx: Vec<Effect<String>>) {
        for e in fx {
            match e {
                Effect::Show(key) => self.show(&key, true),
                Effect::Hide => self.hide_panel(),
            }
        }
    }

    fn hide_panel(&mut self) {
        if lock(&self.shared.panel).take().is_some() {
            on_main(panel::hide);
            // Keep thumbnails only for windows of recently hovered apps.
            let alive: std::collections::HashSet<u32> = {
                let mut apps = lock(&self.shared.apps);
                apps.retain(|_, c| c.at.elapsed() < Duration::from_secs(120));
                apps.values().flat_map(|c| c.windows.iter().map(|w| w.wid)).collect()
            };
            thumbs::retain(|w| alive.contains(&w));
        }
        self.scroll_acc = 0.0;
    }

    /// List (and cache) the windows of the app at `path`.
    fn list(&mut self, path: &str) -> Option<(apps::AppInfo, Vec<AppWindow>)> {
        let app = apps::app_for_path(path)?;
        let wins = windows::app_windows(&mut self.el, app.pid, app.hidden, true);
        let focused = windows::focused_window(app.pid);
        lock(&self.shared.apps).insert(path.to_string(), AppCache { pid: app.pid, windows: wins.clone(), focused, at: Instant::now() });
        Some((app, wins))
    }

    /// Show (or rebuild) the panel for the app at `key`.
    fn show(&mut self, key: &str, animate: bool) {
        let Some(d) = self.dock.clone() else { return self.hide_panel() };
        let Some(item) = d.items.iter().find(|i| i.path == key) else { return self.hide_panel() };
        let Some((app, wins)) = self.list(key) else { return self.hide_panel() };
        let cfg = self.shared.cfg();
        if wins.is_empty() || (wins.len() == 1 && !cfg.show_single_window) {
            return self.hide_panel();
        }
        let icon_mode = !thumbs::has_permission();
        let (area, max_w, max_h) = bounds_for(&d);
        let aspects: Vec<f64> = wins.iter().map(|w| w.aspect()).collect();
        let layout = Layout::compute(&aspects, cfg.preview_size, d.orientation, max_w, max_h);
        let pl = place(item.frame, d.band(), d.orientation, layout.w, layout.h, area);
        let (hover, selected) = {
            let old = lock(&self.shared.panel);
            match old.as_ref() {
                Some(o) if o.key == key && o.windows.len() == wins.len() => (o.hover, o.selected),
                _ => (Hit::None, None),
            }
        };
        let model = Model {
            pid: app.pid,
            app_name: if app.name.is_empty() { item.title.clone() } else { app.name.clone() },
            app_path: app.path.clone(),
            items: wins
                .iter()
                .map(|w| TileView {
                    wid: w.wid,
                    title: if w.title.trim().is_empty() { app.name.clone() } else { w.title.clone() },
                    aspect: w.aspect(),
                    badge: badge(w),
                    dimmed: w.minimized || w.app_hidden,
                })
                .collect(),
            layout: layout.clone(),
            frame: pl.panel,
            hover,
            selected,
            icon_mode,
        };
        *lock(&self.shared.panel) = Some(PanelInfo {
            key: key.to_string(),
            frame: pl.panel,
            bridge: pl.bridge,
            icon: item.frame,
            layout,
            windows: wins.clone(),
            hover,
            selected,
        });
        on_main(move |mtm| panel::show(mtm, model, animate));
        if !icon_mode {
            self.capture(&wins, true);
        }
        self.last_refresh = Instant::now();
    }

    fn capture(&self, wins: &[AppWindow], first: bool) {
        let reqs: Vec<thumbs::Request> = wins
            .iter()
            // A minimized window can't be captured live; keep its last image.
            .filter(|w| !w.minimized || thumbs::cached(w.wid).is_none())
            .map(|w| thumbs::Request {
                wid: w.wid,
                w: w.frame.w,
                h: w.frame.h,
                max_age: if first { Duration::from_millis(300) } else { Duration::from_millis(900) },
            })
            .collect();
        if reqs.is_empty() {
            return;
        }
        let _ = std::thread::Builder::new().name("dock-previews-capture".into()).spawn(move || {
            thumbs::capture(reqs, |wid, img| on_main(move |mtm| panel::set_thumbnail(mtm, wid, img)));
        });
    }

    /// Periodic refresh of the open panel: rebuild if the window list
    /// changed, else just recapture thumbnails.
    fn refresh_panel(&mut self) {
        self.last_refresh = Instant::now();
        let Some(info) = lock(&self.shared.panel).clone() else { return };
        let Some((_, wins)) = self.list(&info.key) else { return self.hide_panel() };
        let same = wins.len() == info.windows.len()
            && wins.iter().zip(&info.windows).all(|(a, b)| a.wid == b.wid && a.title == b.title && a.minimized == b.minimized && a.other_space == b.other_space);
        if same {
            if thumbs::has_permission() {
                self.capture(&wins, false);
            }
        } else if wins.is_empty() || (wins.len() == 1 && !self.shared.cfg().show_single_window) {
            let fx = self.hover.dismiss(self.icon_under_pointer());
            self.apply(fx);
            self.hide_panel();
        } else {
            self.show(&info.key, false);
        }
    }

    fn panel_hit(&self, x: f64, y: f64) -> Option<(Hit, PanelInfo)> {
        let info = lock(&self.shared.panel).clone()?;
        Some((info.layout.hit(x - info.frame.x, y - info.frame.y), info))
    }

    fn act_on(&mut self, info: &PanelInfo, i: usize, action: Action) {
        let Some(w) = info.windows.get(i) else { return };
        let el = self.el.get(w.wid);
        focus::act(action, el.as_ref());
        // Let the app react (close animation, minimize genie) before re-listing.
        std::thread::sleep(Duration::from_millis(if action == Action::Close { 250 } else { 450 }));
        self.refresh_panel();
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Wake | Msg::Stop => {}
            Msg::Config => {
                let cfg = self.shared.cfg();
                self.hover.set_timing(Timing::new(cfg.hover_delay()));
                let fx = self.hover.reset();
                self.apply(fx);
                self.hide_panel();
            }
            Msg::Click(x, y) => {
                let Some((hit, info)) = self.panel_hit(x, y) else { return };
                match hit {
                    Hit::Tile(i) => {
                        if let Some(w) = info.windows.get(i).cloned() {
                            let fx = self.hover.dismiss(None);
                            self.apply(fx);
                            self.hide_panel();
                            let el = self.el.get(w.wid);
                            focus::focus(&w, el.as_ref());
                        }
                    }
                    Hit::Close(i) => self.act_on(&info, i, Action::Close),
                    Hit::Minimize(i) => self.act_on(&info, i, Action::ToggleMinimize),
                    _ => {}
                }
            }
            Msg::Middle(x, y) => {
                if let Some((Hit::Tile(i) | Hit::Close(i) | Hit::Minimize(i), info)) = self.panel_hit(x, y) {
                    self.act_on(&info, i, Action::Close);
                }
            }
            Msg::Key(Key::Esc) | Msg::Dismiss => {
                // A click on the panel itself never gets here (swallowed).
                let on_icon = self.icon_under_pointer();
                let fx = self.hover.dismiss(on_icon);
                self.apply(fx);
                self.hide_panel();
            }
            Msg::Key(k) => {
                let Some(info) = lock(&self.shared.panel).clone() else { return };
                if let Hit::Tile(i) | Hit::Close(i) | Hit::Minimize(i) = info.hover {
                    self.act_on(&info, i, if k == Key::W { Action::Close } else { Action::ToggleMinimize });
                }
            }
            Msg::Scroll { delta, continuous } => {
                let step = if continuous {
                    self.scroll_acc += delta;
                    if self.scroll_acc.abs() < 24.0 {
                        return;
                    }
                    let s = self.scroll_acc.signum();
                    self.scroll_acc = 0.0;
                    s
                } else {
                    delta.signum()
                };
                let Some(info) = lock(&self.shared.panel).clone() else { return };
                let n = info.windows.len();
                if n == 0 {
                    return;
                }
                // Scrolling down moves forward. Without a selection the front
                // window (tile 0) counts as current.
                let cur = info.selected.map(|s| s as i64).unwrap_or(0);
                let next = (cur + if step < 0.0 { 1 } else { -1 }).rem_euclid(n as i64) as usize;
                if let Some(p) = lock(&self.shared.panel).as_mut() {
                    p.selected = Some(next);
                }
                let hover = info.hover;
                on_main(move |mtm| panel::set_state(mtm, hover, Some(next)));
                let w = info.windows[next].clone();
                let el = self.el.get(w.wid);
                focus::focus(&w, el.as_ref());
            }
            Msg::Cycle { path, pid, action } => {
                let fx = self.hover.dismiss(Some(path.clone()));
                self.apply(fx);
                self.hide_panel();
                let wins = lock(&self.shared.apps).get(&path).map(|c| c.windows.clone()).unwrap_or_default();
                match action {
                    CycleAction::Minimize(id) => {
                        let el = self.el.get(id as u32);
                        focus::act(Action::Minimize, el.as_ref());
                    }
                    CycleAction::Focus(id) => {
                        if let Some(w) = wins.iter().find(|w| w.wid as u64 == id) {
                            let el = self.el.get(w.wid);
                            focus::focus(w, el.as_ref());
                        }
                    }
                }
                log::debug!("dock-previews: click-to-cycle {action:?} for pid {pid}");
                std::thread::sleep(Duration::from_millis(300));
                self.list(&path);
            }
        }
    }

    fn step(&mut self) {
        let now = Instant::now();
        if let Some(p) = cursor_location() {
            self.pointer = (p.x, p.y);
        }
        let (x, y) = self.pointer;
        let shown = self.shown();
        let near = shown || self.dock.as_ref().is_none_or(|d| d.zone(ZONE_MARGIN).contains(x, y));
        let max_age = if near || self.hover.pending().is_some() { DOCK_FAST } else { DOCK_SLOW };
        if self.dock_at.is_none_or(|t| t.elapsed() >= max_age) {
            self.refresh_dock();
        }
        let target = self.target();
        let on_panel = matches!(target, Target::Panel | Target::Bridge);
        let fx = self.hover.pointer(now, target);
        self.apply(fx);

        // The Dock hid (auto-hide) while the pointer isn't on the panel.
        if self.shown() && !on_panel && self.dock.as_ref().is_some_and(|d| !d.is_visible()) {
            let fx = self.hover.dismiss(None);
            self.apply(fx);
            self.hide_panel();
        }

        // Prefetch the windows of the app waiting for the hover delay (also
        // what click-to-cycle decides from).
        if let Some(k) = self.hover.pending().cloned() {
            let fresh = self.prefetched.as_ref().is_some_and(|(p, t)| *p == k && t.elapsed() < Duration::from_secs(1));
            if !fresh {
                self.prefetched = Some((k.clone(), Instant::now()));
                if let Some((_, wins)) = self.list(&k) {
                    if thumbs::has_permission() {
                        self.capture(&wins, true);
                    }
                }
            }
        }

        // Hover highlight inside the panel.
        let upd = {
            let mut g = lock(&self.shared.panel);
            match g.as_mut() {
                Some(p) => {
                    let h = p.layout.hit(x - p.frame.x, y - p.frame.y);
                    let h = if matches!(h, Hit::None | Hit::Panel) { Hit::None } else { h };
                    (h != p.hover).then(|| {
                        p.hover = h;
                        (h, p.selected)
                    })
                }
                None => None,
            }
        };
        if let Some((h, sel)) = upd {
            on_main(move |mtm| panel::set_state(mtm, h, sel));
        }

        if self.shown() && self.last_refresh.elapsed() >= REFRESH {
            self.refresh_panel();
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        let mut warned = false;
        loop {
            let near = self.shown() || self.hover.pending().is_some() || {
                let (x, y) = self.pointer;
                self.dock.as_ref().is_some_and(|d| d.zone(ZONE_MARGIN).contains(x, y))
            };
            let mut timeout = if near { FAST } else { SLOW };
            if let Some(d) = self.hover.deadline() {
                timeout = timeout.min(d.saturating_duration_since(Instant::now()));
            }
            let mut msgs = Vec::new();
            match rx.recv_timeout(timeout) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(m) => msgs.push(m),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let mut stop = false;
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Stop => stop = true,
                    Msg::Wake => {}
                    m => msgs.push(m),
                }
            }
            if stop || self.shared.stopping.load(Ordering::SeqCst) {
                break;
            }
            if !ax::is_trusted() {
                if !warned {
                    log::warn!("dock-previews: Accessibility permission missing; waiting for it");
                    warned = true;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            warned = false;
            for m in msgs {
                self.handle(m);
            }
            self.step();
        }
        self.hide_panel();
    }
}

// ------------------------------------------------------------- runtime

struct Inner {
    shared: Arc<Shared>,
    tap_runloop: Arc<Mutex<usize>>,
    tap: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Inner {
    fn start(cfg: Config) -> Result<Inner> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            cfg: Mutex::new(cfg),
            dock: Mutex::new(None),
            zone: Mutex::new(RectF::default()),
            panel: Mutex::new(None),
            apps: Mutex::new(HashMap::new()),
            tx: Mutex::new(tx),
            port: AtomicPtr::new(std::ptr::null_mut()),
            tap_active: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            was_near: AtomicBool::new(false),
            swallow_left: AtomicBool::new(false),
            swallow_right: AtomicBool::new(false),
            swallow_other: AtomicBool::new(false),
        });
        let tap_runloop = Arc::new(Mutex::new(0usize));
        let tap = {
            let (s, rl) = (shared.clone(), tap_runloop.clone());
            std::thread::Builder::new().name("dock-previews-tap".into()).spawn(move || tap_loop(s, rl))?
        };
        let worker = {
            let s = shared.clone();
            std::thread::Builder::new().name("dock-previews-worker".into()).spawn(move || Worker::new(s).run(rx))?
        };
        Ok(Inner { shared, tap_runloop, tap: Some(tap), worker: Some(worker) })
    }

    fn stop(&mut self) {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.send(Msg::Stop);
        let rl = *lock(&self.tap_runloop);
        if rl != 0 {
            unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
        }
        if let Some(t) = self.tap.take() {
            let _ = t.join();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        *lock(&self.shared.panel) = None;
        if let Some(mtm) = MainThreadMarker::new() {
            panel::destroy(mtm);
        } else {
            on_main(panel::destroy);
        }
    }
}

pub(crate) struct Runtime {
    cfg: Config,
    inner: Option<Inner>,
}

impl Runtime {
    pub(crate) fn start(cfg: Config) -> Result<Self> {
        let inner = if cfg.enabled { Some(Inner::start(cfg.clone())?) } else { None };
        Ok(Runtime { cfg, inner })
    }

    pub(crate) fn update(&mut self, cfg: Config) {
        if cfg.enabled && self.inner.is_none() {
            match Inner::start(cfg.clone()) {
                Ok(i) => self.inner = Some(i),
                Err(e) => log::error!("dock-previews: can't start: {e:#}"),
            }
        } else if !cfg.enabled {
            if let Some(mut i) = self.inner.take() {
                i.stop();
            }
        } else if let Some(i) = &self.inner {
            *lock(&i.shared.cfg) = cfg.clone();
            i.shared.send(Msg::Config);
        }
        self.cfg = cfg;
    }

    pub(crate) fn config(&self) -> Config {
        self.cfg.clone()
    }

    pub(crate) fn is_shown(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| lock(&i.shared.panel).is_some())
    }

    pub(crate) fn is_input_active(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.shared.tap_active.load(Ordering::SeqCst))
    }

    pub(crate) fn stop(&mut self) {
        if let Some(mut i) = self.inner.take() {
            i.stop();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn permissions() -> crate::Permissions {
    crate::Permissions { accessibility: ax::is_trusted(), screen_recording: thumbs::has_permission() }
}
