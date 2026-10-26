//! Platform-independent core: the value cache, control-method selection,
//! and the worker thread that talks to the (slow) hardware.
//!
//! Setters update the cache immediately and queue the hardware write; the
//! worker coalesces queued writes (only the latest value per display and
//! feature is sent), spaces DDC writes to one display at least
//! [`MIN_DDC_WRITE_INTERVAL`] apart, retries DDC reads, and re-enumerates
//! when the set of displays changes.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::config::Config;
use crate::ddc::{Feature, MUTE_OFF, MUTE_ON};
use crate::keys::{step_value, Action, MediaKey};
use crate::values::{dim_factor, effective_max, level_to_pct, pct_to_raw, raw_to_pct};
use crate::{BrightnessMethod, DisplayInfo, OsdKind, Rect};

/// Minimum spacing of DDC writes to the same display.
pub(crate) const MIN_DDC_WRITE_INTERVAL: Duration = Duration::from_millis(50);
/// How often the worker checks whether displays were added / removed.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// DDC read attempts per feature.
const READ_ATTEMPTS: u32 = 4;
/// Ignore read-back values this soon after a set (the monitor may not have
/// applied the write yet, or a newer write is queued).
const SET_SHADOW: Duration = Duration::from_secs(2);

/// One display as a backend enumerates it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawDisplay {
    pub id: String,
    pub name: String,
    pub is_builtin: bool,
    pub bounds: Rect,
    /// A DDC channel exists (it may still not answer).
    pub ddc: bool,
    /// Native brightness (DisplayServices / Windows panel IOCTL).
    pub native_brightness: bool,
    /// The gamma table can be set (software dimming).
    pub gamma: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadError {
    Unsupported,
    Transient(String),
}

/// Hardware access. Lives on the worker thread.
pub(crate) trait Backend: Send {
    fn enumerate(&mut self) -> Vec<RawDisplay>;
    /// Cheap value that changes when displays are added, removed or moved.
    fn signature(&mut self) -> u64;
    /// One attempt to read a VCP code: `(current, max)`.
    fn ddc_read(&mut self, id: &str, vcp: u8) -> Result<(u16, u16), ReadError>;
    fn ddc_write(&mut self, id: &str, vcp: u8, value: u16) -> Result<()>;
    /// Native brightness in 0.0–1.0.
    fn native_get(&mut self, id: &str) -> Option<f32>;
    fn native_set(&mut self, id: &str, level: f32) -> Result<()>;
    /// Scale the display's original gamma table by `factor` (1.0 = original).
    fn gamma_set(&mut self, id: &str, factor: f32) -> Result<()>;
    /// Put back every gamma table this backend changed.
    fn gamma_restore(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Val {
    current: u16,
    max: u16,
}

#[derive(Debug)]
struct Entry {
    raw: RawDisplay,
    vals: HashMap<Feature, Val>,
    unsupported: HashSet<Feature>,
    /// The DDC channel exists but never answered.
    ddc_dead: bool,
    /// Software dimming level, percent (100 = not dimmed).
    soft: u8,
    native: Option<u8>,
    /// Volume to restore when mute is emulated with volume 0.
    saved_volume: Option<u16>,
    recent_set: HashMap<Feature, Instant>,
}

impl Entry {
    fn new(raw: RawDisplay) -> Self {
        Self { raw, vals: HashMap::new(), unsupported: HashSet::new(), ddc_dead: false, soft: 100, native: None, saved_volume: None, recent_set: HashMap::new() }
    }

    fn ddc_ok(&self) -> bool {
        self.raw.ddc && !self.ddc_dead
    }

    fn supports(&self, f: Feature) -> bool {
        self.ddc_ok() && !self.unsupported.contains(&f)
    }

    fn max_for(&self, f: Feature, cfg: &Config) -> u16 {
        let o = cfg.override_for(&self.raw.id);
        let ov = match f {
            Feature::Brightness => o.max_brightness,
            Feature::Contrast => o.max_contrast,
            Feature::Volume => o.max_volume,
            _ => 0,
        };
        if ov > 0 {
            ov
        } else {
            effective_max(self.vals.get(&f).map(|v| v.max).unwrap_or(100))
        }
    }

    fn pct(&self, f: Feature, cfg: &Config) -> Option<u8> {
        self.vals.get(&f).map(|v| raw_to_pct(v.current, self.max_for(f, cfg)))
    }

    fn method(&self, cfg: &Config) -> BrightnessMethod {
        let o = cfg.override_for(&self.raw.id);
        if o.force_software_dim && self.raw.gamma {
            BrightnessMethod::Software
        } else if self.raw.native_brightness {
            BrightnessMethod::Native
        } else if self.supports(Feature::Brightness) {
            BrightnessMethod::Ddc
        } else if cfg.software_dim_fallback && self.raw.gamma {
            BrightnessMethod::Software
        } else {
            BrightnessMethod::None
        }
    }

    fn brightness_pct(&self, cfg: &Config) -> Option<u8> {
        match self.method(cfg) {
            BrightnessMethod::Ddc => self.pct(Feature::Brightness, cfg),
            BrightnessMethod::Native => self.native,
            BrightnessMethod::Software => Some(self.soft),
            BrightnessMethod::None => None,
        }
    }

    fn muted(&self) -> Option<bool> {
        if let Some(v) = self.vals.get(&Feature::Mute) {
            return Some(v.current == MUTE_ON);
        }
        if self.saved_volume.is_some() {
            return Some(true);
        }
        self.vals.get(&Feature::Volume).map(|_| false)
    }

    fn info(&self, cfg: &Config) -> DisplayInfo {
        let o = cfg.override_for(&self.raw.id);
        let ddc_val = |f: Feature| if self.supports(f) { self.pct(f, cfg) } else { None };
        let ddc_max = |f: Feature| if self.supports(f) && self.vals.contains_key(&f) { Some(self.max_for(f, cfg)) } else { None };
        let method = self.method(cfg);
        DisplayInfo {
            id: self.raw.id.clone(),
            name: self.raw.name.clone(),
            is_builtin: self.raw.is_builtin,
            enabled: o.enabled,
            bounds: self.raw.bounds,
            supports_ddc: self.ddc_ok() && !self.vals.is_empty(),
            brightness_method: method,
            brightness: self.brightness_pct(cfg),
            contrast: ddc_val(Feature::Contrast),
            volume: ddc_val(Feature::Volume),
            muted: if self.supports(Feature::Volume) { self.muted() } else { None },
            input_source: if self.supports(Feature::InputSource) { self.vals.get(&Feature::InputSource).map(|v| v.current) } else { None },
            max_brightness: if method == BrightnessMethod::Ddc { ddc_max(Feature::Brightness) } else { None },
            max_contrast: ddc_max(Feature::Contrast),
            max_volume: ddc_max(Feature::Volume),
        }
    }
}

pub(crate) struct State {
    pub config: Config,
    displays: Vec<Entry>,
    /// Whether the first enumeration finished.
    ready: bool,
}

#[derive(Debug)]
enum Cmd {
    Ddc { id: String, vcp: u8, value: u16 },
    Native { id: String, level: f32 },
    Gamma { id: String, factor: f32 },
    Refresh { id: Option<String>, done: Option<Sender<()>> },
    Stop,
}

impl Cmd {
    /// Key for coalescing writes.
    fn write_key(&self) -> Option<(String, u16)> {
        match self {
            Cmd::Ddc { id, vcp, .. } => Some((id.clone(), *vcp as u16)),
            Cmd::Native { id, .. } => Some((id.clone(), 0x100)),
            Cmd::Gamma { id, .. } => Some((id.clone(), 0x101)),
            _ => None,
        }
    }
}

/// Shared between the [`crate::Handle`], the key hook and the worker.
pub(crate) struct Inner {
    state: Mutex<State>,
    tx: Mutex<Sender<Cmd>>,
}

/// Result of a key / hotkey action, for the OSD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Feedback {
    pub bounds: Rect,
    pub kind: OsdKind,
    pub pct: u8,
}

impl Inner {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(cmd);
    }

    pub(crate) fn config(&self) -> Config {
        self.lock().config.clone()
    }

    pub(crate) fn displays(&self) -> Vec<DisplayInfo> {
        let st = self.lock();
        st.displays.iter().map(|e| e.info(&st.config)).collect()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.lock().ready
    }

    pub(crate) fn request_refresh(&self, id: Option<String>, done: Option<Sender<()>>) {
        self.send(Cmd::Refresh { id, done });
    }

    pub(crate) fn stop_worker(&self) {
        self.send(Cmd::Stop);
    }

    pub(crate) fn update_config(&self, cfg: Config) {
        let mut st = self.lock();
        st.config = cfg;
        // Undo software dimming where it no longer applies.
        let mut restore = Vec::new();
        let State { config, displays, .. } = &mut *st;
        for e in displays.iter_mut() {
            if e.soft < 100 && (!config.enabled || !config.override_for(&e.raw.id).enabled || e.method(config) != BrightnessMethod::Software) {
                e.soft = 100;
                restore.push(e.raw.id.clone());
            }
        }
        drop(st);
        for id in restore {
            self.send(Cmd::Gamma { id, factor: 1.0 });
        }
    }

    /// Stable id of the enabled display containing the global point.
    pub(crate) fn display_at(&self, x: f64, y: f64) -> Option<String> {
        let st = self.lock();
        st.displays.iter().find(|e| e.raw.bounds.contains(x, y)).map(|e| e.raw.id.clone())
    }

    /// Whether a media key pressed while targeting `id` should be handled
    /// by us rather than the system.
    pub(crate) fn takes_over(&self, id: &str, key: MediaKey) -> bool {
        let st = self.lock();
        let cfg = &st.config;
        if !cfg.enabled {
            return false;
        }
        let Some(e) = st.displays.iter().find(|e| e.raw.id == id) else { return false };
        if e.raw.is_builtin || !cfg.override_for(id).enabled {
            return false;
        }
        if key.is_brightness() {
            cfg.take_over_brightness_keys && e.method(cfg) != BrightnessMethod::None
        } else {
            cfg.take_over_volume_keys && e.supports(Feature::Volume) && e.vals.contains_key(&Feature::Volume)
        }
    }

    /// Set a feature in percent (mute: 0 = off, anything else = on).
    pub(crate) fn set_pct(&self, id: &str, feature: Feature, pct: u8) -> Result<()> {
        let pct = pct.min(100);
        let mut st = self.lock();
        let State { config, displays, .. } = &mut *st;
        let e = displays.iter_mut().find(|e| e.raw.id == id).ok_or_else(|| anyhow!("unknown display {id:?}"))?;
        if !config.enabled || !config.override_for(id).enabled {
            bail!("display control is disabled for {id:?}");
        }
        let now = Instant::now();
        let cmd = match feature {
            Feature::Brightness => match e.method(config) {
                BrightnessMethod::Ddc => {
                    let max = e.max_for(feature, config);
                    let raw = pct_to_raw(pct, max);
                    e.vals.insert(feature, Val { current: raw, max: e.vals.get(&feature).map(|v| v.max).unwrap_or(max) });
                    e.recent_set.insert(feature, now);
                    Cmd::Ddc { id: id.into(), vcp: feature.vcp(), value: raw }
                }
                BrightnessMethod::Native => {
                    e.native = Some(pct);
                    Cmd::Native { id: id.into(), level: pct as f32 / 100.0 }
                }
                BrightnessMethod::Software => {
                    e.soft = pct;
                    Cmd::Gamma { id: id.into(), factor: dim_factor(pct) }
                }
                BrightnessMethod::None => bail!("{id:?} has no brightness control"),
            },
            Feature::Contrast | Feature::Volume => {
                if !e.supports(feature) {
                    bail!("{id:?} does not support {feature:?} over DDC");
                }
                let max = e.max_for(feature, config);
                let raw = pct_to_raw(pct, max);
                e.vals.insert(feature, Val { current: raw, max: e.vals.get(&feature).map(|v| v.max).unwrap_or(max) });
                e.recent_set.insert(feature, now);
                if feature == Feature::Volume {
                    e.saved_volume = None;
                }
                Cmd::Ddc { id: id.into(), vcp: feature.vcp(), value: raw }
            }
            Feature::Mute => {
                drop(st);
                return self.set_mute(id, pct != 0);
            }
            Feature::InputSource => bail!("input source is read-only"),
        };
        drop(st);
        self.send(cmd);
        Ok(())
    }

    pub(crate) fn set_mute(&self, id: &str, on: bool) -> Result<()> {
        let mut st = self.lock();
        let State { config, displays, .. } = &mut *st;
        let e = displays.iter_mut().find(|e| e.raw.id == id).ok_or_else(|| anyhow!("unknown display {id:?}"))?;
        if !config.enabled || !config.override_for(id).enabled {
            bail!("display control is disabled for {id:?}");
        }
        let now = Instant::now();
        let cmd = if e.supports(Feature::Mute) && e.vals.contains_key(&Feature::Mute) {
            let v = if on { MUTE_ON } else { MUTE_OFF };
            e.vals.insert(Feature::Mute, Val { current: v, max: 2 });
            e.recent_set.insert(Feature::Mute, now);
            Cmd::Ddc { id: id.into(), vcp: Feature::Mute.vcp(), value: v }
        } else if e.supports(Feature::Volume) {
            // Emulate mute with volume 0.
            let cur = e.vals.get(&Feature::Volume).copied();
            let target = if on {
                if e.saved_volume.is_none() {
                    e.saved_volume = Some(cur.map(|v| v.current).unwrap_or(0));
                }
                0
            } else {
                match e.saved_volume.take() {
                    Some(v) => v,
                    None => return Ok(()),
                }
            };
            e.vals.insert(Feature::Volume, Val { current: target, max: cur.map(|v| v.max).unwrap_or(100) });
            e.recent_set.insert(Feature::Volume, now);
            Cmd::Ddc { id: id.into(), vcp: Feature::Volume.vcp(), value: target }
        } else {
            bail!("{id:?} has no DDC volume control");
        };
        drop(st);
        self.send(cmd);
        Ok(())
    }

    /// Perform a key / hotkey action on display `id`.
    pub(crate) fn perform(&self, id: &str, action: Action) -> Option<Feedback> {
        let (cur, step, bounds, muted) = {
            let st = self.lock();
            let e = st.displays.iter().find(|e| e.raw.id == id)?;
            let cur = match action.feature() {
                Feature::Brightness => e.brightness_pct(&st.config),
                Feature::Contrast | Feature::Volume if e.supports(action.feature()) => e.pct(action.feature(), &st.config),
                _ => None,
            };
            (cur, st.config.step_for(id), e.raw.bounds, e.muted())
        };
        match action {
            Action::Adjust { feature, up, fine } => {
                let Some(cur) = cur else {
                    // Values not read yet (or the read failed): try again.
                    self.request_refresh(Some(id.to_string()), None);
                    return None;
                };
                if feature == Feature::Volume && muted == Some(true) {
                    let _ = self.set_mute(id, false);
                }
                let cur_now = if feature == Feature::Volume { self.displays().into_iter().find(|d| d.id == id).and_then(|d| d.volume).unwrap_or(cur) } else { cur };
                let next = step_value(cur_now, step, up, fine);
                if let Err(e) = self.set_pct(id, feature, next) {
                    log::debug!("display-control: {e:#}");
                    return None;
                }
                let kind = match feature {
                    Feature::Brightness => OsdKind::Brightness,
                    Feature::Contrast => OsdKind::Contrast,
                    _ if next == 0 => OsdKind::Muted,
                    _ => OsdKind::Volume,
                };
                Some(Feedback { bounds, kind, pct: next })
            }
            Action::ToggleMute => {
                let on = !muted.unwrap_or(false);
                if let Err(e) = self.set_mute(id, on) {
                    log::debug!("display-control: {e:#}");
                    return None;
                }
                let vol = self.displays().into_iter().find(|d| d.id == id).and_then(|d| d.volume).unwrap_or(0);
                Some(Feedback { bounds, kind: if on { OsdKind::Muted } else { OsdKind::Volume }, pct: if on { 0 } else { vol } })
            }
        }
    }
}

/// Start the worker; returns once the first enumeration is done (or after
/// `wait` at most), so `displays()` lists the displays right away.
pub(crate) fn start(config: Config, make_backend: Box<dyn FnOnce() -> Box<dyn Backend> + Send>, wait: Duration) -> Result<(Arc<Inner>, std::thread::JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let inner = Arc::new(Inner { state: Mutex::new(State { config, displays: Vec::new(), ready: false }), tx: Mutex::new(tx) });
    let (ready_tx, ready_rx) = mpsc::channel();
    let w = inner.clone();
    let thread = std::thread::Builder::new().name("display-control".into()).spawn(move || {
        let backend = make_backend();
        worker(w, backend, rx, ready_tx);
    })?;
    let _ = ready_rx.recv_timeout(wait);
    Ok((inner, thread))
}

fn worker(inner: Arc<Inner>, mut backend: Box<dyn Backend>, rx: Receiver<Cmd>, ready: Sender<()>) {
    // Enumerate first (fast) so displays() is useful immediately, then read
    // the DDC values (slow).
    let mut sig = backend.signature();
    enumerate(&inner, backend.as_mut());
    let _ = ready.send(());
    read_values(&inner, backend.as_mut(), None);
    let mut last_write: HashMap<String, Instant> = HashMap::new();
    loop {
        let first = match rx.recv_timeout(POLL_INTERVAL) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut cmds: Vec<Cmd> = first.into_iter().collect();
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        let idle = cmds.is_empty();
        let (writes, refreshes, stop) = coalesce(cmds);
        for cmd in writes {
            apply_write(backend.as_mut(), cmd, &mut last_write);
        }
        if stop {
            break;
        }
        let mut full = false;
        if idle {
            let s = backend.signature();
            if s != sig {
                sig = s;
                full = true;
            }
        }
        let mut ids: Vec<String> = Vec::new();
        let mut waiters = Vec::new();
        for (id, done) in refreshes {
            match id {
                None => full = true,
                Some(id) => {
                    if !ids.contains(&id) {
                        ids.push(id)
                    }
                }
            }
            waiters.extend(done);
        }
        if full {
            sig = backend.signature();
            enumerate(&inner, backend.as_mut());
            read_values(&inner, backend.as_mut(), None);
        } else {
            for id in ids {
                read_values(&inner, backend.as_mut(), Some(&id));
            }
        }
        for w in waiters {
            let _ = w.send(());
        }
    }
    backend.gamma_restore();
    log::info!("display-control: worker stopped");
}

type Refresh = (Option<String>, Option<Sender<()>>);

/// Keep only the latest write per (display, feature), in first-seen order.
fn coalesce(cmds: Vec<Cmd>) -> (Vec<Cmd>, Vec<Refresh>, bool) {
    let mut writes: Vec<Cmd> = Vec::new();
    let mut refreshes = Vec::new();
    let mut stop = false;
    for c in cmds {
        if let Some(k) = c.write_key() {
            if let Some(slot) = writes.iter_mut().find(|w| w.write_key().as_ref() == Some(&k)) {
                *slot = c;
            } else {
                writes.push(c);
            }
            continue;
        }
        match c {
            Cmd::Refresh { id, done } => refreshes.push((id, done)),
            Cmd::Stop => stop = true,
            _ => {}
        }
    }
    (writes, refreshes, stop)
}

fn apply_write(backend: &mut dyn Backend, cmd: Cmd, last_write: &mut HashMap<String, Instant>) {
    let res = match &cmd {
        Cmd::Ddc { id, vcp, value } => {
            if let Some(t) = last_write.get(id) {
                let since = t.elapsed();
                if since < MIN_DDC_WRITE_INTERVAL {
                    std::thread::sleep(MIN_DDC_WRITE_INTERVAL - since);
                }
            }
            let r = backend.ddc_write(id, *vcp, *value);
            last_write.insert(id.clone(), Instant::now());
            r
        }
        Cmd::Native { id, level } => backend.native_set(id, *level),
        Cmd::Gamma { id, factor } => backend.gamma_set(id, *factor),
        _ => Ok(()),
    };
    if let Err(e) = res {
        log::warn!("display-control: {cmd:?} failed: {e:#}");
    }
}

fn enumerate(inner: &Inner, backend: &mut dyn Backend) {
    let raws = backend.enumerate();
    let mut st = inner.lock();
    let mut old: Vec<Entry> = std::mem::take(&mut st.displays);
    let mut reapply = Vec::new();
    for raw in raws {
        let entry = match old.iter().position(|e| e.raw.id == raw.id) {
            Some(i) => {
                let mut e = old.swap_remove(i);
                if e.raw.ddc != raw.ddc {
                    e.ddc_dead = false;
                }
                e.raw = raw;
                e
            }
            None => {
                log::info!(
                    "display-control: found {:?} (builtin {}, ddc {}, native {}, gamma {})",
                    raw.id,
                    raw.is_builtin,
                    raw.ddc,
                    raw.native_brightness,
                    raw.gamma
                );
                Entry::new(raw)
            }
        };
        // Display reconfiguration resets gamma tables.
        if entry.soft < 100 {
            reapply.push((entry.raw.id.clone(), dim_factor(entry.soft)));
        }
        st.displays.push(entry);
    }
    st.ready = true;
    drop(st);
    for (id, f) in reapply {
        if let Err(e) = backend.gamma_set(&id, f) {
            log::debug!("display-control: re-applying dimming to {id:?}: {e:#}");
        }
    }
}

const READ_ORDER: [Feature; 5] = [Feature::Brightness, Feature::Contrast, Feature::Volume, Feature::Mute, Feature::InputSource];

/// Read a VCP code with retries.
pub(crate) fn read_with_retries(backend: &mut dyn Backend, id: &str, vcp: u8) -> Result<(u16, u16), ReadError> {
    let mut last = ReadError::Transient("no attempt".into());
    for attempt in 0..READ_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(40 * attempt as u64));
        }
        match backend.ddc_read(id, vcp) {
            Ok(v) => return Ok(v),
            Err(ReadError::Unsupported) => return Err(ReadError::Unsupported),
            Err(e) => last = e,
        }
    }
    Err(last)
}

type ReadResult = Result<(u16, u16), ReadError>;

fn read_values(inner: &Inner, backend: &mut dyn Backend, only: Option<&str>) {
    let targets: Vec<(String, bool, bool)> = {
        let st = inner.lock();
        st.displays
            .iter()
            .filter(|e| only.is_none_or(|id| e.raw.id == id))
            .filter(|e| st.config.override_for(&e.raw.id).enabled)
            .map(|e| (e.raw.id.clone(), e.raw.ddc, e.raw.native_brightness))
            .collect()
    };
    for (id, ddc, native) in targets {
        let native_level = if native { backend.native_get(&id) } else { None };
        let mut results: Vec<(Feature, ReadResult)> = Vec::new();
        let mut dead = false;
        if ddc {
            for f in READ_ORDER {
                let r = read_with_retries(backend, &id, f.vcp());
                let first_failed = f == Feature::Brightness && matches!(r, Err(ReadError::Transient(_)));
                results.push((f, r));
                if first_failed {
                    // Try one more feature before declaring the channel dead
                    // (a few monitors don't answer brightness reads).
                    let r = read_with_retries(backend, &id, Feature::Volume.vcp());
                    dead = matches!(r, Err(ReadError::Transient(_)));
                    results.push((Feature::Volume, r));
                    break;
                }
            }
        }
        let mut st = inner.lock();
        let Some(e) = st.displays.iter_mut().find(|e| e.raw.id == id) else { continue };
        if let Some(l) = native_level {
            e.native = Some(level_to_pct(l));
        }
        if ddc {
            e.ddc_dead = dead;
            if dead {
                log::info!("display-control: {id:?} does not answer DDC/CI");
            }
            for (f, r) in results {
                let shadowed = e.recent_set.get(&f).is_some_and(|t| t.elapsed() < SET_SHADOW);
                match r {
                    Ok((current, max)) => {
                        e.unsupported.remove(&f);
                        if !shadowed {
                            e.vals.insert(f, Val { current, max });
                        }
                    }
                    Err(ReadError::Unsupported) => {
                        e.unsupported.insert(f);
                        e.vals.remove(&f);
                    }
                    Err(ReadError::Transient(msg)) => {
                        log::debug!("display-control: reading {f:?} of {id:?}: {msg}");
                    }
                }
            }
            // A feature that keeps failing while others work is treated as unsupported.
            for f in READ_ORDER {
                if !dead && !e.vals.contains_key(&f) && !e.unsupported.contains(&f) && e.vals.contains_key(&Feature::Brightness) {
                    e.unsupported.insert(f);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A scripted backend for tests.
    #[derive(Default)]
    pub struct FakeHw {
        pub displays: Vec<RawDisplay>,
        /// (id, vcp) → (current, max); missing = unsupported.
        pub vcp: HashMap<(String, u8), (u16, u16)>,
        pub fail_reads: u32,
        pub dead: HashSet<String>,
        pub writes: Vec<(String, u8, u16)>,
        pub native: HashMap<String, f32>,
        pub gamma: HashMap<String, f32>,
        pub restored: bool,
        pub sig: u64,
    }

    pub struct FakeBackend(pub Arc<Mutex<FakeHw>>);

    impl Backend for FakeBackend {
        fn enumerate(&mut self) -> Vec<RawDisplay> {
            self.0.lock().unwrap().displays.clone()
        }
        fn signature(&mut self) -> u64 {
            self.0.lock().unwrap().sig
        }
        fn ddc_read(&mut self, id: &str, vcp: u8) -> Result<(u16, u16), ReadError> {
            let mut hw = self.0.lock().unwrap();
            if hw.dead.contains(id) {
                return Err(ReadError::Transient("no answer".into()));
            }
            if hw.fail_reads > 0 {
                hw.fail_reads -= 1;
                return Err(ReadError::Transient("checksum".into()));
            }
            hw.vcp.get(&(id.to_string(), vcp)).copied().ok_or(ReadError::Unsupported)
        }
        fn ddc_write(&mut self, id: &str, vcp: u8, value: u16) -> Result<()> {
            let mut hw = self.0.lock().unwrap();
            hw.writes.push((id.into(), vcp, value));
            if let Some(v) = hw.vcp.get_mut(&(id.to_string(), vcp)) {
                v.0 = value;
            }
            Ok(())
        }
        fn native_get(&mut self, id: &str) -> Option<f32> {
            self.0.lock().unwrap().native.get(id).copied()
        }
        fn native_set(&mut self, id: &str, level: f32) -> Result<()> {
            self.0.lock().unwrap().native.insert(id.into(), level);
            Ok(())
        }
        fn gamma_set(&mut self, id: &str, factor: f32) -> Result<()> {
            self.0.lock().unwrap().gamma.insert(id.into(), factor);
            Ok(())
        }
        fn gamma_restore(&mut self) {
            let mut hw = self.0.lock().unwrap();
            hw.gamma.clear();
            hw.restored = true;
        }
    }

    pub fn raw(id: &str, builtin: bool, ddc: bool, native: bool, x: i32) -> RawDisplay {
        RawDisplay { id: id.into(), name: id.into(), is_builtin: builtin, bounds: Rect { x, y: 0, width: 1000, height: 800 }, ddc, native_brightness: native, gamma: true }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use crate::config::{DisplayOverride, Mods};

    fn setup(hw: FakeHw, cfg: Config) -> (Arc<Inner>, Arc<Mutex<FakeHw>>, std::thread::JoinHandle<()>) {
        let hw = Arc::new(Mutex::new(hw));
        let h2 = hw.clone();
        let (inner, t) = start(cfg, Box::new(move || Box::new(FakeBackend(h2)) as Box<dyn Backend>), Duration::from_secs(5)).unwrap();
        // Wait for the initial reads.
        let (tx, rx) = mpsc::channel();
        inner.request_refresh(Some("__none__".into()), Some(tx));
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        (inner, hw, t)
    }

    fn sync(inner: &Inner) {
        let (tx, rx) = mpsc::channel();
        inner.request_refresh(Some("__none__".into()), Some(tx));
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    fn standard_hw() -> FakeHw {
        let mut hw = FakeHw { displays: vec![raw("Built-in#builtin", true, false, true, 0), raw("DELL#1", false, true, false, 1000), raw("TV#2", false, true, false, 2000), raw("Old#3", false, false, false, 3000)], ..Default::default() };
        hw.native.insert("Built-in#builtin".into(), 0.5);
        for (vcp, v) in [(0x10, (40, 100)), (0x12, (75, 100)), (0x62, (30, 100)), (0x8D, (2, 2)), (0x60, (0x0F, 0x12))] {
            hw.vcp.insert(("DELL#1".into(), vcp), v);
        }
        // A TV that answers brightness (0..255) and volume but not mute.
        hw.vcp.insert(("TV#2".into(), 0x10), (255, 255));
        hw.vcp.insert(("TV#2".into(), 0x62), (20, 50));
        hw.fail_reads = 3; // flaky first reads
        hw
    }

    #[test]
    fn enumerates_reads_and_selects_methods() {
        let (inner, _hw, t) = setup(standard_hw(), Config::default());
        let d = inner.displays();
        assert_eq!(d.len(), 4);
        let b = &d[0];
        assert!(b.is_builtin && !b.supports_ddc);
        assert_eq!((b.brightness_method, b.brightness), (BrightnessMethod::Native, Some(50)));
        let dell = &d[1];
        assert!(dell.supports_ddc);
        assert_eq!(dell.brightness_method, BrightnessMethod::Ddc);
        assert_eq!((dell.brightness, dell.contrast, dell.volume, dell.muted, dell.input_source), (Some(40), Some(75), Some(30), Some(false), Some(0x0F)));
        assert_eq!(dell.max_brightness, Some(100));
        let tv = &d[2];
        assert_eq!((tv.brightness, tv.volume, tv.contrast, tv.max_volume), (Some(100), Some(40), None, Some(50)));
        let old = &d[3];
        assert_eq!((old.brightness_method, old.brightness, old.supports_ddc), (BrightnessMethod::Software, Some(100), false));
        inner.stop_worker();
        t.join().unwrap();
    }

    #[test]
    fn writes_are_scaled_coalesced_and_restored() {
        let (inner, hw, t) = setup(standard_hw(), Config::default());
        // Many quick slider moves → far fewer writes, last one wins.
        for p in 0..=60u8 {
            inner.set_pct("TV#2", Feature::Brightness, p).unwrap();
        }
        inner.set_pct("TV#2", Feature::Volume, 50).unwrap();
        sync(&inner);
        {
            let hw = hw.lock().unwrap();
            let tv: Vec<_> = hw.writes.iter().filter(|w| w.0 == "TV#2" && w.1 == 0x10).collect();
            assert!(tv.len() < 30, "coalesced: {}", tv.len());
            assert_eq!(tv.last().unwrap().2, 153, "60 % of 255");
            assert!(hw.writes.contains(&("TV#2".into(), 0x62, 25)), "50 % of max 50");
        }
        assert_eq!(inner.displays()[2].brightness, Some(60));
        assert!(inner.set_pct("TV#2", Feature::Contrast, 10).is_err(), "unsupported");
        assert!(inner.set_pct("nope", Feature::Brightness, 10).is_err());
        assert!(inner.set_pct("DELL#1", Feature::InputSource, 10).is_err());
        // Native + software paths.
        inner.set_pct("Built-in#builtin", Feature::Brightness, 80).unwrap();
        inner.set_pct("Old#3", Feature::Brightness, 0).unwrap();
        sync(&inner);
        {
            let hw = hw.lock().unwrap();
            assert_eq!(hw.native["Built-in#builtin"], 0.8);
            assert!((hw.gamma["Old#3"] - dim_factor(0)).abs() < 1e-6);
        }
        inner.stop_worker();
        t.join().unwrap();
        assert!(hw.lock().unwrap().restored, "gamma restored on stop");
    }

    #[test]
    fn mute_real_and_emulated() {
        let (inner, hw, t) = setup(standard_hw(), Config::default());
        inner.set_mute("DELL#1", true).unwrap();
        assert_eq!(inner.displays()[1].muted, Some(true));
        // TV has no 0x8D: mute = volume 0, unmute restores 20.
        inner.set_mute("TV#2", true).unwrap();
        assert_eq!(inner.displays()[2].muted, Some(true));
        assert_eq!(inner.displays()[2].volume, Some(0));
        inner.set_mute("TV#2", false).unwrap();
        assert_eq!(inner.displays()[2].volume, Some(40));
        sync(&inner);
        let hw2 = hw.lock().unwrap();
        assert!(hw2.writes.contains(&("DELL#1".into(), 0x8D, MUTE_ON)));
        assert_eq!(hw2.writes.iter().rfind(|w| w.0 == "TV#2" && w.1 == 0x62).unwrap().2, 20);
        drop(hw2);
        assert!(inner.set_mute("Old#3", true).is_err());
        inner.stop_worker();
        t.join().unwrap();
    }

    #[test]
    fn key_routing_and_actions() {
        let cfg = Config { take_over_brightness_keys: true, take_over_volume_keys: true, ..Default::default() };
        let (inner, _hw, t) = setup(standard_hw(), cfg.clone());
        assert_eq!(inner.display_at(1500.0, 10.0).as_deref(), Some("DELL#1"));
        assert_eq!(inner.display_at(-5.0, 10.0), None);
        assert!(!inner.takes_over("Built-in#builtin", MediaKey::BrightnessUp));
        assert!(inner.takes_over("DELL#1", MediaKey::BrightnessUp));
        assert!(inner.takes_over("Old#3", MediaKey::BrightnessDown), "software dimming");
        assert!(inner.takes_over("DELL#1", MediaKey::VolumeUp));
        assert!(!inner.takes_over("Old#3", MediaKey::VolumeUp));
        let fb = inner.perform("DELL#1", Action::for_media_key(MediaKey::BrightnessUp, Mods::default())).unwrap();
        assert_eq!((fb.kind, fb.pct), (OsdKind::Brightness, 42));
        let fb = inner.perform("DELL#1", Action::ToggleMute).unwrap();
        assert_eq!(fb.kind, OsdKind::Muted);
        // Volume up while muted unmutes and steps.
        let fb = inner.perform("DELL#1", Action::for_media_key(MediaKey::VolumeUp, Mods::default())).unwrap();
        assert_eq!((fb.kind, fb.pct), (OsdKind::Volume, 36));
        assert_eq!(inner.displays()[1].muted, Some(false));
        // Overrides: disabled display / keys off.
        let mut c2 = cfg.clone();
        c2.displays.insert("DELL#1".into(), DisplayOverride { enabled: false, ..Default::default() });
        c2.take_over_volume_keys = false;
        inner.update_config(c2);
        assert!(!inner.takes_over("DELL#1", MediaKey::BrightnessUp));
        assert!(!inner.takes_over("TV#2", MediaKey::VolumeUp));
        assert!(inner.set_pct("DELL#1", Feature::Brightness, 1).is_err());
        inner.stop_worker();
        t.join().unwrap();
    }

    #[test]
    fn dead_ddc_falls_back_to_software_and_overrides_apply() {
        let mut hw = standard_hw();
        hw.dead.insert("TV#2".into());
        let mut cfg = Config::default();
        cfg.displays.insert("DELL#1".into(), DisplayOverride { max_brightness: 50, force_software_dim: false, ..Default::default() });
        let (inner, hw, t) = setup(hw, cfg.clone());
        let d = inner.displays();
        assert!(!d[2].supports_ddc);
        assert_eq!(d[2].brightness_method, BrightnessMethod::Software);
        assert_eq!(d[1].brightness, Some(80), "40 of an overridden max 50");
        inner.set_pct("DELL#1", Feature::Brightness, 100).unwrap();
        sync(&inner);
        assert_eq!(hw.lock().unwrap().writes.last().unwrap(), &("DELL#1".to_string(), 0x10, 50));
        // Software dimming is undone when the fallback is switched off.
        inner.set_pct("TV#2", Feature::Brightness, 10).unwrap();
        sync(&inner);
        assert!(hw.lock().unwrap().gamma["TV#2"] < 0.5);
        let mut c2 = cfg;
        c2.software_dim_fallback = false;
        inner.update_config(c2);
        sync(&inner);
        assert_eq!(hw.lock().unwrap().gamma["TV#2"], 1.0);
        assert_eq!(inner.displays()[2].brightness_method, BrightnessMethod::None);
        inner.stop_worker();
        t.join().unwrap();
    }

    #[test]
    fn hotplug_reenumerates() {
        let (inner, hw, t) = setup(standard_hw(), Config::default());
        {
            let mut h = hw.lock().unwrap();
            h.displays.retain(|d| d.id != "TV#2");
            h.sig += 1;
        }
        inner.request_refresh(None, None);
        sync(&inner);
        assert_eq!(inner.displays().len(), 3);
        inner.stop_worker();
        t.join().unwrap();
    }

    #[test]
    fn coalesce_keeps_latest() {
        let cmds = vec![
            Cmd::Ddc { id: "a".into(), vcp: 0x10, value: 1 },
            Cmd::Ddc { id: "a".into(), vcp: 0x62, value: 5 },
            Cmd::Refresh { id: None, done: None },
            Cmd::Ddc { id: "a".into(), vcp: 0x10, value: 2 },
            Cmd::Gamma { id: "a".into(), factor: 0.5 },
            Cmd::Stop,
        ];
        let (w, r, stop) = coalesce(cmds);
        assert!(stop);
        assert_eq!(r.len(), 1);
        assert_eq!(w.len(), 3);
        assert!(matches!(&w[0], Cmd::Ddc { vcp: 0x10, value: 2, .. }));
        assert!(matches!(&w[1], Cmd::Ddc { vcp: 0x62, value: 5, .. }));
    }
}
