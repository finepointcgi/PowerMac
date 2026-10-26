//! The worker: owns the platform backend, applies the config, re-applies
//! it when displays come and go, runs the revert timer and keeps the
//! crash-recovery state file.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::logic::{self, Pending, RevertTimer, Target, VirtualPlan};
use crate::{Config, DisplayConfig, DisplayModes, Mode, ModeSpec, PendingChange, Resolution, Token};

/// How often the display list is checked for connects / disconnects.
const POLL: Duration = Duration::from_secs(2);

/// What a backend reports about a physical display.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RawDisplay {
    pub id: String,
    pub name: String,
    pub is_builtin: bool,
    pub vendor: u32,
    pub model: u32,
    pub current: Option<Mode>,
    pub modes: Vec<Mode>,
    /// Physical size in mm.
    pub size_mm: (f64, f64),
    /// Windows: effective DPI.
    pub dpi: Option<u32>,
}

pub(crate) trait Backend {
    /// Physical displays (never this module's virtual ones), modes normalized.
    fn enumerate(&mut self) -> Vec<RawDisplay>;
    /// Cheap fingerprint of the connected displays.
    fn signature(&mut self) -> Vec<String>;
    fn set_mode(&mut self, id: &str, target: Target, mode: &Mode, permanent: bool) -> Result<()>;
    fn hidpi_supported(&self) -> bool;
    /// Create (or update the modes of) the virtual display for `id` and
    /// mirror `id` onto it.
    fn enable_hidpi(&mut self, id: &str, plan: &VirtualPlan) -> Result<()>;
    /// Un-mirror `id` and destroy its virtual display.
    fn disable_hidpi(&mut self, id: &str) -> Result<()>;
    /// The virtual display's current mode and modes.
    fn virtual_modes(&mut self, id: &str) -> Option<(Option<Mode>, Vec<Mode>)>;
    /// After a crash: undo mirroring of these displays left from a previous run.
    fn repair_mirroring(&mut self, ids: &[String]);
}

/// Persisted so a crash is undone on the next start.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct State {
    /// Modes of physical displays before `preferredMode` changed them.
    pub originals: BTreeMap<String, Mode>,
    /// Unconfirmed physical mode changes.
    pub pending: Vec<Pending>,
    /// Displays mirrored onto a virtual HiDPI display.
    pub mirrored: Vec<String>,
}

impl State {
    fn is_empty(&self) -> bool {
        self.originals.is_empty() && self.pending.is_empty() && self.mirrored.is_empty()
    }
}

pub(crate) enum Cmd {
    Update(Config),
    Displays(mpsc::Sender<Vec<DisplayModes>>),
    Apply { id: String, mode: Mode, timeout: Duration, permanent: bool, reply: mpsc::Sender<Result<Token>> },
    Confirm(Token, mpsc::Sender<Result<()>>),
    Revert(Token, mpsc::Sender<Result<()>>),
    Pending(mpsc::Sender<Vec<PendingChange>>),
    Stop(mpsc::Sender<()>),
}

fn scale_percent(d: &RawDisplay, current: Option<&Mode>) -> Option<u32> {
    if let Some(dpi) = d.dpi {
        return Some((dpi * 100 + 48) / 96);
    }
    current.filter(|m| m.width > 0).map(|m| (m.pixel_width * 100 + m.width / 2) / m.width)
}

fn describe(b: &mut dyn Backend, d: &RawDisplay, cfg: &Config, plans: &BTreeMap<String, VirtualPlan>) -> DisplayModes {
    let dcfg = logic::match_config(&d.id, d.vendor, d.model, &cfg.displays).map(|(_, c)| c.clone()).unwrap_or_default();
    let native = logic::native_resolution(&d.modes);
    let virt = plans.get(&d.id).and_then(|plan| {
        b.virtual_modes(&d.id).map(|(cur, modes)| {
            if modes.is_empty() {
                // This process can't see the virtual display's modes yet:
                // report the planned ones (it starts in the target mode).
                let synth = |r: &Resolution| Mode { width: r.width, height: r.height, pixel_width: r.width * 2, pixel_height: r.height * 2, refresh: plan.refresh, hidpi: true };
                (cur.or(Some(synth(&plan.target))), plan.logical.iter().map(synth).collect())
            } else {
                (cur, modes)
            }
        })
    });
    let hidpi_active = virt.is_some();
    let (current, modes) = virt.unwrap_or((d.current, d.modes.clone()));
    DisplayModes {
        id: d.id.clone(),
        name: d.name.clone(),
        is_builtin: d.is_builtin,
        vendor: d.vendor,
        model: d.model,
        scale_percent: scale_percent(d, current.as_ref()),
        current,
        modes,
        physical_current: d.current,
        native,
        hidpi_capable: !d.is_builtin && b.hidpi_supported(),
        hidpi_active,
        native_hidpi: d.modes.iter().any(|m| m.hidpi),
        suggested_hidpi: native.map(|n| logic::hidpi_logical_resolutions(n, &dcfg.custom_resolutions)).unwrap_or_default(),
        dpi: d.dpi,
    }
}

pub(crate) fn describe_all(b: &mut dyn Backend, cfg: &Config, plans: &BTreeMap<String, VirtualPlan>) -> Vec<DisplayModes> {
    let ds = b.enumerate();
    ds.iter().map(|d| describe(b, d, cfg, plans)).collect()
}

pub(crate) struct Worker {
    backend: Box<dyn Backend>,
    config: Config,
    state_path: Option<PathBuf>,
    timer: RevertTimer,
    originals: BTreeMap<String, Mode>,
    plans: BTreeMap<String, VirtualPlan>,
    /// Config entry last applied to each connected display.
    applied: BTreeMap<String, DisplayConfig>,
    signature: Vec<String>,
}

impl Worker {
    pub fn new(backend: Box<dyn Backend>, config: Config, state_path: Option<PathBuf>) -> Self {
        Self { backend, config, state_path, timer: RevertTimer::default(), originals: BTreeMap::new(), plans: BTreeMap::new(), applied: BTreeMap::new(), signature: Vec::new() }
    }

    fn load_state(&self) -> Option<State> {
        let p = self.state_path.as_ref()?;
        let s = std::fs::read_to_string(p).ok()?;
        match serde_json::from_str(&s) {
            Ok(st) => Some(st),
            Err(e) => {
                log::warn!("display-scaling: ignoring unreadable {}: {e}", p.display());
                None
            }
        }
    }

    fn save_state(&self) {
        let Some(p) = &self.state_path else { return };
        let st = State {
            originals: self.originals.clone(),
            pending: self.timer.pending().iter().filter(|p| p.target == Target::Physical).cloned().collect(),
            mirrored: self.plans.keys().cloned().collect(),
        };
        let r = if st.is_empty() {
            match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        } else {
            (|| -> Result<()> {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let tmp = p.with_extension("json.tmp");
                std::fs::write(&tmp, serde_json::to_vec_pretty(&st)?)?;
                std::fs::rename(&tmp, p)?;
                Ok(())
            })()
        };
        if let Err(e) = r {
            log::warn!("display-scaling: saving {}: {e:#}", p.display());
        }
    }

    /// Undo what a previous (crashed) run left, then apply the config.
    pub fn startup(&mut self) {
        if let Some(st) = self.load_state() {
            log::info!("display-scaling: restoring state left by a previous run ({} mirrored, {} changed, {} unconfirmed)", st.mirrored.len(), st.originals.len(), st.pending.len());
            if !st.mirrored.is_empty() {
                self.backend.repair_mirroring(&st.mirrored);
            }
            let present = self.backend.enumerate();
            let mut restore: BTreeMap<String, Mode> = st.originals.clone();
            for p in &st.pending {
                // An unconfirmed change goes back to what was there before it.
                restore.entry(p.display_id.clone()).or_insert(p.previous);
            }
            for (id, mode) in restore {
                let Some(d) = present.iter().find(|d| d.id == id) else { continue };
                if d.current.is_some_and(|c| !logic::same_mode(&c, &mode)) {
                    if let Err(e) = self.backend.set_mode(&id, Target::Physical, &mode, false) {
                        log::warn!("display-scaling: restoring {id} to {}: {e:#}", mode.label());
                    }
                }
            }
            self.save_state();
        }
        self.signature = self.backend.signature();
        self.reconcile();
    }

    pub fn run(&mut self, rx: mpsc::Receiver<Cmd>) {
        let mut next_poll = Instant::now() + POLL;
        loop {
            let now = Instant::now();
            let wake = self.timer.next_deadline().map_or(next_poll, |d| d.min(next_poll));
            match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(Cmd::Stop(reply)) => {
                    self.stop();
                    let _ = reply.send(());
                    return;
                }
                Ok(cmd) => self.handle(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return;
                }
            }
            self.revert_expired(Instant::now());
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + POLL;
                self.poll();
            }
        }
    }

    pub fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Update(c) => {
                self.config = c;
                self.reconcile();
            }
            Cmd::Displays(r) => {
                let _ = r.send(describe_all(&mut *self.backend, &self.config, &self.plans));
            }
            Cmd::Apply { id, mode, timeout, permanent, reply } => {
                let _ = reply.send(self.apply(&id, &mode, timeout, permanent, Instant::now()));
            }
            Cmd::Confirm(t, r) => {
                let _ = r.send(self.confirm(t));
            }
            Cmd::Revert(t, r) => {
                let res = match self.timer.confirm(t) {
                    Some(p) => self.revert_one(&p),
                    None => Err(anyhow!("no pending change {t:?}")),
                };
                self.save_state();
                let _ = r.send(res);
            }
            Cmd::Pending(r) => {
                let now = Instant::now();
                let v = self
                    .timer
                    .pending()
                    .iter()
                    .map(|p| PendingChange { token: p.token, display_id: p.display_id.clone(), previous: p.previous, applied: p.applied, seconds_left: p.deadline.saturating_duration_since(now).as_secs_f64().ceil() as u64 })
                    .collect();
                let _ = r.send(v);
            }
            Cmd::Stop(r) => {
                self.stop();
                let _ = r.send(());
            }
        }
    }

    fn poll(&mut self) {
        let sig = self.backend.signature();
        if sig != self.signature {
            log::info!("display-scaling: displays changed, re-applying");
            self.signature = sig;
            self.reconcile();
        }
    }

    /// Apply the config to every connected display whose entry changed
    /// since it was last applied (or that just connected).
    pub fn reconcile(&mut self) {
        let displays = self.backend.enumerate();
        let present: Vec<&str> = displays.iter().map(|d| d.id.as_str()).collect();
        let gone: Vec<String> = self.plans.keys().filter(|id| !present.contains(&id.as_str())).cloned().collect();
        for id in gone {
            log::info!("display-scaling: {id} disconnected, removing its HiDPI display");
            if let Err(e) = self.backend.disable_hidpi(&id) {
                log::warn!("display-scaling: {id}: {e:#}");
            }
            self.plans.remove(&id);
        }
        self.applied.retain(|id, _| present.contains(&id.as_str()));
        let stale: Vec<String> = self.timer.pending().iter().map(|p| p.display_id.clone()).filter(|id| !present.contains(&id.as_str())).collect();
        for id in stale {
            self.timer.forget_display(&id);
        }
        for d in &displays {
            let cfg = if self.config.enabled { logic::match_config(&d.id, d.vendor, d.model, &self.config.displays).map(|(_, c)| c.clone()).unwrap_or_default() } else { DisplayConfig::default() };
            if self.applied.get(&d.id) == Some(&cfg) {
                continue;
            }
            if let Err(e) = self.apply_display(d, &cfg) {
                log::warn!("display-scaling: {}: {e:#}", d.id);
            }
            self.applied.insert(d.id.clone(), cfg);
        }
        self.save_state();
    }

    fn restore_original(&mut self, d: &RawDisplay) -> Result<()> {
        if let Some(orig) = self.originals.remove(&d.id) {
            if d.current.is_some_and(|c| !logic::same_mode(&c, &orig)) {
                log::info!("display-scaling: {} back to {}", d.id, orig.label());
                self.backend.set_mode(&d.id, Target::Physical, &orig, false)?;
            }
        }
        Ok(())
    }

    fn apply_display(&mut self, d: &RawDisplay, cfg: &DisplayConfig) -> Result<()> {
        let want_hidpi = cfg.hidpi_enabled && !d.is_builtin && self.backend.hidpi_supported();
        if cfg.hidpi_enabled && !want_hidpi {
            log::warn!("display-scaling: HiDPI not available for {}", d.id);
        }
        if want_hidpi {
            self.restore_original(d)?;
            let native = logic::native_resolution(&d.modes).ok_or_else(|| anyhow!("{} reports no modes", d.id))?;
            let refresh = d.current.map(|m| m.refresh).unwrap_or(60.0);
            let plan = logic::plan_virtual(native, d.size_mm, refresh, cfg);
            if self.plans.get(&d.id) != Some(&plan) {
                log::info!("display-scaling: HiDPI for {} ({} logical sizes, target {})", d.id, plan.logical.len(), plan.target);
                // Recorded before the change so a crash in between is still undone.
                self.plans.insert(d.id.clone(), plan.clone());
                self.save_state();
                if let Err(e) = self.backend.enable_hidpi(&d.id, &plan) {
                    self.plans.remove(&d.id);
                    let _ = self.backend.disable_hidpi(&d.id);
                    self.save_state();
                    return Err(e);
                }
            }
            let spec = cfg.preferred_mode.clone().filter(|p| plan.logical.contains(&Resolution { width: p.width, height: p.height }) || p.hidpi == Some(false)).unwrap_or(ModeSpec {
                width: plan.target.width,
                height: plan.target.height,
                hidpi: Some(true),
                ..Default::default()
            });
            let (cur, modes) = self.backend.virtual_modes(&d.id).ok_or_else(|| anyhow!("virtual display for {} is gone", d.id))?;
            if modes.is_empty() {
                log::info!("display-scaling: modes of {}'s virtual display not visible yet; it starts in {} HiDPI", d.id, plan.target);
                return Ok(());
            }
            let m = logic::select_mode(&modes, &spec).ok_or_else(|| anyhow!("virtual display for {} lacks {}x{}", d.id, spec.width, spec.height))?;
            if cur.is_none_or(|c| !logic::same_mode(&c, &m)) {
                self.backend.set_mode(&d.id, Target::Virtual, &m, false)?;
            }
            return Ok(());
        }
        if self.plans.remove(&d.id).is_some() {
            log::info!("display-scaling: HiDPI off for {}", d.id);
            self.save_state();
            self.backend.disable_hidpi(&d.id)?;
        }
        match &cfg.preferred_mode {
            Some(spec) => {
                let m = logic::select_mode(&d.modes, spec).ok_or_else(|| anyhow!("{} has no mode {}x{}", d.id, spec.width, spec.height))?;
                let Some(cur) = d.current else { bail!("{}: current mode unknown", d.id) };
                if !logic::same_mode(&cur, &m) {
                    self.originals.entry(d.id.clone()).or_insert(cur);
                    self.save_state();
                    log::info!("display-scaling: {} to {}", d.id, m.label());
                    self.backend.set_mode(&d.id, Target::Physical, &m, false)?;
                }
                Ok(())
            }
            None => self.restore_original(d),
        }
    }

    pub fn apply(&mut self, id: &str, mode: &Mode, timeout: Duration, permanent: bool, now: Instant) -> Result<Token> {
        let d = self.backend.enumerate().into_iter().find(|d| d.id == id).ok_or_else(|| anyhow!("no display {id:?}"))?;
        let (target, cur, modes) = match self.plans.contains_key(id).then(|| self.backend.virtual_modes(id)).flatten() {
            Some((c, m)) => (Target::Virtual, c, m),
            None => (Target::Physical, d.current, d.modes),
        };
        let m = *modes.iter().find(|m| logic::same_mode(m, mode)).ok_or_else(|| anyhow!("{id} does not offer {}", mode.label()))?;
        let prev = cur.ok_or_else(|| anyhow!("{id}: current mode unknown"))?;
        // Arm (and persist) before switching so a crash mid-way still reverts.
        let token = self.timer.arm(id, target, prev, m, permanent, now + timeout);
        self.save_state();
        if let Err(e) = self.backend.set_mode(id, target, &m, false).with_context(|| format!("switching {id} to {}", m.label())) {
            self.timer.confirm(token);
            self.save_state();
            return Err(e);
        }
        Ok(token)
    }

    pub fn confirm(&mut self, token: Token) -> Result<()> {
        let p = self.timer.confirm(token).ok_or_else(|| anyhow!("no pending change {token:?} (already reverted?)"))?;
        // The user chose this mode; it no longer counts as ours to restore.
        if p.target == Target::Physical {
            self.originals.remove(&p.display_id);
        }
        self.save_state();
        if p.permanent {
            self.backend.set_mode(&p.display_id, p.target, &p.applied, true)?;
        }
        Ok(())
    }

    fn revert_one(&mut self, p: &Pending) -> Result<()> {
        log::info!("display-scaling: reverting {} to {}", p.display_id, p.previous.label());
        self.backend.set_mode(&p.display_id, p.target, &p.previous, false)
    }

    pub fn revert_expired(&mut self, now: Instant) {
        let gone = self.timer.expired(now);
        if gone.is_empty() {
            return;
        }
        for p in &gone {
            if let Err(e) = self.revert_one(p) {
                log::warn!("display-scaling: revert {}: {e:#}", p.display_id);
            }
        }
        self.save_state();
    }

    pub fn stop(&mut self) {
        for p in self.timer.drain() {
            if let Err(e) = self.revert_one(&p) {
                log::warn!("display-scaling: revert {}: {e:#}", p.display_id);
            }
        }
        for id in std::mem::take(&mut self.plans).into_keys() {
            if let Err(e) = self.backend.disable_hidpi(&id) {
                log::warn!("display-scaling: HiDPI off for {id}: {e:#}");
            }
        }
        let displays = self.backend.enumerate();
        for (id, orig) in std::mem::take(&mut self.originals) {
            if let Some(d) = displays.iter().find(|d| d.id == id) {
                if d.current.is_some_and(|c| !logic::same_mode(&c, &orig)) {
                    if let Err(e) = self.backend.set_mode(&id, Target::Physical, &orig, false) {
                        log::warn!("display-scaling: restoring {id}: {e:#}");
                    }
                }
            }
        }
        self.applied.clear();
        self.save_state();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn m(w: u32, h: u32, scale: u32) -> Mode {
        Mode { width: w, height: h, pixel_width: w * scale, pixel_height: h * scale, refresh: 60.0, hidpi: scale == 2 }
    }

    #[derive(Default)]
    pub struct Fake {
        pub displays: Vec<RawDisplay>,
        pub virtuals: BTreeMap<String, (Option<Mode>, Vec<Mode>)>,
        pub log: Vec<String>,
        pub fail_hidpi: bool,
    }

    #[derive(Clone, Default)]
    pub struct FakeBackend(pub Arc<Mutex<Fake>>);

    impl Backend for FakeBackend {
        fn enumerate(&mut self) -> Vec<RawDisplay> {
            self.0.lock().unwrap().displays.clone()
        }
        fn signature(&mut self) -> Vec<String> {
            self.0.lock().unwrap().displays.iter().map(|d| d.id.clone()).collect()
        }
        fn set_mode(&mut self, id: &str, target: Target, mode: &Mode, permanent: bool) -> Result<()> {
            let mut f = self.0.lock().unwrap();
            f.log.push(format!("set {id} {target:?} {}x{}{} perm={permanent}", mode.width, mode.height, if mode.hidpi { "@2x" } else { "" }));
            match target {
                Target::Physical => f.displays.iter_mut().find(|d| d.id == id).ok_or_else(|| anyhow!("gone"))?.current = Some(*mode),
                Target::Virtual => f.virtuals.get_mut(id).ok_or_else(|| anyhow!("no virtual"))?.0 = Some(*mode),
            }
            Ok(())
        }
        fn hidpi_supported(&self) -> bool {
            true
        }
        fn enable_hidpi(&mut self, id: &str, plan: &VirtualPlan) -> Result<()> {
            let mut f = self.0.lock().unwrap();
            f.log.push(format!("hidpi on {id} target {}", plan.target));
            if f.fail_hidpi {
                bail!("nope");
            }
            let mut modes: Vec<Mode> = plan.logical.iter().map(|r| m(r.width, r.height, 1)).collect();
            modes.extend(plan.logical.iter().map(|r| m(r.width, r.height, 2)));
            let cur = f.virtuals.get(id).and_then(|v| v.0);
            f.virtuals.insert(id.to_string(), (cur.or(Some(modes[0])), modes));
            Ok(())
        }
        fn disable_hidpi(&mut self, id: &str) -> Result<()> {
            let mut f = self.0.lock().unwrap();
            f.log.push(format!("hidpi off {id}"));
            f.virtuals.remove(id);
            Ok(())
        }
        fn virtual_modes(&mut self, id: &str) -> Option<(Option<Mode>, Vec<Mode>)> {
            self.0.lock().unwrap().virtuals.get(id).cloned()
        }
        fn repair_mirroring(&mut self, ids: &[String]) {
            self.0.lock().unwrap().log.push(format!("repair {ids:?}"));
        }
    }

    fn qhd(id: &str) -> RawDisplay {
        RawDisplay {
            id: id.into(),
            name: "QHD".into(),
            vendor: 0x1e6d,
            model: 0x5b7f,
            current: Some(m(2560, 1440, 1)),
            modes: vec![m(2560, 1440, 1), m(1920, 1080, 1), m(1280, 720, 1)],
            size_mm: (597.0, 336.0),
            ..Default::default()
        }
    }
    fn builtin() -> RawDisplay {
        RawDisplay { id: "Built-in#builtin".into(), name: "Built-in".into(), is_builtin: true, current: Some(m(1512, 982, 2)), modes: vec![m(1512, 982, 2), m(1800, 1169, 2)], ..Default::default() }
    }

    fn setup(displays: Vec<RawDisplay>) -> (FakeBackend, Worker, PathBuf) {
        let fake = FakeBackend::default();
        fake.0.lock().unwrap().displays = displays;
        let dir = std::env::temp_dir().join(format!("display-scaling-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(crate::STATE_FILE);
        let w = Worker::new(Box::new(fake.clone()), Config::default(), Some(path.clone()));
        (fake, w, path)
    }

    fn cfg(entries: &[(&str, DisplayConfig)]) -> Config {
        Config { enabled: true, displays: entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), ..Default::default() }
    }

    fn log(f: &FakeBackend) -> Vec<String> {
        std::mem::take(&mut f.0.lock().unwrap().log)
    }

    #[test]
    fn default_config_changes_nothing() {
        let (f, mut w, path) = setup(vec![qhd("Q#1"), builtin()]);
        w.startup();
        w.handle(Cmd::Update(Config { enabled: true, ..Default::default() }));
        w.stop();
        assert!(log(&f).is_empty());
        assert!(!path.exists());
        let ds = describe_all(&mut f.clone(), &Config::default(), &BTreeMap::new());
        assert!(ds[0].hidpi_capable && !ds[0].native_hidpi && !ds[0].hidpi_active);
        assert!(!ds[1].hidpi_capable && ds[1].native_hidpi);
        assert_eq!(ds[1].scale_percent, Some(200));
        assert_eq!(ds[0].native, Some(Resolution { width: 2560, height: 1440 }));
        assert!(ds[0].suggested_hidpi.contains(&Resolution { width: 1920, height: 1080 }));
    }

    #[test]
    fn hidpi_lifecycle() {
        let (f, mut w, path) = setup(vec![qhd("Q#1"), builtin()]);
        w.startup();
        let on = DisplayConfig { hidpi_enabled: true, target_logical_resolution: Some(Resolution { width: 1920, height: 1080 }), ..Default::default() };
        // builtin is never given a virtual display
        w.handle(Cmd::Update(cfg(&[("Q#1", on.clone()), ("Built-in#builtin", on.clone())])));
        assert_eq!(log(&f), vec!["hidpi on Q#1 target 1920x1080", "set Q#1 Virtual 1920x1080@2x perm=false"]);
        let st: State = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(st.mirrored, vec!["Q#1".to_string()]);
        // same config again: untouched
        w.handle(Cmd::Update(cfg(&[("Q#1", on.clone()), ("Built-in#builtin", on.clone())])));
        assert!(log(&f).is_empty());
        let ds = describe_all(&mut f.clone(), &w.config, &w.plans);
        assert!(ds[0].hidpi_active && ds[0].current.unwrap().hidpi && ds[0].modes.iter().any(|m| m.hidpi && m.width == 2048));
        // a custom resolution rebuilds the plan, and preferred mode picks it
        let custom = DisplayConfig {
            custom_resolutions: vec![Resolution { width: 2200, height: 1238 }],
            preferred_mode: Some(ModeSpec { width: 2200, height: 1238, ..Default::default() }),
            ..on.clone()
        };
        w.handle(Cmd::Update(cfg(&[("Q#1", custom)])));
        assert_eq!(log(&f), vec!["hidpi on Q#1 target 2200x1238", "set Q#1 Virtual 2200x1238@2x perm=false"]);
        // disconnect → virtual torn down; reconnect → re-applied
        let saved = f.0.lock().unwrap().displays.remove(0);
        w.poll();
        assert_eq!(log(&f), vec!["hidpi off Q#1"]);
        assert!(!path.exists());
        f.0.lock().unwrap().displays.push(saved);
        w.poll();
        assert_eq!(log(&f)[0], "hidpi on Q#1 target 2200x1238");
        // turning it off
        w.handle(Cmd::Update(cfg(&[])));
        assert_eq!(log(&f), vec!["hidpi off Q#1"]);
        w.handle(Cmd::Update(cfg(&[("Q#1", on)])));
        log(&f);
        w.stop();
        assert_eq!(log(&f), vec!["hidpi off Q#1"]);
        assert!(!path.exists());
    }

    #[test]
    fn hidpi_failure_is_cleaned_up() {
        let (f, mut w, path) = setup(vec![qhd("Q#1")]);
        f.0.lock().unwrap().fail_hidpi = true;
        w.startup();
        w.handle(Cmd::Update(cfg(&[("Q#1", DisplayConfig { hidpi_enabled: true, ..Default::default() })])));
        assert_eq!(log(&f), vec!["hidpi on Q#1 target 2560x1440", "hidpi off Q#1"]);
        assert!(w.plans.is_empty() && !path.exists());
    }

    #[test]
    fn preferred_mode_and_restore() {
        let (f, mut w, path) = setup(vec![qhd("Q#1")]);
        w.startup();
        let pref = DisplayConfig { preferred_mode: Some(ModeSpec { width: 1920, height: 1080, ..Default::default() }), ..Default::default() };
        w.handle(Cmd::Update(cfg(&[("Q#1", pref.clone())])));
        assert_eq!(log(&f), vec!["set Q#1 Physical 1920x1080 perm=false"]);
        assert!(path.exists());
        w.handle(Cmd::Update(cfg(&[])));
        assert_eq!(log(&f), vec!["set Q#1 Physical 2560x1440 perm=false"]);
        assert!(!path.exists());
        w.handle(Cmd::Update(cfg(&[("Q#1", pref)])));
        log(&f);
        w.stop();
        assert_eq!(log(&f), vec!["set Q#1 Physical 2560x1440 perm=false"]);
    }

    #[test]
    fn crash_recovery() {
        let (f, mut w, path) = setup(vec![qhd("Q#1")]);
        w.startup();
        let pref = DisplayConfig { preferred_mode: Some(ModeSpec { width: 1280, height: 720, ..Default::default() }), ..Default::default() };
        w.handle(Cmd::Update(cfg(&[("Q#1", pref), ("Z#2", DisplayConfig { hidpi_enabled: true, ..Default::default() })])));
        f.0.lock().unwrap().displays.push(qhd("Z#2"));
        w.poll();
        log(&f);
        assert!(path.exists());
        drop(w); // "crash": no stop()
        f.0.lock().unwrap().virtuals.clear(); // the virtual display died with the process
        let mut w2 = Worker::new(Box::new(f.clone()), Config::default(), Some(path.clone()));
        w2.startup();
        assert_eq!(log(&f), vec!["repair [\"Z#2\"]", "set Q#1 Physical 2560x1440 perm=false"]);
        assert!(!path.exists());
    }

    #[test]
    fn revert_flow() {
        let (f, mut w, path) = setup(vec![qhd("Q#1")]);
        w.startup();
        let t0 = Instant::now();
        assert!(w.apply("Q#1", &m(1600, 900, 1), Duration::from_secs(15), false, t0).is_err(), "not offered");
        assert!(w.apply("nope", &m(1920, 1080, 1), Duration::from_secs(15), false, t0).is_err());
        let tok = w.apply("Q#1", &m(1920, 1080, 1), Duration::from_secs(15), false, t0).unwrap();
        assert_eq!(log(&f), vec!["set Q#1 Physical 1920x1080 perm=false"]);
        let st: State = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(st.pending.len(), 1);
        w.revert_expired(t0 + Duration::from_secs(14));
        assert!(log(&f).is_empty());
        w.revert_expired(t0 + Duration::from_secs(15));
        assert_eq!(log(&f), vec!["set Q#1 Physical 2560x1440 perm=false"]);
        assert!(w.confirm(tok).is_err());
        assert!(!path.exists());
        // confirmed + permanent
        let tok = w.apply("Q#1", &m(1280, 720, 1), Duration::from_secs(15), true, t0).unwrap();
        w.confirm(tok).unwrap();
        assert_eq!(log(&f), vec!["set Q#1 Physical 1280x720 perm=false", "set Q#1 Physical 1280x720 perm=true"]);
        w.revert_expired(t0 + Duration::from_secs(60));
        assert!(log(&f).is_empty());
        // pending at stop → reverted
        w.apply("Q#1", &m(1920, 1080, 1), Duration::from_secs(15), false, t0).unwrap();
        w.stop();
        assert_eq!(log(&f), vec!["set Q#1 Physical 1920x1080 perm=false", "set Q#1 Physical 1280x720 perm=false"]);
        assert!(!path.exists());
    }

    #[test]
    fn revert_on_virtual_display() {
        let (f, mut w, _) = setup(vec![qhd("Q#1")]);
        w.startup();
        w.handle(Cmd::Update(cfg(&[("Q#1", DisplayConfig { hidpi_enabled: true, ..Default::default() })])));
        log(&f);
        let t0 = Instant::now();
        let tok = w.apply("Q#1", &m(1920, 1080, 2), Duration::from_secs(5), false, t0).unwrap();
        assert!(w.apply("Q#1", &m(2560, 1440, 1), Duration::from_secs(5), false, t0).is_ok(), "1x modes of the virtual display are offered too");
        w.revert_expired(t0 + Duration::from_secs(6));
        assert_eq!(log(&f), vec!["set Q#1 Virtual 1920x1080@2x perm=false", "set Q#1 Virtual 2560x1440 perm=false", "set Q#1 Virtual 2560x1440@2x perm=false"]);
        assert!(w.confirm(tok).is_err());
    }

    #[test]
    fn handle_end_to_end_with_fake() {
        let fake = FakeBackend::default();
        fake.0.lock().unwrap().displays = vec![qhd("Q#1")];
        let f2 = fake.clone();
        let h = crate::DisplayScaling::start_with(Config::default(), None, move || Box::new(f2) as Box<dyn Backend>).unwrap();
        assert_eq!(h.displays().len(), 1);
        let tok = h.apply_mode_with_timeout("Q#1", &m(1920, 1080, 1), Duration::from_millis(200), false).unwrap();
        assert_eq!(h.pending().len(), 1);
        std::thread::sleep(Duration::from_millis(600));
        assert!(h.pending().is_empty());
        assert!(h.confirm(tok).is_err());
        let tok = h.apply_mode_with_timeout("Q#1", &m(1280, 720, 1), Duration::from_secs(30), false).unwrap();
        h.revert(tok).unwrap();
        h.stop();
        assert_eq!(log(&fake), vec!["set Q#1 Physical 1920x1080 perm=false", "set Q#1 Physical 2560x1440 perm=false", "set Q#1 Physical 1280x720 perm=false", "set Q#1 Physical 2560x1440 perm=false"]);
    }
}
