//! Planning: config + what exists right now → what to build.
//!
//! Pure logic over an [`Inventory`] (devices with their stream layouts, apps
//! with their Core Audio process objects), so routing-rule resolution,
//! solo/mute math and channel mapping are unit-tested with fake devices.
//! The platform layer turns a [`Topology`] into taps + one private
//! aggregate device and reports the stream order it really got
//! ([`Realized`]); [`resolve_layout`] maps strips onto those streams.

use super::config::{AppRoute, Effects, MixSource, Mixer, SourceKind, MAX_OUTPUTS, MAX_SOURCES};
use super::dsp::biquad::{Coeffs, MAX_BANDS};
use super::dsp::db_to_gain;
use super::graph::{ChanLoc, Layout};
use super::params::{ParamValues, StripValues};
use super::state::{MixerState, RouteStatus, RouteTarget, StripKind, StripState, StripStatus};
use crate::{AppLevel, AudioDevice, CapturePermission, Config, DeviceMatch, Transport};
use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// A device as the mixer sees it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvDevice {
    pub uid: String,
    pub name: String,
    pub transport: Transport,
    /// Channels of each input stream, in stream order.
    pub input_streams: Vec<u32>,
    /// Channels of each output stream.
    pub output_streams: Vec<u32>,
    pub sample_rate: f64,
}

impl InvDevice {
    pub fn input_channels(&self) -> u32 {
        self.input_streams.iter().sum()
    }

    pub fn output_channels(&self) -> u32 {
        self.output_streams.iter().sum()
    }

    fn as_audio_device(&self) -> AudioDevice {
        AudioDevice { uid: self.uid.clone(), name: self.name.clone(), transport: self.transport, ..Default::default() }
    }
}

/// A running app with audio processes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvApp {
    pub bundle_id: String,
    pub name: String,
    /// Core Audio process objects (what a tap is made of).
    pub objects: Vec<u32>,
    pub pids: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inventory {
    pub devices: Vec<InvDevice>,
    pub apps: Vec<InvApp>,
    /// Process taps work here (macOS 14.4+).
    pub taps_supported: bool,
    pub capture_permission: CapturePermission,
}

impl Default for Inventory {
    fn default() -> Self {
        Inventory { devices: Vec::new(), apps: Vec::new(), taps_supported: false, capture_permission: CapturePermission::NotApplicable }
    }
}

impl Inventory {
    /// uid first, then name (the rules' [`DeviceMatch`] semantics).
    pub fn resolve(&self, m: &DeviceMatch) -> Option<&InvDevice> {
        self.devices
            .iter()
            .find(|d| m.matches_uid(&d.as_audio_device()))
            .or_else(|| self.devices.iter().find(|d| m.matches_name(&d.as_audio_device())))
    }

    pub fn app(&self, bundle_id: &str) -> Option<&InvApp> {
        self.apps.iter().find(|a| a.bundle_id == bundle_id && !a.objects.is_empty())
    }
}

/// A process tap to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapSpec {
    pub bundle_id: String,
    /// Sorted, deduplicated process objects.
    pub objects: Vec<u32>,
    /// `CATapMutedWhenTapped`: the app is heard only through the mixer
    /// (and again normally if the mixer stops). Otherwise `CATapUnmuted`.
    pub mute_original: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubDevice {
    pub uid: String,
    /// The mixer reads its input streams.
    pub input: bool,
    /// The mixer writes its output streams.
    pub output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceInput {
    /// Index into [`Topology::taps`].
    Tap(usize),
    Device { uid: String, first_channel: u32, mono: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoSource {
    pub key: String,
    pub input: SourceInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoOutput {
    pub key: String,
    pub uid: String,
    pub first_channel: u32,
}

/// The structure of the running graph. Any change here means rebuilding
/// the aggregate device; everything else is a parameter update.
#[derive(Debug, Clone, PartialEq)]
pub struct Topology {
    /// Clock / main sub-device.
    pub clock: String,
    pub sample_rate: f64,
    /// Clock first.
    pub subdevices: Vec<SubDevice>,
    pub taps: Vec<TapSpec>,
    pub sources: Vec<TopoSource>,
    pub outputs: Vec<TopoOutput>,
}

/// Result of [`plan`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub topology: Option<Topology>,
    pub values: ParamValues,
    pub state: MixerState,
    /// Apps whose original output the mixer mutes: per-app volume must not
    /// make its own scaled copy of them on the default output.
    pub exclusive_apps: BTreeSet<String>,
}

/// Linear fader gain of a mixer source: fader × mute × solo × app level.
pub fn source_gain(src: &MixSource, any_solo: bool, level: Option<&AppLevel>) -> f32 {
    if src.muted || (any_solo && !src.solo) {
        return 0.0;
    }
    db_to_gain(src.gain_db) * app_level_gain(level)
}

pub fn app_level_gain(level: Option<&AppLevel>) -> f32 {
    match level {
        Some(l) if l.muted => 0.0,
        Some(l) => l.volume.clamp(0.0, crate::MAX_APP_VOLUME),
        None => 1.0,
    }
}

/// Whether any source of an (enabled) mixer is soloed.
pub fn any_solo(m: &Mixer) -> bool {
    m.sources.iter().any(|s| s.solo)
}

pub fn effect_values(e: &Effects, gain: f32, sample_rate: f64) -> StripValues {
    let mut eq = [None; MAX_BANDS];
    if e.eq.enabled {
        for (slot, b) in eq.iter_mut().zip(e.eq.bands.iter().filter(|b| b.enabled)) {
            let c = Coeffs::design(b.kind, sample_rate, b.freq_hz as f64, b.gain_db as f64, b.q as f64);
            if !c.is_identity() {
                *slot = Some(c);
            }
        }
    }
    StripValues {
        gain,
        trim: db_to_gain(e.gain_db),
        eq,
        limiter_enabled: e.limiter.enabled,
        ceiling_db: e.limiter.ceiling_db,
        release_ms: e.limiter.release_ms,
        lookahead_ms: e.limiter.lookahead_ms,
    }
}

pub const ROUTE_SOURCE_PREFIX: &str = "route:";
pub const ROUTE_OUTPUT_PREFIX: &str = "route-out:";

/// Why a device can't be an output (None = it can).
fn output_problem(d: &InvDevice, first_channel: u32) -> Option<String> {
    if d.output_channels() == 0 {
        return Some(format!("{} has no outputs", d.name));
    }
    if d.transport == Transport::Aggregate {
        return Some(format!("{} is an aggregate device (they can't be nested)", d.name));
    }
    if first_channel >= d.output_channels() {
        return Some(format!("{} has only {} output channels", d.name, d.output_channels()));
    }
    None
}

fn input_problem(d: &InvDevice, first_channel: u32) -> Option<String> {
    if d.input_channels() == 0 {
        return Some(format!("{} has no inputs", d.name));
    }
    if d.transport == Transport::Aggregate {
        return Some(format!("{} is an aggregate device (they can't be nested)", d.name));
    }
    if first_channel >= d.input_channels() {
        return Some(format!("{} has only {} input channels", d.name, d.input_channels()));
    }
    None
}

fn rate_problem(d: &InvDevice, rate: f64) -> Option<String> {
    ((d.sample_rate - rate).abs() > 0.5 && d.sample_rate > 0.0)
        .then(|| format!("{} runs at {} Hz and the mixer at {} Hz (set them to the same rate in Audio MIDI Setup)", d.name, d.sample_rate, rate))
}

fn app_problem(inv: &Inventory, bundle_id: &str) -> Option<StripState> {
    if bundle_id.is_empty() {
        return Some(StripState::unavailable("no app chosen"));
    }
    if !inv.taps_supported {
        return Some(StripState::unavailable("app audio needs macOS 14.4 or later"));
    }
    if inv.capture_permission == CapturePermission::Denied {
        return Some(StripState::unavailable(
            "System Audio Recording is off for PowerMac (System Settings → Privacy & Security → Screen & System Audio Recording)",
        ));
    }
    if inv.app(bundle_id).is_none() {
        return Some(StripState::waiting("not running"));
    }
    None
}

fn device_label(m: &DeviceMatch) -> String {
    if m.name.is_empty() {
        m.uid.clone()
    } else {
        m.name.clone()
    }
}

/// Builder state.
struct Builder<'a> {
    inv: &'a Inventory,
    taps: Vec<TapSpec>,
    sources: Vec<(TopoSource, StripValues)>,
    outputs: Vec<(TopoOutput, StripValues)>,
    sends: Vec<(usize, usize, f32)>,
}

impl Builder<'_> {
    fn tap(&mut self, bundle_id: &str, mute_original: bool) -> usize {
        if let Some(i) = self.taps.iter().position(|t| t.bundle_id == bundle_id) {
            self.taps[i].mute_original |= mute_original;
            return i;
        }
        let mut objects = self.inv.app(bundle_id).map(|a| a.objects.clone()).unwrap_or_default();
        objects.sort_unstable();
        objects.dedup();
        self.taps.push(TapSpec { bundle_id: bundle_id.to_string(), objects, mute_original });
        self.taps.len() - 1
    }
}

/// Plan the mixer and the app routes.
pub fn plan(cfg: &Config, levels: &BTreeMap<String, AppLevel>, inv: &Inventory, platform_supported: bool) -> Plan {
    let mut st = MixerState {
        supported: platform_supported,
        app_sources_supported: inv.taps_supported,
        capture_permission: inv.capture_permission,
        ..Default::default()
    };
    let module_on = cfg.enabled && platform_supported;
    let mixer_on = module_on && cfg.mixer.enabled;
    let mixer = &cfg.mixer;
    let mut b = Builder { inv, taps: Vec::new(), sources: Vec::new(), outputs: Vec::new(), sends: Vec::new() };

    // ---- outputs: candidates (device found and usable) --------------------
    struct OutCand<'d> {
        status_idx: usize,
        key: String,
        dev: &'d InvDevice,
        first_channel: u32,
        gain: f32,
        effects: Option<&'d Effects>,
    }
    let mut out_cands: Vec<OutCand> = Vec::new();
    let mut seen_ids = HashSet::new();
    for (i, o) in mixer.outputs.iter().enumerate() {
        let id = if o.id.is_empty() { format!("#{i}") } else { o.id.clone() };
        let mut s = StripStatus { id: id.clone(), name: o.name.clone(), kind: StripKind::Output, ..Default::default() };
        let dev = inv.resolve(&o.device);
        if let Some(d) = dev {
            s.device_uid = Some(d.uid.clone());
            s.device_name = Some(d.name.clone());
            if s.name.is_empty() {
                s.name = d.name.clone();
            }
        }
        s.state = if !mixer_on {
            StripState::Off
        } else if !seen_ids.insert(id.clone()) {
            StripState::unavailable(format!("duplicate output id {id}"))
        } else if i >= MAX_OUTPUTS {
            StripState::unavailable(format!("at most {MAX_OUTPUTS} outputs"))
        } else {
            match dev {
                None => StripState::waiting(format!("{} is not connected", device_label(&o.device))),
                Some(d) => match output_problem(d, o.first_channel) {
                    Some(p) => StripState::unavailable(p),
                    None => {
                        out_cands.push(OutCand {
                            status_idx: st.outputs.len(),
                            key: id.clone(),
                            dev: d,
                            first_channel: o.first_channel,
                            gain: if o.muted { 0.0 } else { db_to_gain(o.gain_db) },
                            effects: Some(&o.effects),
                        });
                        StripState::Active
                    }
                },
            }
        };
        st.outputs.push(s);
    }

    // Routes: resolve targets.
    let routes: Vec<(&String, &AppRoute)> = cfg.app_routes.iter().collect();
    let mut route_targets: Vec<Vec<Option<&InvDevice>>> = Vec::new();
    for (bid, r) in &routes {
        let mut rs = RouteStatus {
            bundle_id: (*bid).clone(),
            name: if r.name.is_empty() { inv.app(bid).map(|a| a.name.clone()).unwrap_or_else(|| (*bid).clone()) } else { r.name.clone() },
            enabled: r.enabled,
            keep_default: r.keep_default,
            ..Default::default()
        };
        let mut devs = Vec::new();
        for t in &r.outputs {
            // Only routes that can run now get outputs (so a route for an app
            // that isn't running doesn't keep a device busy).
            let d = inv.resolve(t).filter(|_| module_on && r.enabled && !r.outputs.is_empty() && app_problem(inv, bid).is_none());
            let problem = match inv.resolve(t) {
                None => Some(format!("{} is not connected", device_label(t))),
                Some(d) => output_problem(d, 0),
            };
            rs.targets.push(RouteTarget {
                uid: inv.resolve(t).map(|d| d.uid.clone()),
                name: inv.resolve(t).map(|d| d.name.clone()).unwrap_or_else(|| device_label(t)),
                available: problem.is_none(),
                reason: problem.clone(),
            });
            devs.push(if problem.is_none() { d } else { None });
        }
        route_targets.push(devs);
        st.routes.push(rs);
    }
    for devs in &route_targets {
        for d in devs.iter().flatten() {
            let key = format!("{ROUTE_OUTPUT_PREFIX}{}", d.uid);
            if out_cands.iter().any(|c| c.key == key) {
                continue;
            }
            out_cands.push(OutCand { status_idx: usize::MAX, key, dev: d, first_channel: 0, gain: 1.0, effects: None });
        }
    }

    // ---- clock and sample rate -------------------------------------------
    let Some(clock) = out_cands.first().map(|c| c.dev) else {
        finish_inactive(&mut st, module_on, cfg, mixer_on, inv);
        return Plan { state: st, ..Default::default() };
    };
    let rate = clock.sample_rate;
    st.sample_rate = Some(rate);
    st.clock_device = Some(clock.uid.clone());

    let mut route_out_status: Vec<StripStatus> = Vec::new();
    for c in &out_cands {
        let problem = rate_problem(c.dev, rate);
        if c.status_idx != usize::MAX {
            if let Some(p) = problem {
                st.outputs[c.status_idx].state = StripState::unavailable(p);
                continue;
            }
        } else {
            route_out_status.push(StripStatus {
                id: c.key.clone(),
                name: c.dev.name.clone(),
                kind: StripKind::RouteOutput,
                state: problem.clone().map(StripState::unavailable).unwrap_or(StripState::Active),
                device_uid: Some(c.dev.uid.clone()),
                device_name: Some(c.dev.name.clone()),
            });
            if problem.is_some() {
                continue;
            }
        }
        let values = match c.effects {
            Some(e) => effect_values(e, c.gain, rate),
            None => StripValues { gain: c.gain, ..Default::default() },
        };
        b.outputs.push((TopoOutput { key: c.key.clone(), uid: c.dev.uid.clone(), first_channel: c.first_channel }, values));
    }

    // ---- mixer sources -----------------------------------------------------
    let solo = any_solo(mixer);
    let mut seen_src = HashSet::new();
    for (i, s) in mixer.sources.iter().enumerate() {
        let id = if s.id.is_empty() { format!("#{i}") } else { s.id.clone() };
        let kind = match s.kind {
            SourceKind::App { .. } => StripKind::App,
            SourceKind::Input { .. } => StripKind::Input,
            SourceKind::Loopback { .. } => StripKind::Loopback,
        };
        let mut status = StripStatus { id: id.clone(), name: s.name.clone(), kind, ..Default::default() };
        let mut input: Option<SourceInput> = None;
        let mut level = None;
        status.state = if !mixer_on {
            StripState::Off
        } else if !seen_src.insert(id.clone()) {
            StripState::unavailable(format!("duplicate source id {id}"))
        } else if i >= MAX_SOURCES {
            StripState::unavailable(format!("at most {MAX_SOURCES} sources"))
        } else {
            match &s.kind {
                SourceKind::App { bundle_id } => {
                    if status.name.is_empty() {
                        status.name = inv.app(bundle_id).map(|a| a.name.clone()).unwrap_or_else(|| bundle_id.clone());
                    }
                    match app_problem(inv, bundle_id) {
                        Some(p) => p,
                        None => {
                            input = Some(SourceInput::Tap(b.tap(bundle_id, !s.keep_original)));
                            level = levels.get(bundle_id);
                            StripState::Active
                        }
                    }
                }
                SourceKind::Input { device } | SourceKind::Loopback { device } => match inv.resolve(device) {
                    None => StripState::waiting(format!("{} is not connected", device_label(device))),
                    Some(d) => {
                        status.device_uid = Some(d.uid.clone());
                        status.device_name = Some(d.name.clone());
                        if status.name.is_empty() {
                            status.name = d.name.clone();
                        }
                        match input_problem(d, s.first_channel).or_else(|| rate_problem(d, rate)) {
                            Some(p) => StripState::unavailable(p),
                            None => {
                                let mono = s.mono || s.first_channel + 1 >= d.input_channels();
                                input = Some(SourceInput::Device { uid: d.uid.clone(), first_channel: s.first_channel, mono });
                                StripState::Active
                            }
                        }
                    }
                },
            }
        };
        if let Some(input) = input {
            let gain = source_gain(s, solo, level);
            b.sources.push((TopoSource { key: id.clone(), input }, effect_values(&s.effects, gain, rate)));
        }
        st.sources.push(status);
    }

    // Mixer sends between active strips.
    if mixer_on {
        for send in &mixer.sends {
            let si = b.sources.iter().position(|(t, _)| t.key == send.source);
            let oi = b.outputs.iter().position(|(t, _)| t.key == send.output);
            if let (Some(si), Some(oi)) = (si, oi) {
                let g = if send.muted { 0.0 } else { db_to_gain(send.gain_db) };
                b.sends.push((si, oi, g));
            }
        }
    }

    // ---- route sources -----------------------------------------------------
    for (ri, (bid, r)) in routes.iter().enumerate() {
        let rs = &mut st.routes[ri];
        if !module_on || !r.enabled {
            rs.state = StripState::Off;
            continue;
        }
        if r.outputs.is_empty() {
            rs.state = StripState::unavailable("no target device");
            continue;
        }
        let outs: Vec<usize> = route_targets[ri]
            .iter()
            .flatten()
            .filter_map(|d| {
                let key = format!("{ROUTE_OUTPUT_PREFIX}{}", d.uid);
                b.outputs.iter().position(|(t, _)| t.key == key)
            })
            .collect();
        if let Some(p) = app_problem(inv, bid) {
            rs.state = p;
            continue;
        }
        if outs.is_empty() {
            rs.state = StripState::waiting(if rs.targets.iter().any(|t| t.available) {
                "the target device runs at another sample rate".to_string()
            } else {
                "no target device is connected".to_string()
            });
            continue;
        }
        let tap = b.tap(bid, !r.keep_default);
        let gain = app_level_gain(levels.get(*bid)) * db_to_gain(r.gain_db);
        b.sources.push((TopoSource { key: format!("{ROUTE_SOURCE_PREFIX}{bid}"), input: SourceInput::Tap(tap) }, StripValues { gain, ..Default::default() }));
        let si = b.sources.len() - 1;
        for oi in outs {
            b.sends.push((si, oi, 1.0));
        }
        rs.active = true;
        rs.state = StripState::Active;
    }
    st.outputs.extend(route_out_status);
    for (t, _) in b.sources.iter().filter(|(t, _)| t.key.starts_with(ROUTE_SOURCE_PREFIX)) {
        st.sources.push(StripStatus {
            id: t.key.clone(),
            name: st.routes.iter().find(|r| t.key[ROUTE_SOURCE_PREFIX.len()..] == r.bundle_id).map(|r| r.name.clone()).unwrap_or_default(),
            kind: StripKind::RouteSource,
            state: StripState::Active,
            device_uid: None,
            device_name: None,
        });
    }

    if b.sources.is_empty() {
        // Nothing to play: don't keep devices busy with silence.
        for s in st.outputs.iter_mut() {
            if s.kind == StripKind::RouteOutput {
                s.state = StripState::Off;
            } else if s.state.is_active() {
                s.state = StripState::waiting("no active sources");
            }
        }
        st.outputs.retain(|s| s.kind != StripKind::RouteOutput);
        st.sample_rate = None;
        st.clock_device = None;
        return Plan { state: st, ..Default::default() };
    }

    // Feedback warning: a device both read (loopback/input) and written.
    for (t, _) in &b.sources {
        if let SourceInput::Device { uid, .. } = &t.input {
            if b.outputs.iter().any(|(o, _)| &o.uid == uid) {
                let name = inv.devices.iter().find(|d| &d.uid == uid).map(|d| d.name.as_str()).unwrap_or(uid);
                st.errors.push(format!("feedback: {name} is both a source and an output of the mixer"));
            }
        }
    }

    // ---- sub-devices: clock, outputs, then inputs --------------------------
    let mut subs: Vec<SubDevice> = Vec::new();
    let mut add = |uid: &str, input: bool, output: bool| match subs.iter_mut().find(|s| s.uid == uid) {
        Some(s) => {
            s.input |= input;
            s.output |= output;
        }
        None => subs.push(SubDevice { uid: uid.to_string(), input, output }),
    };
    add(&clock.uid, false, false);
    for (o, _) in &b.outputs {
        add(&o.uid, false, true);
    }
    for (s, _) in &b.sources {
        if let SourceInput::Device { uid, .. } = &s.input {
            add(uid, true, false);
        }
    }

    // ---- values ------------------------------------------------------------
    let ns = b.sources.len();
    let no = b.outputs.len();
    let mut sends = vec![0.0; ns * no];
    for (s, o, g) in &b.sends {
        sends[s * no + o] = *g;
    }
    let values = ParamValues {
        sources: b.sources.iter().map(|(_, v)| v.clone()).collect(),
        outputs: b.outputs.iter().map(|(_, v)| v.clone()).collect(),
        sends,
    };
    let exclusive_apps = b.taps.iter().filter(|t| t.mute_original).map(|t| t.bundle_id.clone()).collect();
    let topology = Topology {
        clock: clock.uid.clone(),
        sample_rate: rate,
        subdevices: subs,
        taps: b.taps,
        sources: b.sources.into_iter().map(|(t, _)| t).collect(),
        outputs: b.outputs.into_iter().map(|(t, _)| t).collect(),
    };
    Plan { topology: Some(topology), values, state: st, exclusive_apps }
}

/// No output could be used: explain why on every strip and route.
fn finish_inactive(st: &mut MixerState, module_on: bool, cfg: &Config, mixer_on: bool, inv: &Inventory) {
    for (ri, (bid, r)) in cfg.app_routes.iter().enumerate() {
        let rs = &mut st.routes[ri];
        rs.state = if !module_on || !r.enabled {
            StripState::Off
        } else if r.outputs.is_empty() {
            StripState::unavailable("no target device")
        } else if let Some(p) = app_problem(inv, bid) {
            p
        } else {
            StripState::waiting("no target device is connected")
        };
    }
    for s in st.sources.iter_mut() {
        s.state = StripState::Off;
    }
    if mixer_on {
        for (i, s) in cfg.mixer.sources.iter().enumerate() {
            let id = if s.id.is_empty() { format!("#{i}") } else { s.id.clone() };
            let name = s.name.clone();
            st.sources.push(StripStatus {
                id,
                name,
                kind: match s.kind {
                    SourceKind::App { .. } => StripKind::App,
                    SourceKind::Input { .. } => StripKind::Input,
                    SourceKind::Loopback { .. } => StripKind::Loopback,
                },
                state: StripState::waiting("no output device is available"),
                ..Default::default()
            });
        }
        if cfg.mixer.outputs.is_empty() {
            st.errors.push("the mixer has no outputs".into());
        }
    } else {
        for (i, s) in cfg.mixer.sources.iter().enumerate() {
            st.sources.push(StripStatus { id: if s.id.is_empty() { format!("#{i}") } else { s.id.clone() }, name: s.name.clone(), ..Default::default() });
        }
    }
}

// ---------------------------------------------------------------------------
// Realised layout

/// A sub-device as the aggregate really has it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RealizedSub {
    pub uid: String,
    pub input_streams: Vec<u32>,
    pub output_streams: Vec<u32>,
}

/// The aggregate's real stream order: sub-devices' streams in sub-device
/// order, then one input stream per tap.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Realized {
    pub subdevices: Vec<RealizedSub>,
    /// (index into `Topology::taps`, channels), in aggregate order.
    pub taps: Vec<(usize, u32)>,
    pub sample_rate: f64,
}

impl Realized {
    /// What a topology should give if the HAL keeps our order (used by the
    /// fake device layer and as the fallback when the order can't be read).
    pub fn expected(topo: &Topology, inv: &Inventory) -> Realized {
        Realized {
            subdevices: topo
                .subdevices
                .iter()
                .map(|s| {
                    let d = inv.devices.iter().find(|d| d.uid == s.uid);
                    RealizedSub {
                        uid: s.uid.clone(),
                        input_streams: d.map(|d| d.input_streams.clone()).unwrap_or_default(),
                        output_streams: d.map(|d| d.output_streams.clone()).unwrap_or_default(),
                    }
                })
                .collect(),
            taps: (0..topo.taps.len()).map(|i| (i, 2)).collect(),
            sample_rate: topo.sample_rate,
        }
    }

    pub fn input_streams(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.subdevices.iter().flat_map(|s| s.input_streams.iter().copied()).collect();
        v.extend(self.taps.iter().map(|t| t.1));
        v
    }

    pub fn output_streams(&self) -> Vec<u32> {
        self.subdevices.iter().flat_map(|s| s.output_streams.iter().copied()).collect()
    }
}

/// Global channel `ch` of a device whose streams start at `base` → location.
fn locate(streams: &[u32], base: usize, ch: u32) -> Option<ChanLoc> {
    let mut left = ch;
    for (i, n) in streams.iter().enumerate() {
        if left < *n {
            return Some(ChanLoc::new((base + i) as u32, left));
        }
        left -= n;
    }
    None
}

/// Map every strip of `topo` onto the aggregate's streams.
pub fn resolve_layout(topo: &Topology, real: &Realized) -> Result<Layout> {
    let mut in_base = Vec::new();
    let mut out_base = Vec::new();
    let (mut ib, mut ob) = (0usize, 0usize);
    for s in &real.subdevices {
        in_base.push(ib);
        out_base.push(ob);
        ib += s.input_streams.len();
        ob += s.output_streams.len();
    }
    let sub = |uid: &str| real.subdevices.iter().position(|s| s.uid == uid);
    let mut layout = Layout::default();
    for s in &topo.sources {
        let locs = match &s.input {
            SourceInput::Tap(t) => {
                let pos = real.taps.iter().position(|(i, _)| i == t).ok_or_else(|| anyhow!("tap for {} is missing from the aggregate", s.key))?;
                let ch = real.taps[pos].1;
                let stream = (ib + pos) as u32;
                if ch == 0 {
                    return Err(anyhow!("tap for {} has no channels", s.key));
                }
                [Some(ChanLoc::new(stream, 0)), (ch >= 2).then_some(ChanLoc::new(stream, 1))]
            }
            SourceInput::Device { uid, first_channel, mono } => {
                let i = sub(uid).ok_or_else(|| anyhow!("{uid} is missing from the aggregate"))?;
                let streams = &real.subdevices[i].input_streams;
                let l = locate(streams, in_base[i], *first_channel).ok_or_else(|| anyhow!("{uid} has no input channel {first_channel}"))?;
                let r = if *mono { None } else { locate(streams, in_base[i], first_channel + 1) };
                [Some(l), r]
            }
        };
        layout.sources.push(locs);
    }
    for o in &topo.outputs {
        let i = sub(&o.uid).ok_or_else(|| anyhow!("{} is missing from the aggregate", o.uid))?;
        let streams = &real.subdevices[i].output_streams;
        let l = locate(streams, out_base[i], o.first_channel).ok_or_else(|| anyhow!("{} has no output channel {}", o.uid, o.first_channel))?;
        let r = locate(streams, out_base[i], o.first_channel + 1);
        layout.outputs.push([Some(l), r]);
    }
    Ok(layout)
}
