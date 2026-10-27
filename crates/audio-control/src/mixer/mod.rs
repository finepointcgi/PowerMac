//! Per-app routing and the mixer matrix.
//!
//! * **Routes** (`Config::app_routes`, keyed by bundle id): send an app to
//!   other output devices (real ones or PowerMac virtual devices), instead
//!   of or in addition to its normal output.
//! * **Mixer** (`Config::mixer`): sources (apps, input devices, virtual
//!   devices' loopback) → outputs (devices), each crosspoint with gain and
//!   mute, per-source gain/mute/solo, per-output master, insert effects
//!   (gain, up to 10-band parametric EQ, look-ahead limiter) and meters.
//!
//! ## How it runs (macOS)
//!
//! Routes and the mixer compile into one [`Topology`]: one process tap per
//! app (`CATapMutedWhenTapped` when the app should only be heard through
//! the mixer, `CATapUnmuted` otherwise) and **one private aggregate
//! device** whose clock is the first output, with every other output and
//! input device as drift-compensated sub-devices and the taps in its tap
//! list. A single IOProc on it runs the [`graph::Graph`]: every source and
//! output is in the same callback, so there is no cross-clock buffering in
//! our code (the HAL's drift compensation resamples the other devices).
//! Input streams nobody reads are switched off with
//! `kAudioDevicePropertyIOProcStreamUsage`.
//!
//! The topology is re-planned whenever devices, defaults or the set of
//! audio processes change (the engine already listens for those), so a
//! route comes back by itself when its app restarts or its device
//! reconnects. Only a topology change rebuilds the aggregate; gains, mutes,
//! solo, EQ and limiter changes are lock-free parameter updates.
//!
//! ## Windows
//!
//! There is no mixer on Windows yet: per-app capture would need
//! `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`
//! (Windows 10 2004+) plus our own WASAPI render clients and resampling.
//! Routes use the OS instead: the undocumented-but-stable
//! `IAudioPolicyConfigFactory::SetPersistedDefaultAudioEndpoint` (what the
//! Windows "App volume and device preferences" page and EarTrumpet use)
//! moves an app to one other endpoint. `keep_default`, more than one
//! target and `gain_db` are not possible that way and are reported in the
//! route's state.

pub mod config;
pub mod dsp;
pub mod graph;
pub mod params;
pub mod plan;
pub mod state;

#[cfg(test)]
mod tests;

pub use config::*;
pub use plan::{Inventory, InvApp, InvDevice, Plan, Realized, RealizedSub, Topology};
pub use state::*;

use crate::{AppLevel, Backend, Config};
use anyhow::{anyhow, Result};
use graph::Graph;
use params::{ParamValues, Params};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A platform mixer device (an aggregate + taps on macOS). Dropping it stops
/// IO, destroys what it created and drops the graph, on the dropping
/// (control) thread.
pub trait MixDevice: Send {
    /// The real stream layout of the device, before `start`.
    fn realized(&self) -> &Realized;
    /// Hand the graph to the audio thread and start IO.
    fn start(&mut self, graph: Box<Graph>) -> Result<()>;
    fn diagnostics(&self) -> Vec<String> {
        Vec::new()
    }
}

/// How long to wait before retrying a topology that failed to start.
const RETRY_AFTER: Duration = Duration::from_secs(30);

struct Running {
    topo: Topology,
    params: Arc<Params>,
    device: Box<dyn MixDevice>,
    values: ParamValues,
}

/// Owns the running mixer (control side). Used under a mutex by the engine.
#[derive(Default)]
pub(crate) struct MixerController {
    running: Option<Running>,
    state: MixerState,
    exclusive: BTreeSet<String>,
    inventory: Option<Inventory>,
    failed: Option<(Topology, Instant)>,
}

/// Whether the config asks for the mixer engine at all.
pub fn wanted(cfg: &Config) -> bool {
    cfg.enabled && (cfg.mixer.enabled || cfg.app_routes.values().any(|r| r.enabled))
}

impl MixerController {
    /// Re-plan against a fresh inventory and start / rebuild / update.
    pub(crate) fn reconcile(&mut self, backend: &dyn Backend, cfg: &Config, levels: &BTreeMap<String, AppLevel>) {
        if !backend.mixer_supported() {
            self.stop();
            let mut st = plan::plan(cfg, levels, &Inventory::default(), false).state;
            if let Some(routes) = backend.apply_native_routes(&cfg.app_routes, cfg.enabled) {
                st.routes = routes;
            }
            self.state = st;
            return;
        }
        if !wanted(cfg) {
            self.stop();
            self.failed = None;
            self.state = plan::plan(cfg, levels, self.inventory.as_ref().unwrap_or(&Inventory::default()), true).state;
            return;
        }
        match backend.mix_inventory() {
            Ok(inv) => self.inventory = Some(inv),
            Err(e) => {
                self.stop();
                self.state = MixerState { supported: true, errors: vec![format!("reading audio devices: {e:#}")], ..Default::default() };
                return;
            }
        }
        self.apply_plan(backend, cfg, levels);
    }

    /// Fast path for parameter edits: re-plan against the cached inventory
    /// and only store values. Returns `false` if the change needs a rebuild
    /// (the caller then asks the engine to [`reconcile`](Self::reconcile)).
    pub(crate) fn update_values(&mut self, cfg: &Config, levels: &BTreeMap<String, AppLevel>) -> bool {
        let (Some(inv), Some(run)) = (self.inventory.as_ref(), self.running.as_mut()) else { return !wanted(cfg) && self.running.is_none() };
        let p = plan::plan(cfg, levels, inv, true);
        if p.topology.as_ref() != Some(&run.topo) {
            return false;
        }
        run.params.store(&p.values, Some(&run.values));
        run.values = p.values;
        self.set_state(p.state, Vec::new());
        true
    }

    fn apply_plan(&mut self, backend: &dyn Backend, cfg: &Config, levels: &BTreeMap<String, AppLevel>) {
        let Some(inv) = self.inventory.clone() else { return };
        let p = plan::plan(cfg, levels, &inv, true);
        let mut errors = Vec::new();
        match (&p.topology, self.running.as_mut()) {
            (Some(t), Some(run)) if *t == run.topo => {
                run.params.store(&p.values, Some(&run.values));
                run.values = p.values.clone();
            }
            (topo, _) => {
                self.stop();
                if let Some(t) = topo {
                    let retry_blocked = self.failed.as_ref().is_some_and(|(ft, at)| ft == t && at.elapsed() < RETRY_AFTER);
                    if retry_blocked {
                        errors.push("the mixer failed to start; retrying in a moment".into());
                    } else {
                        match self.start(backend, t.clone(), p.values.clone()) {
                            Ok(()) => {
                                self.failed = None;
                                log::info!(
                                    "audio-control: mixer running: {} source(s), {} output(s), {} tap(s), clock {} @ {} Hz",
                                    t.sources.len(),
                                    t.outputs.len(),
                                    t.taps.len(),
                                    t.clock,
                                    t.sample_rate
                                );
                            }
                            Err(e) => {
                                log::warn!("audio-control: starting the mixer failed: {e:#}");
                                errors.push(format!("starting the mixer failed: {e:#}"));
                                self.failed = Some((t.clone(), Instant::now()));
                            }
                        }
                    }
                }
            }
        }
        self.exclusive = if self.running.is_some() { p.exclusive_apps.clone() } else { BTreeSet::new() };
        self.set_state(p.state, errors);
    }

    fn set_state(&mut self, mut st: MixerState, errors: Vec<String>) {
        st.errors.extend(errors);
        st.running = self.running.is_some();
        if let Some(run) = &self.running {
            st.callbacks = run.params.stats.callbacks.load(Ordering::Relaxed);
            let mism = run.params.stats.layout_mismatches.load(Ordering::Relaxed);
            if mism > 0 {
                st.errors.push(format!("{mism} callback(s) had an unexpected buffer layout"));
            }
        } else {
            for s in st.sources.iter_mut().chain(st.outputs.iter_mut()) {
                if s.state.is_active() {
                    s.state = StripState::waiting("the mixer is not running");
                }
            }
            for r in st.routes.iter_mut() {
                if r.active {
                    r.active = false;
                    r.state = StripState::waiting("the mixer is not running");
                }
            }
        }
        self.state = st;
    }

    fn start(&mut self, backend: &dyn Backend, topo: Topology, values: ParamValues) -> Result<()> {
        let mut dev = backend.mix_open(&topo)?;
        let layout = plan::resolve_layout(&topo, dev.realized())?;
        let rate = if dev.realized().sample_rate > 0.0 { dev.realized().sample_rate } else { topo.sample_rate };
        if (rate - topo.sample_rate).abs() > 0.5 {
            return Err(anyhow!("the mixer device runs at {rate} Hz, expected {} Hz", topo.sample_rate));
        }
        let params = Arc::new(Params::new(topo.sources.len(), topo.outputs.len()));
        params.store(&values, None);
        let graph = Box::new(Graph::new(params.clone(), layout, rate));
        dev.start(graph)?;
        self.running = Some(Running { topo, params, device: dev, values });
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        if let Some(run) = self.running.take() {
            drop(run.device);
            log::info!("audio-control: mixer stopped");
        }
        self.exclusive.clear();
    }

    pub(crate) fn state(&self) -> MixerState {
        let mut st = self.state.clone();
        if let Some(run) = &self.running {
            st.callbacks = run.params.stats.callbacks.load(Ordering::Relaxed);
        }
        st
    }

    /// Apps the running mixer has taken over (their original output muted).
    pub(crate) fn exclusive_apps(&self) -> &BTreeSet<String> {
        &self.exclusive
    }

    pub(crate) fn meters(&self) -> Meters {
        let Some(run) = &self.running else { return Meters::default() };
        let read = |id: &str, m: &dsp::meter::MeterShared| {
            let v = m.read();
            MeterReading {
                id: id.to_string(),
                peak: v.peak,
                rms: v.rms,
                limiter_reduction_db: if v.limiter_gain > 0.0 && v.limiter_gain < 1.0 { -dsp::gain_to_db(v.limiter_gain) } else { 0.0 },
                clips: v.clips,
            }
        };
        Meters {
            sources: run.topo.sources.iter().zip(run.params.sources.iter()).map(|(t, p)| read(&t.key, &p.meter)).collect(),
            outputs: run.topo.outputs.iter().zip(run.params.outputs.iter()).map(|(t, p)| read(&t.key, &p.meter)).collect(),
        }
    }

    pub(crate) fn diagnostics(&self) -> Vec<String> {
        let Some(run) = &self.running else { return Vec::new() };
        let s = &run.params.stats;
        let mut v = vec![format!(
            "mixer: {} source(s), {} output(s), {} tap(s), clock {} @ {} Hz, {} callbacks, max {} frames",
            run.topo.sources.len(),
            run.topo.outputs.len(),
            run.topo.taps.len(),
            run.topo.clock,
            run.topo.sample_rate,
            s.callbacks.load(Ordering::Relaxed),
            s.max_frames.load(Ordering::Relaxed)
        )];
        v.extend(run.device.diagnostics());
        v
    }
}

impl Drop for MixerController {
    fn drop(&mut self) {
        self.stop();
    }
}
