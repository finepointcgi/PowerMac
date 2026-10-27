//! Audio control: default-device rules, device volume/mute and per-app volume.
//!
//! * **Devices** – list outputs/inputs with name, uid, transport, volume and
//!   mute; get/set the default output, input and system-alert device.
//! * **Rules** ([`rules`]) – a priority list of preferred outputs/inputs, a
//!   "never auto-switch to" list (AirPods…) that is reverted within ~50 ms
//!   of the OS switching on its own, and "keep the built-in mic when
//!   Bluetooth headphones connect". Switching from our UI
//!   ([`Handle::set_default_output`]) always wins.
//! * **Per-app volume** – macOS 14.4+: Core Audio process taps (experimental,
//!   off by default, needs the audio-capture permission). Windows: audio
//!   sessions (`ISimpleAudioVolume`).
//! * **Virtual devices** ([`driver`]) – install, remove and configure the
//!   PowerMac Audio HAL driver (`crates/pm-audio-driver`, macOS).
//! * **Per-app routing and the mixer** ([`mixer`]) – send an app to other
//!   devices (`Config::app_routes`), and a sources → outputs matrix with
//!   gain/mute/solo, EQ, limiter and meters (`Config::mixer`). macOS 14.4+
//!   for app sources; Windows routes apps natively, without the mixer.
//!
//! macOS backend: `src/macos` (CoreAudio, hand-bound). Windows backend:
//! `src/windows` (MMDevice API, `IPolicyConfig`, audio sessions).

pub mod driver;
pub mod mixer;
pub mod rules;
mod types;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod win;

/// Test helpers used by `examples/audio_probe.rs`.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use macos::testing as macos_testing;

pub use mixer::config::*;
pub use mixer::state::*;
pub use rules::{Action, RulesEngine, Snapshot};
pub use types::*;

use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Safety-net poll interval (device notifications normally arrive at once).
const POLL: Duration = Duration::from_secs(2);
/// Coalesce bursts of notifications (a device connecting fires several).
const DEBOUNCE: Duration = Duration::from_millis(30);
/// Highest per-app gain.
pub const MAX_APP_VOLUME: f32 = 1.5;

/// What a platform provides.
pub(crate) trait Backend: Send + Sync + 'static {
    /// Devices and defaults. `levels`: also read volume/mute (slower).
    fn snapshot(&self, levels: bool) -> Result<Snapshot>;
    fn set_default(&self, kind: DeviceKind, uid: &str) -> Result<()>;
    fn set_volume(&self, uid: &str, volume: f32) -> Result<()>;
    fn set_mute(&self, uid: &str, mute: bool) -> Result<()>;
    /// Call `notify` whenever devices or defaults (or audio processes)
    /// change. Dropping the returned guard stops it.
    fn watch(&self, notify: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Send>>;

    fn per_app_supported(&self) -> bool;
    fn max_app_volume(&self) -> f32;
    fn apps(&self, levels: &BTreeMap<String, AppLevel>) -> Result<Vec<AudioApp>>;
    /// Make the per-app levels true (create/update/remove taps or set
    /// session volumes). `enabled == false` removes everything.
    fn apply_app_levels(&self, levels: &BTreeMap<String, AppLevel>, enabled: bool) -> Result<()>;
    fn capture_permission(&self) -> CapturePermission;
    fn request_capture_permission(&self) -> Result<bool>;
    /// Human-readable state of the per-app machinery (taps, sessions).
    fn app_diagnostics(&self) -> Vec<String> {
        Vec::new()
    }

    /// The mixer engine (taps + aggregate device) works here.
    fn mixer_supported(&self) -> bool {
        false
    }
    /// Devices with stream layouts and apps with their audio processes.
    fn mix_inventory(&self) -> Result<mixer::Inventory> {
        Err(anyhow!("the mixer is not available on this platform"))
    }
    /// Create (but don't start) the platform device for a topology.
    fn mix_open(&self, _topo: &mixer::Topology) -> Result<Box<dyn mixer::MixDevice>> {
        Err(anyhow!("the mixer is not available on this platform"))
    }
    /// Platforms that route apps through the OS instead of the mixer
    /// (Windows) apply the routes here and return their states; `None` =
    /// routes go through the mixer.
    fn apply_native_routes(&self, _routes: &BTreeMap<String, AppRoute>, _enabled: bool) -> Option<Vec<RouteStatus>> {
        None
    }
}

fn platform_backend() -> Result<Arc<dyn Backend>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Arc::new(macos::MacBackend::new()?))
    }
    #[cfg(windows)]
    {
        Ok(Arc::new(win::WinBackend::new()?))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err(anyhow!("audio control is not supported on this platform"))
    }
}

/// Whether per-app volume can work here: macOS 14.4+ with the process-tap
/// API present, or Windows.
pub fn per_app_volume_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::per_app_volume_supported()
    }
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

/// The macOS audio-capture permission needed by process taps.
pub fn capture_permission() -> CapturePermission {
    #[cfg(target_os = "macos")]
    {
        macos::tcc::preflight()
    }
    #[cfg(not(target_os = "macos"))]
    {
        CapturePermission::NotApplicable
    }
}

/// Show the macOS audio-capture permission prompt without a running engine
/// (blocks until the user answers; returns whether access was granted).
/// Same as [`Handle::request_capture_permission`]; `Ok(true)` elsewhere.
pub fn request_capture_permission() -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        macos::tcc::request()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(true)
    }
}

/// One-shot device listing without starting the engine (for the settings
/// window, which runs in its own process).
pub fn list_devices(kind: DeviceKind) -> Result<Vec<AudioDevice>> {
    let snap = platform_backend()?.snapshot(true)?;
    Ok(snap.devices(kind).to_vec())
}

/// One-shot default-device switch without the engine. Prefer
/// [`Handle::set_default_output`] in the app, which also tells the rules.
pub fn set_default_device(kind: DeviceKind, uid: &str) -> Result<()> {
    platform_backend()?.set_default(kind, uid)
}

enum Msg {
    Changed,
    Config(Box<Config>),
    Stop,
}

type Callback = Box<dyn Fn(&Event) + Send>;

struct Shared {
    backend: Arc<dyn Backend>,
    rules: Mutex<RulesEngine>,
    cfg: Mutex<Config>,
    levels: Mutex<BTreeMap<String, AppLevel>>,
    subscribers: Mutex<Vec<Sender<Event>>>,
    callback: Mutex<Option<Callback>>,
    /// The defaults last reported in events (output, input, system).
    reported: Mutex<[Option<String>; 3]>,
    reported_devices: Mutex<Option<HashSet<String>>>,
    /// Serialises per-app reconciles from the engine and `Handle` calls.
    /// Lock order: `apps_lock` → `mixer` → backend internals.
    apps_lock: Mutex<()>,
    mixer: Mutex<mixer::MixerController>,
    /// Wakes the engine thread (`Msg::Changed`).
    wake: Mutex<Option<Sender<Msg>>>,
}

fn kind_slot(kind: DeviceKind) -> usize {
    match kind {
        DeviceKind::Output => 0,
        DeviceKind::Input => 1,
        DeviceKind::SystemOutput => 2,
    }
}

impl Shared {
    fn emit(&self, ev: Event) {
        log::debug!("audio-control event: {ev:?}");
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.retain(|tx| tx.send(ev.clone()).is_ok());
        }
        if let Ok(cb) = self.callback.lock() {
            if let Some(cb) = cb.as_ref() {
                cb(&ev);
            }
        }
    }

    fn name_of(snap: &Snapshot, kind: DeviceKind, uid: &str) -> Option<String> {
        snap.find(kind, uid).map(|d| d.name.clone())
    }

    fn report_default(&self, kind: DeviceKind, uid: Option<&str>, name: Option<String>, source: ChangeSource) {
        let changed = {
            let mut rep = self.reported.lock().unwrap();
            let slot = &mut rep[kind_slot(kind)];
            if slot.as_deref() != uid {
                *slot = uid.map(str::to_string);
                true
            } else {
                false
            }
        };
        if changed {
            self.emit(Event::DefaultChanged { kind, uid: uid.map(str::to_string), name, source });
        }
    }

    /// One evaluation: snapshot, rules, events, per-app reconcile.
    fn process(&self) {
        let snap = match self.backend.snapshot(false) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audio-control: reading devices failed: {e:#}");
                return;
            }
        };
        let actions = self.rules.lock().unwrap().observe(&snap, Instant::now());
        let mut acted = [false; 3];
        for a in &actions {
            let name = Self::name_of(&snap, a.kind, &a.uid).unwrap_or_default();
            match self.backend.set_default(a.kind, &a.uid) {
                Ok(()) => {
                    log::info!("audio-control: {:?} -> {} ({:?})", a.kind, name, a.reason);
                    acted[kind_slot(a.kind)] = true;
                    self.emit(Event::RuleApplied { kind: a.kind, uid: a.uid.clone(), name: name.clone(), reason: a.reason });
                    self.report_default(a.kind, Some(&a.uid), Some(name), ChangeSource::Rules);
                }
                Err(e) => {
                    log::warn!("audio-control: switching {:?} to {name} failed: {e:#}", a.kind);
                    self.rules.lock().unwrap().note_applied(a.kind, snap.default_uid(a.kind));
                    self.emit(Event::Error { message: format!("switching to {name} failed: {e:#}") });
                }
            }
        }

        let uids: HashSet<String> = snap.outputs.iter().chain(snap.inputs.iter()).map(|d| d.uid.clone()).collect();
        let devices_changed = {
            let mut rep = self.reported_devices.lock().unwrap();
            let changed = rep.as_ref().is_some_and(|r| *r != uids);
            *rep = Some(uids);
            changed
        };
        if devices_changed {
            self.emit(Event::DevicesChanged);
        }
        for kind in [DeviceKind::Output, DeviceKind::Input, DeviceKind::SystemOutput] {
            if !acted[kind_slot(kind)] {
                let uid = snap.default_uid(kind);
                let name = uid.and_then(|u| Self::name_of(&snap, kind, u));
                self.report_default(kind, uid, name, ChangeSource::System);
            }
        }
        self.reconcile_apps();
    }

    /// Mixer / routes first (they decide which apps they take over), then
    /// per-app volume for everything else.
    fn reconcile_apps(&self) {
        let cfg = self.cfg.lock().unwrap().clone();
        let enabled = cfg.enabled && cfg.per_app_volume && self.backend.per_app_supported();
        let levels = self.levels.lock().unwrap().clone();
        let _g = self.apps_lock.lock().unwrap();
        let exclusive = {
            let mut m = self.mixer.lock().unwrap();
            let before = m.state();
            m.reconcile(self.backend.as_ref(), &cfg, &levels);
            let after = m.state();
            for e in after.errors.iter().filter(|e| !before.errors.contains(e)) {
                self.emit(Event::Error { message: e.clone() });
            }
            m.exclusive_apps().clone()
        };
        let mut own = levels;
        own.retain(|k, _| !exclusive.contains(k));
        if let Err(e) = self.backend.apply_app_levels(&own, enabled) {
            log::warn!("audio-control: per-app volume: {e:#}");
        }
    }

    fn wake(&self) {
        if let Some(tx) = self.wake.lock().unwrap().as_ref() {
            let _ = tx.send(Msg::Changed);
        }
    }

    /// Push mixer/route parameter changes without a rebuild when possible.
    fn mixer_changed(&self) {
        let cfg = self.cfg.lock().unwrap().clone();
        let levels = self.levels.lock().unwrap().clone();
        let handled = self.mixer.lock().unwrap().update_values(&cfg, &levels);
        if !handled {
            self.wake();
        }
    }
}

/// Entry point.
pub struct AudioControl;

impl AudioControl {
    /// Start watching devices and applying the rules on a background thread.
    pub fn start(cfg: Config) -> Result<Handle> {
        Self::start_with_backend(cfg, platform_backend()?)
    }

    pub(crate) fn start_with_backend(cfg: Config, backend: Arc<dyn Backend>) -> Result<Handle> {
        let shared = Arc::new(Shared {
            backend: backend.clone(),
            rules: Mutex::new(RulesEngine::new(cfg.clone())),
            levels: Mutex::new(cfg.app_volumes.clone()),
            cfg: Mutex::new(cfg),
            subscribers: Mutex::new(Vec::new()),
            callback: Mutex::new(None),
            reported: Mutex::new([None, None, None]),
            reported_devices: Mutex::new(None),
            apps_lock: Mutex::new(()),
            mixer: Mutex::new(mixer::MixerController::default()),
            wake: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel::<Msg>();
        *shared.wake.lock().unwrap() = Some(tx.clone());
        let notify_tx = Mutex::new(tx.clone());
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Ok(t) = notify_tx.lock() {
                let _ = t.send(Msg::Changed);
            }
        });
        let watch = backend.watch(notify)?;
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("audio-control".into())
            .spawn(move || engine_loop(sh, rx, watch))?;
        Ok(Handle { shared, tx, thread: Some(thread) })
    }
}

fn engine_loop(shared: Arc<Shared>, rx: Receiver<Msg>, watch: Box<dyn Send>) {
    shared.process();
    'outer: loop {
        let msg = match rx.recv_timeout(POLL) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Changed,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut pending = vec![msg];
        if matches!(pending[0], Msg::Changed) {
            std::thread::sleep(DEBOUNCE);
        }
        while let Ok(m) = rx.try_recv() {
            pending.push(m);
        }
        for m in pending {
            match m {
                Msg::Stop => break 'outer,
                Msg::Config(cfg) => {
                    shared.rules.lock().unwrap().set_config((*cfg).clone());
                    *shared.levels.lock().unwrap() = cfg.app_volumes.clone();
                    *shared.cfg.lock().unwrap() = *cfg;
                }
                Msg::Changed => {}
            }
        }
        shared.process();
    }
    drop(watch);
    *shared.wake.lock().unwrap() = None;
    let _g = shared.apps_lock.lock().unwrap();
    shared.mixer.lock().unwrap().stop();
    // Windows: forget the per-app endpoints we set.
    let _ = shared.backend.apply_native_routes(&BTreeMap::new(), false);
    if let Err(e) = shared.backend.apply_app_levels(&BTreeMap::new(), false) {
        log::warn!("audio-control: removing per-app taps: {e:#}");
    }
}

/// A running audio controller.
pub struct Handle {
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// Apply a new config (rules are re-evaluated at once). `app_volumes`
    /// replaces the in-memory per-app levels, so persist
    /// [`app_levels`](Self::app_levels) before calling this.
    pub fn update(&self, cfg: Config) {
        let _ = self.tx.send(Msg::Config(Box::new(cfg)));
    }

    /// Stop the background thread and remove every tap.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// The current config, including per-app levels, routes and the mixer
    /// changed through this handle.
    pub fn config(&self) -> Config {
        let mut c = self.shared.cfg.lock().unwrap().clone();
        c.app_volumes = self.app_levels();
        c
    }

    /// Output devices with volume/mute; `is_default` marks the default output.
    pub fn outputs(&self) -> Vec<AudioDevice> {
        self.shared.backend.snapshot(true).map(|s| s.outputs).unwrap_or_default()
    }

    /// Input devices with volume/mute; `is_default` marks the default input.
    pub fn inputs(&self) -> Vec<AudioDevice> {
        self.shared.backend.snapshot(true).map(|s| s.inputs).unwrap_or_default()
    }

    pub fn default_device(&self, kind: DeviceKind) -> Option<AudioDevice> {
        let s = self.shared.backend.snapshot(false).ok()?;
        let uid = s.default_uid(kind)?.to_string();
        s.find(kind, &uid).cloned()
    }

    fn set_default(&self, kind: DeviceKind, uid: &str) -> Result<()> {
        let snap = self.shared.backend.snapshot(false)?;
        let dev = snap.find(kind, uid).ok_or_else(|| anyhow!("no {kind:?} device with uid {uid}"))?;
        let name = dev.name.clone();
        self.shared.rules.lock().unwrap().note_manual(kind, uid);
        if let Err(e) = self.shared.backend.set_default(kind, uid) {
            self.shared.rules.lock().unwrap().note_applied(kind, snap.default_uid(kind));
            return Err(e);
        }
        self.shared.report_default(kind, Some(uid), Some(name), ChangeSource::Manual);
        Ok(())
    }

    /// Switch the default output from our UI (the rules won't undo it).
    pub fn set_default_output(&self, uid: &str) -> Result<()> {
        self.set_default(DeviceKind::Output, uid)
    }

    pub fn set_default_input(&self, uid: &str) -> Result<()> {
        self.set_default(DeviceKind::Input, uid)
    }

    pub fn set_default_system_output(&self, uid: &str) -> Result<()> {
        self.set_default(DeviceKind::SystemOutput, uid)
    }

    /// Device volume 0.0–1.0 (output scope, or input scope for input-only
    /// devices).
    pub fn set_device_volume(&self, uid: &str, volume: f32) -> Result<()> {
        self.shared.backend.set_volume(uid, volume.clamp(0.0, 1.0))
    }

    pub fn set_device_mute(&self, uid: &str, mute: bool) -> Result<()> {
        self.shared.backend.set_mute(uid, mute)
    }

    /// Apps producing audio (plus apps with a saved level that are running).
    pub fn apps(&self) -> Vec<AudioApp> {
        let levels = self.shared.levels.lock().unwrap().clone();
        match self.shared.backend.apps(&levels) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("audio-control: listing apps failed: {e:#}");
                Vec::new()
            }
        }
    }

    /// Per-app levels to persist in `Config::app_volumes`.
    pub fn app_levels(&self) -> BTreeMap<String, AppLevel> {
        self.shared.levels.lock().unwrap().clone()
    }

    fn change_level(&self, bundle_id: &str, f: impl FnOnce(&mut AppLevel)) -> Result<()> {
        let max = self.shared.backend.max_app_volume();
        let level = {
            let mut levels = self.shared.levels.lock().unwrap();
            let l = levels.entry(bundle_id.to_string()).or_default();
            f(l);
            l.volume = l.volume.clamp(0.0, max);
            let l = *l;
            if l.is_neutral() {
                levels.remove(bundle_id);
            }
            l
        };
        self.shared.emit(Event::AppLevelChanged { bundle_id: bundle_id.to_string(), volume: level.volume, muted: level.muted });
        // Apps the mixer has taken over get their level there.
        self.shared.mixer_changed();
        if self.shared.mixer.lock().unwrap().exclusive_apps().contains(bundle_id) {
            return Ok(());
        }
        let enabled = {
            let c = self.shared.cfg.lock().unwrap();
            c.enabled && c.per_app_volume
        };
        if !enabled {
            return Err(anyhow!("per-app volume is turned off (level saved)"));
        }
        if !self.shared.backend.per_app_supported() {
            return Err(anyhow!("per-app volume needs macOS 14.4 or later (level saved)"));
        }
        let mut levels = self.shared.levels.lock().unwrap().clone();
        let _g = self.shared.apps_lock.lock().unwrap();
        let exclusive = self.shared.mixer.lock().unwrap().exclusive_apps().clone();
        levels.retain(|k, _| !exclusive.contains(k));
        self.shared.backend.apply_app_levels(&levels, true)
    }

    /// Per-app volume 0.0–1.5 (1.0 = unchanged; Windows caps at 1.0). The
    /// level is kept even when applying fails.
    pub fn set_app_volume(&self, bundle_id: &str, volume: f32) -> Result<()> {
        self.change_level(bundle_id, |l| l.volume = volume)
    }

    pub fn set_app_mute(&self, bundle_id: &str, mute: bool) -> Result<()> {
        self.change_level(bundle_id, |l| l.muted = mute)
    }

    /// One line per active per-app tap / session override, plus the mixer
    /// (for logs and the probe example).
    pub fn per_app_diagnostics(&self) -> Vec<String> {
        let mut v = self.shared.backend.app_diagnostics();
        v.extend(self.shared.mixer.lock().unwrap().diagnostics());
        v
    }

    // ---- routing and mixer ------------------------------------------------

    /// The per-app routes (persist into `Config::app_routes`).
    pub fn routes(&self) -> BTreeMap<String, AppRoute> {
        self.shared.cfg.lock().unwrap().app_routes.clone()
    }

    /// Add, replace (`Some`) or remove (`None`) the route of one app. Takes
    /// effect at once (or when the app / device appears) and emits
    /// [`Event::RoutesChanged`]; see [`mixer_state`](Self::mixer_state) for
    /// whether it is running.
    pub fn set_route(&self, bundle_id: &str, route: Option<AppRoute>) -> Result<()> {
        if bundle_id.trim().is_empty() {
            return Err(anyhow!("a route needs an app"));
        }
        {
            let mut c = self.shared.cfg.lock().unwrap();
            match route {
                Some(r) => {
                    c.app_routes.insert(bundle_id.to_string(), r);
                }
                None => {
                    c.app_routes.remove(bundle_id);
                }
            }
        }
        self.shared.emit(Event::RoutesChanged);
        self.shared.wake();
        Ok(())
    }

    /// The mixer config (persist into `Config::mixer`).
    pub fn mixer(&self) -> Mixer {
        self.shared.cfg.lock().unwrap().mixer.clone()
    }

    /// Replace the mixer config. Gain, mute, solo, send, EQ and limiter
    /// changes apply immediately without interrupting audio; adding or
    /// removing sources/outputs rebuilds the mixer device (a short gap).
    /// Emits [`Event::MixerChanged`].
    pub fn set_mixer(&self, m: Mixer) -> Result<()> {
        if m.sources.iter().any(|s| s.id.starts_with(mixer::plan::ROUTE_SOURCE_PREFIX))
            || m.outputs.iter().any(|o| o.id.starts_with(mixer::plan::ROUTE_OUTPUT_PREFIX))
        {
            return Err(anyhow!("ids starting with \"route:\" / \"route-out:\" are reserved"));
        }
        self.shared.cfg.lock().unwrap().mixer = m;
        self.shared.emit(Event::MixerChanged);
        self.shared.mixer_changed();
        Ok(())
    }

    /// Strip, route and engine status for the UI.
    pub fn mixer_state(&self) -> MixerState {
        self.shared.mixer.lock().unwrap().state()
    }

    /// Current meters of every running strip (poll at ~20 Hz,
    /// [`METER_INTERVAL`]). Route strips have ids `route:<bundle id>` and
    /// `route-out:<device uid>`. Empty while the mixer isn't running.
    pub fn meters(&self) -> Meters {
        self.shared.mixer.lock().unwrap().meters()
    }

    /// New receiver for [`Event`]s.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        self.shared.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Call `f` for every event (on the audio-control thread or the caller's
    /// thread; keep it short).
    pub fn set_event_callback(&self, f: impl Fn(&Event) + Send + 'static) {
        *self.shared.callback.lock().unwrap() = Some(Box::new(f));
    }

    pub fn capture_permission(&self) -> CapturePermission {
        self.shared.backend.capture_permission()
    }

    /// macOS: show the audio-capture permission prompt (blocks until the
    /// user answers). Returns whether access was granted.
    pub fn request_capture_permission(&self) -> Result<bool> {
        self.shared.backend.request_capture_permission()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
