#![allow(non_upper_case_globals)]
//! macOS runtime: a subscriber of the shared event tap, a worker thread and
//! the overlay on the main thread.
//!
//! * **Tap** — a `pm-hook` subscription at
//!   [`pm_hook::priority::WINDOW_SWITCHER`] (after Keyboard Manager, Mouse
//!   Without Borders and the text expander, like the session-level tap it
//!   replaces). Key, modifier and mouse events go straight into the
//!   [`Engine`] (a mutex held for microseconds) so the handler can decide
//!   synchronously whether to swallow them. Opening the switcher takes the
//!   window list from the worker's cache, so it never waits on
//!   Accessibility. The tap's health (`pm-hook` status) drives the Cmd+Tab
//!   take-over: the system switcher comes back whenever the tap is missing,
//!   disabled or blind (Secure Input).
//! * **Worker thread** — keeps the cached window list fresh (full
//!   enumeration every 2 s and on demand), tracks the focused window 4×/s
//!   for most-recently-used ordering, and performs focus changes and
//!   window actions (all Accessibility calls happen here).
//! * **Main thread** — the overlay (see [`super::overlay`]); reached
//!   through the main dispatch queue, so the host app must run
//!   `NSApplication` (PowerMac's tray app does).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use objc2::MainThreadMarker;

use super::apps;
use super::ax;
use super::cmdtab;
use super::ffi::*;
use super::focus;
use super::overlay::{self, ItemView, Model};
use super::spotlight::{self, PreviewView};
use super::thumbs;
use super::windows::{self, Elements};
use crate::config::{Config, Mods};
use crate::engine::{Engine, Snapshot};
use crate::guard::Guard;
use crate::placement;
use crate::model::{self, FilterContext, Mode, Mru, RectF, WindowInfo};
use crate::switcher::{Effect, WindowAction};

enum Msg {
    Commit(WindowInfo),
    Act(WindowAction, WindowInfo),
    Refresh,
    Stop,
}

pub(crate) struct Shared {
    engine: Mutex<Engine>,
    /// All switchable windows, most recent first (unfiltered).
    windows: Mutex<Vec<WindowInfo>>,
    tx: Mutex<Sender<Msg>>,
    shown: AtomicBool,
    /// The shared tap is active (from its status callback).
    tap_active: AtomicBool,
    stopping: AtomicBool,
    /// A left-click was swallowed; swallow its mouse-up too.
    swallow_mouse_up: AtomicBool,
    /// The "can't create the tap" warning was logged.
    warned: AtomicBool,
    /// System Cmd+Tab take-over (see [`crate::guard`]).
    guard: Mutex<Guard>,
    /// Tests: pretend the keyboard hook stopped working.
    debug_broken: AtomicBool,
}

impl Shared {
    fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn send(&self, m: Msg) {
        let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(m);
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

// ------------------------------------------------------------ snapshot

fn screens() -> Vec<RectF> {
    display_bounds().into_iter().map(|r| RectF::new(r.origin.x, r.origin.y, r.size.width, r.size.height)).collect()
}

/// Build the list for `mode` from the cached windows (tap thread: no AX).
fn snapshot(shared: &Shared, cfg: &Config, mode: Mode, cursor: (f64, f64)) -> Snapshot {
    let mut all = shared.windows.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let front = apps::frontmost_pid().map(|p| p as i64);
    // The frontmost app's most recent window goes first even if the focus
    // poll hasn't caught up yet.
    if let Some(fp) = front {
        if all.first().is_some_and(|w| w.pid != fp) {
            if let Some(k) = all.iter().position(|w| w.pid == fp && !w.minimized && !w.app_hidden) {
                let w = all.remove(k);
                all.insert(0, w);
            }
        }
    }
    let scr = screens();
    let cur = scr.iter().position(|s| s.contains(cursor.0, cursor.1)).or(if scr.is_empty() { None } else { Some(0) });
    let ctx = FilterContext { active_pid: front, screens: scr.clone(), current_screen: cur };
    let items = model::filter(all, cfg, mode, &ctx);
    let screen = cur.and_then(|i| scr.get(i).copied()).unwrap_or(RectF::new(0.0, 0.0, 1440.0, 900.0));
    Snapshot { items, screen, icon_mode: !cfg.thumbnails || !thumbs::has_permission() }
}

fn model_of(e: &Engine) -> Model {
    Model {
        items: e.items().iter().map(ItemView::from_window).collect(),
        layout: e.layout().clone(),
        selected: e.selected(),
        icon_mode: e.icon_mode(),
    }
}

/// Show the preview of the selected window (if on) and fetch a sharp image
/// of it and its neighbours.
fn show_preview(eng: &Engine) {
    let cfg = eng.config();
    if !cfg.preview_selected_window || !eng.is_shown() {
        return;
    }
    let Some(w) = eng.selected_item() else {
        on_main(spotlight::hide);
        return;
    };
    let outline = !thumbs::has_permission();
    let view = PreviewView {
        wid: w.id as u32,
        pid: w.pid as i32,
        title: w.display_title().to_string(),
        app_path: w.app_path.clone(),
        placement: placement::place(w, eng.screen(), &screens(), cfg.only_current_screen),
        dim: cfg.effective_dim(),
        outline,
        avoid: Some(eng.layout().panel),
    };
    on_main(move |mtm| spotlight::show(mtm, view));
    if outline {
        return;
    }
    // The selected window first, then the next ones Tab would reach.
    let n = eng.items().len();
    let sel = eng.selected();
    let reqs: Vec<thumbs::Request> = [sel, (sel + 1) % n.max(1), (sel + n.max(1) - 1) % n.max(1)]
        .into_iter()
        .filter_map(|i| eng.item(i))
        .filter(|w| !w.minimized)
        .map(|w| thumbs::Request::full(w.id as u32, w.frame.w, w.frame.h))
        .fold(Vec::new(), |mut v, r| {
            if !v.iter().any(|x: &thumbs::Request| x.wid == r.wid) {
                v.push(r);
            }
            v
        });
    if reqs.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("window-switcher-preview".into()).spawn(move || {
        thumbs::capture(reqs, |wid, img| on_main(move |mtm| spotlight::set_image(mtm, wid, img)));
    });
}

fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            f(mtm)
        }
    });
}

/// Perform effects (called with the engine locked; everything here is
/// cheap or asynchronous).
fn perform(shared: &Arc<Shared>, eng: &Engine, fx: Vec<Effect>) {
    for e in fx {
        match e {
            Effect::Open(_) => {
                shared.send(Msg::Refresh);
                schedule_tick(shared, eng);
                if !eng.icon_mode() {
                    request_thumbnails(eng.items());
                }
            }
            Effect::Show => {
                shared.shown.store(true, Ordering::SeqCst);
                let m = model_of(eng);
                on_main(move |mtm| overlay::show(mtm, m));
                show_preview(eng);
            }
            Effect::Select(i) => {
                if eng.is_shown() {
                    on_main(move |mtm| overlay::select(mtm, i));
                    show_preview(eng);
                }
            }
            Effect::Commit(i) => {
                hide(shared);
                if let Some(w) = eng.item(i) {
                    shared.send(Msg::Commit(w.clone()));
                }
            }
            Effect::Cancel => hide(shared),
            Effect::Act(a, i) => {
                if let Some(w) = eng.item(i) {
                    shared.send(Msg::Act(a, w.clone()));
                }
            }
        }
    }
}

fn hide(shared: &Shared) {
    if shared.shown.swap(false, Ordering::SeqCst) {
        on_main(overlay::hide);
    }
}

fn schedule_tick(shared: &Arc<Shared>, eng: &Engine) {
    let Some(deadline) = eng.show_deadline() else { return };
    let shared = shared.clone();
    let _ = std::thread::Builder::new().name("window-switcher-delay".into()).spawn(move || {
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        let mut eng = shared.engine();
        let fx = eng.tick(Instant::now());
        perform(&shared, &eng, fx);
    });
}

fn request_thumbnails(items: &[WindowInfo]) {
    let reqs: Vec<thumbs::Request> = items
        .iter()
        .map(|w| thumbs::Request::thumbnail(w.id as u32, w.frame.w, w.frame.h))
        .collect();
    if reqs.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("window-switcher-capture".into()).spawn(move || {
        thumbs::capture(reqs, |wid, img| {
            on_main(move |mtm| {
                overlay::set_thumbnail(mtm, wid, img.clone());
                // The preview waits for a capture when nothing was cached.
                if spotlight::current() == Some(wid) {
                    spotlight::set_image(mtm, wid, img);
                }
            })
        });
    });
}

// ------------------------------------------------------------ event tap

fn handle_event(shared: &Arc<Shared>, etype: u32, event: CGEventRef) -> bool {
    let field = |f: u32| unsafe { CGEventGetIntegerValueField(event, f) };
    let now = Instant::now();
    match etype {
        kCGEventKeyDown | kCGEventKeyUp => {
            let code = field(kCGKeyboardEventKeycode) as u16;
            let Some((vk, _)) = pm_input::vk::mac_to_vk(code) else {
                // Unknown key: swallow while the switcher is open.
                return shared.engine().is_active();
            };
            let mut eng = shared.engine();
            if etype == kCGEventKeyUp {
                return eng.key_up(vk).swallow;
            }
            let mods = mods_from_flags(unsafe { CGEventGetFlags(event) });
            let p = unsafe { CGEventGetLocation(event) };
            let cfg = eng.config().clone();
            let o = eng.key_down(vk, mods, now, &mut |mode| snapshot(shared, &cfg, mode, (p.x, p.y)));
            perform(shared, &eng, o.effects);
            o.swallow
        }
        kCGEventFlagsChanged => {
            let mods = mods_from_flags(unsafe { CGEventGetFlags(event) });
            let mut eng = shared.engine();
            if eng.is_active() {
                let fx = eng.modifiers_changed(mods);
                perform(shared, &eng, fx);
            }
            false
        }
        kCGEventMouseMoved => {
            if shared.shown.load(Ordering::Relaxed) {
                let p = unsafe { CGEventGetLocation(event) };
                let mut eng = shared.engine();
                let fx = eng.hover(p.x, p.y);
                perform(shared, &eng, fx);
            }
            false
        }
        kCGEventLeftMouseDown => {
            if !shared.shown.load(Ordering::Relaxed) {
                return false;
            }
            let p = unsafe { CGEventGetLocation(event) };
            let mut eng = shared.engine();
            let o = eng.click(p.x, p.y);
            perform(shared, &eng, o.effects);
            shared.swallow_mouse_up.store(o.swallow, Ordering::SeqCst);
            o.swallow
        }
        kCGEventLeftMouseUp => shared.swallow_mouse_up.swap(false, Ordering::SeqCst),
        _ => false,
    }
}

/// Subscribe to the shared event tap. Its status callback keeps
/// `tap_active` current and re-checks the Cmd+Tab take-over on every change
/// (tap created, disabled / re-enabled, permission lost, Secure Input).
fn subscribe(shared: &Arc<Shared>) -> pm_hook::Guard {
    use pm_hook::{EventMask, EventType, Subscription, Verdict};
    let mask = EventMask::of(&[
        EventType::KeyDown,
        EventType::KeyUp,
        EventType::FlagsChanged,
        EventType::MouseMoved,
        EventType::LeftMouseDown,
        EventType::LeftMouseUp,
    ]);
    let (s1, s2) = (shared.clone(), shared.clone());
    pm_hook::subscribe(
        Subscription::new("window-switcher", pm_hook::priority::WINDOW_SWITCHER, mask, move |ev| {
            let (etype, event) = (ev.cg_type(), ev.cg_event());
            let swallow =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle_event(&s1, etype, event))).unwrap_or(false);
            if swallow {
                Verdict::Swallow
            } else {
                Verdict::Pass
            }
        })
        // Mouse Without Borders' injected events are not ours to interpret.
        .ignore_tags(&[pm_input::INJECTED_TAG])
        .on_status(move |st| {
            let active = st.is_active() && !s2.stopping.load(Ordering::SeqCst);
            let was = s2.tap_active.swap(active, Ordering::SeqCst);
            if active && !was {
                log::info!("window-switcher: event tap active");
            } else if !active && was {
                log::warn!("window-switcher: event tap unavailable ({:?})", st.state);
            } else if !active && !s2.warned.swap(true, Ordering::SeqCst) {
                log::warn!("window-switcher: can't create the event tap (Accessibility permission missing?); retrying");
            }
            // Only while Cmd+Tab can reach us may the system's go.
            sync_cmd_tab(&s2);
        }),
    )
}

// ------------------------------------------------------------- worker

const ENUM_INTERVAL: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(250);

fn worker_loop(shared: Arc<Shared>, rx: Receiver<Msg>) {
    let own_pid = std::process::id() as i32;
    let mut el = Elements::default();
    let mut mru = Mru::new();
    mru.seed(windows::onscreen_order().into_iter().map(|w| w as u64));
    let mut list: Vec<WindowInfo> = Vec::new();
    let mut last_enum: Option<Instant> = None;
    // After a switch the app may report its old focused window for a moment.
    let mut poll_after = Instant::now();
    loop {
        let mut force = false;
        let mut changed = false;
        match rx.recv_timeout(POLL) {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::Commit(w)) => {
                let e = el.get(w.id as u32);
                focus::focus(&w, e.as_ref());
                mru.touch(w.id);
                changed = true;
                poll_after = Instant::now() + Duration::from_millis(400);
                force = w.minimized || w.app_hidden;
            }
            Ok(Msg::Act(a, w)) => {
                let e = el.get(w.id as u32);
                focus::act(a, &w, e.as_ref());
                // Let the app react (close animation, minimize genie) before re-listing.
                std::thread::sleep(Duration::from_millis(if a == WindowAction::QuitApp { 600 } else { 250 }));
                force = true;
            }
            Ok(Msg::Refresh) => force = true,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        // Tap disabled, Secure Input on…: hand Cmd+Tab back to macOS.
        sync_cmd_tab(&shared);
        if !ax::is_trusted() {
            continue;
        }
        if Instant::now() < poll_after {
            // skip
        } else if let Some((_, wid)) = windows::focused_window() {
            if mru.most_recent() != Some(wid as u64) {
                mru.touch(wid as u64);
                changed = true;
            }
        }
        let due = last_enum.is_none_or(|t| t.elapsed() >= ENUM_INTERVAL);
        let enumerated = force || due;
        if enumerated {
            let other_spaces = shared.engine().config().show_other_spaces;
            list = windows::enumerate(&mut el, other_spaces, own_pid);
            last_enum = Some(Instant::now());
            let ids: std::collections::HashSet<u64> = list.iter().map(|w| w.id).collect();
            mru.retain(|w| ids.contains(&w));
            mru.seed(list.iter().map(|w| w.id));
            thumbs::retain(|w| ids.contains(&(w as u64)));
            changed = true;
        }
        if changed {
            let mut sorted = list.clone();
            mru.sort(&mut sorted);
            *shared.windows.lock().unwrap_or_else(|e| e.into_inner()) = sorted;
        }
        if enumerated {
            push_refresh(&shared);
        }
    }
}

/// While the switcher is open, merge the fresh list into it.
fn push_refresh(shared: &Arc<Shared>) {
    let mut eng = shared.engine();
    let Some(mode) = eng.switcher().mode() else { return };
    let cfg = eng.config().clone();
    let cursor = cursor_location().map(|p| (p.x, p.y)).unwrap_or_default();
    let snap = snapshot(shared, &cfg, mode, cursor);
    if snap.icon_mode != eng.icon_mode() {
        return;
    }
    let (changed, fx) = eng.refresh(snap.items);
    if changed && eng.is_shown() {
        let m = model_of(&eng);
        on_main(move |mtm| overlay::show(mtm, m));
        if !eng.icon_mode() {
            request_thumbnails(eng.items());
        }
        show_preview(&eng);
    } else {
        perform(shared, &eng, fx);
    }
}

// ------------------------------------------------- Cmd+Tab replacement

/// Whether keystrokes reach our tap right now.
fn tap_healthy(shared: &Shared) -> bool {
    if !shared.tap_active.load(Ordering::SeqCst) || shared.stopping.load(Ordering::SeqCst) || shared.debug_broken.load(Ordering::SeqCst) {
        return false;
    }
    // Checks `CGEventTapIsEnabled` (re-enabling a silently disabled tap once).
    pm_hook::is_healthy() && !cmdtab::secure_input_on()
}

/// Turn the system's Cmd+Tab off only while it's replaced *and* our tap
/// works; back on otherwise (see [`crate::guard`]).
fn sync_cmd_tab(shared: &Shared) {
    let (tab, backtick) = {
        let eng = shared.engine();
        let c = eng.config();
        (c.replaces_system_switcher(), c.replaces_cmd_backtick())
    };
    let wanted = Guard::wanted(tab, backtick, tap_healthy(shared));
    let mut g = shared.guard.lock().unwrap_or_else(|e| e.into_inner());
    g.sync(wanted, &mut cmdtab::SysBackend);
}

// ------------------------------------------------------------- runtime

pub(crate) struct Runtime {
    shared: Arc<Shared>,
    hook: Option<pm_hook::Guard>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    pub(crate) fn start(cfg: Config) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            engine: Mutex::new(Engine::new(cfg.clone())),
            windows: Mutex::new(Vec::new()),
            tx: Mutex::new(tx),
            shown: AtomicBool::new(false),
            tap_active: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            swallow_mouse_up: AtomicBool::new(false),
            warned: AtomicBool::new(false),
            guard: Mutex::new(Guard::new()),
            debug_broken: AtomicBool::new(false),
        });
        // A crashed earlier run may have left the system switcher off.
        cmdtab::recover();
        cmdtab::install_exit_hook();
        let worker = {
            let s = shared.clone();
            std::thread::Builder::new().name("window-switcher-worker".into()).spawn(move || worker_loop(s, rx))?
        };
        // Without Accessibility the shared tap retries every 2 s and the
        // status callback picks it up.
        let hook = subscribe(&shared);
        Ok(Runtime { shared, hook: Some(hook), worker: Some(worker) })
    }

    pub(crate) fn update(&mut self, cfg: Config) {
        let refresh = {
            let mut eng = self.shared.engine();
            let refresh = eng.config().show_other_spaces != cfg.show_other_spaces;
            let fx = eng.set_config(cfg);
            perform(&self.shared, &eng, fx);
            refresh
        };
        sync_cmd_tab(&self.shared);
        if refresh {
            self.shared.send(Msg::Refresh);
        }
    }

    pub(crate) fn config(&self) -> Config {
        self.shared.engine().config().clone()
    }

    pub(crate) fn show(&self, mode: Mode) {
        let mut eng = self.shared.engine();
        let cfg = eng.config().clone();
        let cursor = cursor_location().map(|p| (p.x, p.y)).unwrap_or_default();
        let shared = &self.shared;
        let fx = eng.open_sticky(mode, Instant::now(), &mut |m| snapshot(shared, &cfg, m, cursor));
        perform(&self.shared, &eng, fx);
    }

    pub(crate) fn hide(&self) {
        let mut eng = self.shared.engine();
        let fx = eng.cancel();
        perform(&self.shared, &eng, fx);
    }

    pub(crate) fn is_shown(&self) -> bool {
        self.shared.shown.load(Ordering::SeqCst)
    }

    pub(crate) fn debug_break_input(&self, broken: bool) {
        self.shared.debug_broken.store(broken, Ordering::SeqCst);
        sync_cmd_tab(&self.shared);
    }

    pub(crate) fn is_input_active(&self) -> bool {
        self.shared.tap_active.load(Ordering::SeqCst)
    }

    /// The current window list (most recent first, unfiltered).
    pub(crate) fn windows(&self) -> Vec<WindowInfo> {
        self.shared.windows.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn stop(&mut self) {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        // Give Cmd+Tab back first, whatever happens below.
        self.shared.guard.lock().unwrap_or_else(|e| e.into_inner()).release(&mut cmdtab::SysBackend);
        self.shared.send(Msg::Stop);
        // Unsubscribe (waits for a handler call in progress).
        self.hook.take();
        self.shared.tap_active.store(false, Ordering::SeqCst);
        sync_cmd_tab(&self.shared);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        self.shared.shown.store(false, Ordering::SeqCst);
        if let Some(mtm) = MainThreadMarker::new() {
            overlay::destroy(mtm);
        } else {
            on_main(overlay::destroy);
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
