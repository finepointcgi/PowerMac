//! Gluing it together: touch frames and clicks go to a worker thread that
//! runs one [`Recognizer`] per device, resolves the binding for the
//! frontmost app and queues the action on an automation executor.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread::JoinHandle;

use keyboard_manager::automation::Executor;

use crate::{Action, Frame, Gesture, GesturesConfig, Recognizer, Status};

/// One recognized gesture, as a listener sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GestureEvent {
    pub gesture: Gesture,
    /// The bound action that runs (`None`: unbound, or recording).
    pub action: Option<Action>,
}

pub type Listener = Box<dyn Fn(&GestureEvent) + Send + Sync>;

/// State shared with the platform reader, the click hook and the worker.
pub(crate) struct Shared {
    pub(crate) config: RwLock<GesturesConfig>,
    /// Fingers touching right now (latest frame of any device).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fingers: AtomicU8,
    /// Bit n set: an n-finger click is bound (any app).
    pub(crate) click_mask: AtomicU32,
    /// Some click binding is app-scoped: keep `front_app` fresh.
    pub(crate) click_scoped: AtomicBool,
    /// Frontmost app at the start of the current touch session.
    pub(crate) front_app: Mutex<Vec<String>>,
    pub(crate) recording: AtomicBool,
    listener: Mutex<Option<Listener>>,
}

impl Shared {
    fn new(cfg: GesturesConfig) -> Self {
        let s = Self {
            config: RwLock::new(cfg),
            fingers: AtomicU8::new(0),
            click_mask: AtomicU32::new(0),
            click_scoped: AtomicBool::new(false),
            front_app: Mutex::new(Vec::new()),
            recording: AtomicBool::new(false),
            listener: Mutex::new(None),
        };
        s.refresh_click_bindings();
        s
    }

    pub(crate) fn refresh_click_bindings(&self) {
        let (mask, scoped) = self
            .config
            .read()
            .map(|c| c.click_bindings())
            .unwrap_or((0, false));
        self.click_mask.store(mask, Ordering::Relaxed);
        self.click_scoped.store(scoped, Ordering::Relaxed);
    }

    /// Whether a physical click with `fingers` down should be kept from the
    /// app (a bound N-finger click that will run an action).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn should_swallow_click(&self, fingers: u8) -> bool {
        if !(3..=5).contains(&fingers)
            || self.click_mask.load(Ordering::Relaxed) & (1 << fingers) == 0
        {
            return false;
        }
        if self.recording.load(Ordering::Relaxed) {
            return true;
        }
        let Ok(cfg) = self.config.read() else {
            return false;
        };
        if !cfg.swallow_clicks {
            return false;
        }
        let front = self.front_app.lock().map(|f| f.clone()).unwrap_or_default();
        matches!(cfg.resolve(Gesture::Click { fingers }, &front), Some(a) if *a != Action::Disabled)
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // fed by the macOS reader
pub(crate) enum Msg {
    Frame(usize, Frame),
    Click,
}

/// The worker's logic, separate from threads and the platform for tests.
pub(crate) struct Dispatcher {
    shared: Arc<Shared>,
    recognizers: HashMap<usize, Recognizer>,
    front: Box<dyn FnMut() -> Vec<String> + Send>,
    run: Box<dyn FnMut(Action) + Send>,
}

impl Dispatcher {
    pub(crate) fn new(
        shared: Arc<Shared>,
        front: Box<dyn FnMut() -> Vec<String> + Send>,
        run: Box<dyn FnMut(Action) + Send>,
    ) -> Self {
        Self {
            shared,
            recognizers: HashMap::new(),
            front,
            run,
        }
    }

    pub(crate) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Frame(dev, f) => {
                let t = self
                    .shared
                    .config
                    .read()
                    .map(|c| c.thresholds)
                    .unwrap_or_default();
                let r = self
                    .recognizers
                    .entry(dev)
                    .or_insert_with(|| Recognizer::new(t));
                r.set_thresholds(t);
                let was_active = r.active();
                let g = r.frame(&f);
                if !was_active && r.active() && self.shared.click_scoped.load(Ordering::Relaxed) {
                    let front = (self.front)();
                    if let Ok(mut fa) = self.shared.front_app.lock() {
                        *fa = front;
                    }
                }
                if let Some(g) = g {
                    self.fire(g);
                }
            }
            Msg::Click => {
                // The device with a session in progress (one trackpad is the
                // common case; with two, the one being touched).
                let g = self
                    .recognizers
                    .values_mut()
                    .filter(|r| r.active())
                    .max_by_key(|r| r.fingers())
                    .and_then(|r| r.click());
                if let Some(g) = g {
                    self.fire(g);
                }
            }
        }
    }

    fn fire(&mut self, g: Gesture) {
        let recording = self.shared.recording.load(Ordering::Relaxed);
        let action = if recording {
            None
        } else {
            let front = (self.front)();
            self.shared
                .config
                .read()
                .ok()
                .and_then(|c| c.resolve(g, &front).cloned())
        };
        log::debug!("gestures: {g} -> {action:?}");
        if let Ok(l) = self.shared.listener.lock() {
            if let Some(l) = l.as_ref() {
                l(&GestureEvent {
                    gesture: g,
                    action: action.clone(),
                });
            }
        }
        if let Some(a) = action.filter(|a| *a != Action::Disabled) {
            (self.run)(a);
        }
    }
}

/// Entry point.
pub struct Gestures;

impl Gestures {
    /// Start with `config`. Never fails: when the trackpad can't be read
    /// [`Handle::status`] says why ("gestures unavailable: …") and the rest
    /// of the app is unaffected. Nothing is read until something is bound.
    pub fn start(config: GesturesConfig) -> Handle {
        for w in config.validate() {
            log::warn!("gestures: {w}");
        }
        let shared = Arc::new(Shared::new(config));
        let executor = Executor::start("gestures", None).ok();
        let (tx, rx) = mpsc::channel::<Msg>();
        let sender = executor.as_ref().and_then(|e| e.sender());
        let s2 = shared.clone();
        let worker = std::thread::Builder::new()
            .name("gestures".into())
            .spawn(move || {
                let mut d = Dispatcher::new(
                    s2,
                    Box::new(keyboard_manager::foreground_app),
                    Box::new(move |a| {
                        if let Some(tx) = &sender {
                            let _ = tx.send(a);
                        }
                    }),
                );
                for m in rx {
                    d.handle(m);
                }
            })
            .ok();
        let h = Handle {
            shared,
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(worker),
            executor,
            platform: Mutex::new(Platform::default()),
            status: Mutex::new(Status::Off),
        };
        h.reconcile();
        h
    }
}

/// The platform pieces that are running.
#[derive(Default)]
struct Platform {
    #[cfg(target_os = "macos")]
    reader: Option<crate::macos::reader::Reader>,
    #[cfg(target_os = "macos")]
    clicks: Option<Result<crate::macos::ClickHook, String>>,
}

/// A running gesture engine. Dropping it stops reading the trackpad.
pub struct Handle {
    shared: Arc<Shared>,
    tx: Mutex<Option<mpsc::Sender<Msg>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    executor: Option<Executor>,
    platform: Mutex<Platform>,
    status: Mutex<Status>,
}

impl Handle {
    /// Replace the configuration (starts / stops the reader as needed).
    pub fn update(&self, config: GesturesConfig) {
        for w in config.validate() {
            log::warn!("gestures: {w}");
        }
        if let Ok(mut c) = self.shared.config.write() {
            *c = config;
        }
        self.shared.refresh_click_bindings();
        self.reconcile();
    }

    pub fn config(&self) -> GesturesConfig {
        self.shared
            .config
            .read()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::Off)
    }

    /// Called for every recognized gesture (bound or not, while running).
    pub fn set_listener(&self, l: Option<Listener>) {
        if let Ok(mut g) = self.shared.listener.lock() {
            *g = l;
        }
    }

    /// Recording mode for a settings page: the trackpad is read even with
    /// nothing bound, gestures go to the listener only, and no action runs.
    pub fn set_recording(&self, on: bool) {
        self.shared.recording.store(on, Ordering::Relaxed);
        self.reconcile();
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn sender(&self) -> Option<mpsc::Sender<Msg>> {
        self.tx.lock().ok().and_then(|g| g.clone())
    }

    fn reconcile(&self) {
        let (want_reader, want_clicks) = {
            let c = self.config();
            let rec = self.shared.recording.load(Ordering::Relaxed);
            let on = c.enabled && (c.needs_reader() || rec);
            (on, on && (c.needs_click_hook() || rec))
        };
        let Ok(mut p) = self.platform.lock() else {
            return;
        };
        let status = self.reconcile_platform(&mut p, want_reader, want_clicks);
        if let Ok(mut s) = self.status.lock() {
            if *s != status {
                match &status {
                    Status::Unavailable(e) => log::warn!("gestures: {e}"),
                    Status::Running { devices, .. } => {
                        log::info!("gestures: reading {} trackpad(s)", devices.len())
                    }
                    Status::Off => {}
                }
            }
            *s = status;
        }
    }

    #[cfg(target_os = "macos")]
    fn reconcile_platform(&self, p: &mut Platform, want_reader: bool, want_clicks: bool) -> Status {
        use crate::macos::{reader::Reader, ClickHook};
        if !want_reader {
            p.clicks = None;
            p.reader = None;
            self.shared.fingers.store(0, Ordering::Relaxed);
            return Status::Off;
        }
        if p.reader.is_none() {
            let Some(tx) = self.sender() else {
                return Status::Unavailable("gestures unavailable: stopped".into());
            };
            let shared = self.shared.clone();
            let tx = Mutex::new(tx);
            match Reader::start(Box::new(move |dev, f: &Frame| {
                let n = f.contacts.iter().filter(|c| c.touching).count().min(255) as u8;
                shared.fingers.store(n, Ordering::Relaxed);
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(Msg::Frame(dev, f.clone()));
                }
            })) {
                Ok(r) => p.reader = Some(r),
                Err(e) => return Status::Unavailable(e),
            }
        }
        match (want_clicks, p.clicks.is_some()) {
            (true, false) => {
                p.clicks = Some(match self.sender() {
                    Some(tx) => ClickHook::start(self.shared.clone(), tx),
                    None => Err("stopped".into()),
                })
            }
            (false, true) => p.clicks = None,
            _ => {}
        }
        let devices = p.reader.as_ref().map(|r| r.devices()).unwrap_or_default();
        let clicks = p
            .clicks
            .as_ref()
            .map(|c| c.as_ref().map(|_| ()).map_err(|e| e.clone()));
        Status::Running { devices, clicks }
    }

    #[cfg(not(target_os = "macos"))]
    fn reconcile_platform(
        &self,
        _p: &mut Platform,
        want_reader: bool,
        _want_clicks: bool,
    ) -> Status {
        if want_reader {
            Status::Unavailable(crate::UNSUPPORTED.into())
        } else {
            Status::Off
        }
    }

    /// Run `gesture`'s binding now, as if it had been performed (with the
    /// current frontmost app). For a settings page's "Test" button.
    pub fn simulate(&self, gesture: Gesture) -> Option<Action> {
        let front = keyboard_manager::foreground_app();
        let a = self.config().resolve(gesture, &front).cloned()?;
        if a != Action::Disabled {
            if let Some(e) = &self.executor {
                e.submit(a.clone());
            }
        }
        Some(a)
    }

    /// Stop reading and wait for queued actions (background shell
    /// commands keep running).
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        if let Ok(mut p) = self.platform.lock() {
            #[cfg(target_os = "macos")]
            {
                p.clicks = None;
                p.reader = None;
            }
            let _ = &mut *p;
        }
        if let Ok(mut t) = self.tx.lock() {
            t.take();
        }
        if let Some(w) = self.worker.lock().ok().and_then(|mut w| w.take()) {
            let _ = w.join();
        }
        if let Some(e) = &self.executor {
            e.stop();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Contact, GestureBinding, SwipeDir};

    fn harness(
        cfg: GesturesConfig,
        front: Vec<String>,
    ) -> (Dispatcher, Arc<Mutex<Vec<Action>>>, Arc<Shared>) {
        let shared = Arc::new(Shared::new(cfg));
        let ran = Arc::new(Mutex::new(Vec::new()));
        let r2 = ran.clone();
        let d = Dispatcher::new(
            shared.clone(),
            Box::new(move || front.clone()),
            Box::new(move |a| r2.lock().unwrap().push(a)),
        );
        (d, ran, shared)
    }

    fn tap(d: &mut Dispatcher, n: i32, t0: f64) {
        for i in 0..8 {
            let contacts = (0..n)
                .map(|k| Contact {
                    id: k,
                    x: 0.3 + 0.1 * k as f32,
                    y: 0.5,
                    touching: true,
                })
                .collect();
            d.handle(Msg::Frame(
                1,
                Frame {
                    time: t0 + i as f64 * 0.01,
                    contacts,
                },
            ));
        }
        d.handle(Msg::Frame(
            1,
            Frame {
                time: t0 + 0.09,
                contacts: vec![],
            },
        ));
    }

    fn text(s: &str) -> Action {
        Action::Text { text: s.into() }
    }

    #[test]
    fn dispatch_resolves_bindings_with_app_scope() {
        let cfg = GesturesConfig {
            bindings: vec![
                GestureBinding::new(Gesture::Tap { fingers: 3 }, text("global")),
                GestureBinding {
                    app: Some("com.apple.Safari".into()),
                    ..GestureBinding::new(Gesture::Tap { fingers: 3 }, text("safari"))
                },
                GestureBinding {
                    app: Some("Terminal".into()),
                    ..GestureBinding::new(Gesture::Tap { fingers: 3 }, Action::Disabled)
                },
                GestureBinding::new(Gesture::Click { fingers: 4 }, text("click4")),
            ],
            ..Default::default()
        };
        let (mut d, ran, shared) = harness(
            cfg.clone(),
            vec!["com.apple.Safari".into(), "Safari".into()],
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = events.clone();
        *shared.listener.lock().unwrap() = Some(Box::new(move |e: &GestureEvent| {
            e2.lock().unwrap().push(e.clone())
        }));
        tap(&mut d, 3, 1.0);
        assert_eq!(*ran.lock().unwrap(), vec![text("safari")]);
        assert_eq!(
            events.lock().unwrap()[0].gesture,
            Gesture::Tap { fingers: 3 }
        );

        let (mut d, ran, _) = harness(
            cfg.clone(),
            vec!["com.apple.finder".into(), "Finder".into()],
        );
        tap(&mut d, 3, 1.0);
        assert_eq!(*ran.lock().unwrap(), vec![text("global")]);

        // Disabled for Terminal: nothing runs.
        let (mut d, ran, _) = harness(
            cfg.clone(),
            vec!["com.apple.Terminal".into(), "Terminal".into()],
        );
        tap(&mut d, 3, 1.0);
        assert!(ran.lock().unwrap().is_empty());

        // Unbound gestures (a 4-finger tap) run nothing; the listener still sees them.
        let (mut d, ran, shared) = harness(cfg.clone(), vec![]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        *shared.listener.lock().unwrap() = Some(Box::new(move |e: &GestureEvent| {
            s2.lock().unwrap().push(e.clone())
        }));
        tap(&mut d, 4, 1.0);
        assert!(ran.lock().unwrap().is_empty());
        assert_eq!(
            seen.lock().unwrap()[0],
            GestureEvent {
                gesture: Gesture::Tap { fingers: 4 },
                action: None
            }
        );

        // A click with 4 fingers down.
        let (mut d, ran, shared) = harness(cfg.clone(), vec![]);
        for i in 0..6 {
            let contacts = (0..4)
                .map(|k| Contact {
                    id: k,
                    x: 0.2 + 0.1 * k as f32,
                    y: 0.5,
                    touching: true,
                })
                .collect();
            d.handle(Msg::Frame(
                1,
                Frame {
                    time: 1.0 + i as f64 * 0.01,
                    contacts,
                },
            ));
        }
        assert!(shared.should_swallow_click(4));
        assert!(
            !shared.should_swallow_click(3),
            "3-finger click isn't bound"
        );
        d.handle(Msg::Click);
        assert_eq!(*ran.lock().unwrap(), vec![text("click4")]);

        // Recording: listener only, no actions, clicks swallowed.
        let (mut d, ran, shared) = harness(cfg, vec![]);
        shared.recording.store(true, Ordering::Relaxed);
        tap(&mut d, 3, 1.0);
        assert!(ran.lock().unwrap().is_empty());
        assert!(shared.should_swallow_click(4));
    }

    #[test]
    fn swallow_respects_app_scope_and_switch() {
        let cfg = GesturesConfig {
            bindings: vec![GestureBinding {
                app: Some("com.google.Chrome".into()),
                ..GestureBinding::new(Gesture::Click { fingers: 3 }, text("x"))
            }],
            ..Default::default()
        };
        let (mut d, _, shared) = harness(cfg.clone(), vec!["com.google.Chrome".into()]);
        assert!(shared.click_scoped.load(Ordering::Relaxed));
        assert!(!shared.should_swallow_click(3), "front app unknown yet");
        // A touch session start refreshes the cached front app.
        d.handle(Msg::Frame(
            1,
            Frame {
                time: 1.0,
                contacts: vec![Contact {
                    id: 1,
                    x: 0.5,
                    y: 0.5,
                    touching: true,
                }],
            },
        ));
        assert!(shared.should_swallow_click(3));
        shared.config.write().unwrap().swallow_clicks = false;
        assert!(!shared.should_swallow_click(3));
    }

    #[test]
    fn handle_with_nothing_bound_is_off() {
        let h = Gestures::start(GesturesConfig::default());
        assert_eq!(h.status(), Status::Off);
        let mut c = GesturesConfig::default();
        c.bindings.push(GestureBinding::new(
            Gesture::Swipe {
                fingers: 3,
                dir: SwipeDir::Left,
            },
            Action::Disabled,
        ));
        h.update(c);
        // Only "disabled" bindings: still nothing to read.
        assert_eq!(h.status(), Status::Off);
        assert_eq!(
            h.simulate(Gesture::Swipe {
                fingers: 3,
                dir: SwipeDir::Left
            }),
            Some(Action::Disabled)
        );
        assert_eq!(h.simulate(Gesture::Tap { fingers: 5 }), None);
        h.stop();
    }
}
