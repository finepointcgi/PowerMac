//! Mixer tests: matrix math, routing-rule resolution, layout mapping, the
//! engine against a fake device layer, and real-time-safety checks (a
//! counting global allocator armed only around the audio-thread calls).
#![allow(clippy::field_reassign_with_default)]

use super::config::*;
use super::dsp::biquad::{BandKind, Coeffs};
use super::dsp::{db_to_gain, Frame};
use super::graph::{ChanLoc, Graph, Layout, VecIo, CHUNK};
use super::params::{ParamValues, Params, StripValues};
use super::plan::*;
use super::state::*;
use super::{MixDevice, Realized};
use crate::rules::Snapshot;
use crate::{AppLevel, AudioApp, Backend, CapturePermission, Config, DeviceKind, DeviceMatch, Event, Transport};
use anyhow::Result;
use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Counting allocator

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

fn armed() -> bool {
    ARMED.try_with(|a| a.get()).unwrap_or(false)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: AllocLayout) -> *mut u8 {
        if armed() {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: AllocLayout) {
        if armed() {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        System.dealloc(p, l)
    }
    unsafe fn alloc_zeroed(&self, l: AllocLayout) -> *mut u8 {
        if armed() {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: AllocLayout, n: usize) -> *mut u8 {
        if armed() {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Run `f` and count heap operations made on this thread meanwhile.
fn count_allocs<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = ALLOCS.load(Ordering::SeqCst);
    ARMED.with(|a| a.set(true));
    let r = f();
    ARMED.with(|a| a.set(false));
    // Other threads don't count (ARMED is per thread), but a concurrently
    // armed test could; tests that arm run their check on one thread only.
    (r, ALLOCS.load(Ordering::SeqCst) - before)
}

#[test]
fn counting_allocator_counts() {
    let ((), n) = count_allocs(|| drop(std::hint::black_box(vec![1u8; 32])));
    assert!(n >= 2, "alloc + free: {n}");
}

// ---------------------------------------------------------------------------
// Fake devices

fn dev(uid: &str, name: &str, ins: &[u32], outs: &[u32], rate: f64) -> InvDevice {
    InvDevice { uid: uid.into(), name: name.into(), transport: Transport::Usb, input_streams: ins.to_vec(), output_streams: outs.to_vec(), sample_rate: rate }
}

fn app(bid: &str, objs: &[u32]) -> InvApp {
    InvApp { bundle_id: bid.into(), name: bid.rsplit('.').next().unwrap_or(bid).into(), objects: objs.to_vec(), pids: objs.iter().map(|o| *o as i32 + 1000).collect() }
}

fn inventory() -> Inventory {
    Inventory {
        devices: vec![
            dev("BuiltInSpeakerDevice", "MacBook Pro Speakers", &[], &[2], 48_000.0),
            dev("BuiltInMicrophoneDevice", "MacBook Pro Microphone", &[1], &[], 48_000.0),
            dev("PowerMacAudio:main", "PowerMac Audio", &[2], &[2], 48_000.0),
            dev("PowerMacAudio:mix1", "PowerMac Mix 1", &[8], &[8], 48_000.0),
            dev("USB-Interface", "Scarlett 4i4", &[2, 2], &[2, 2], 48_000.0),
            dev("HDMI-TV", "LG TV", &[], &[2], 44_100.0),
            InvDevice { transport: Transport::Aggregate, ..dev("Agg", "My Aggregate", &[2], &[2], 48_000.0) },
        ],
        apps: vec![app("com.spotify.client", &[40, 41]), app("com.apple.Music", &[50])],
        taps_supported: true,
        capture_permission: CapturePermission::Granted,
    }
}

fn route_cfg(bid: &str, route: AppRoute) -> Config {
    let mut c = Config::default();
    c.app_routes.insert(bid.into(), route);
    c
}

fn src(id: &str, kind: SourceKind) -> MixSource {
    MixSource { id: id.into(), kind, ..Default::default() }
}

fn out(id: &str, uid: &str) -> MixOutput {
    MixOutput { id: id.into(), device: DeviceMatch::uid(uid, ""), ..Default::default() }
}

fn send(s: &str, o: &str, gain_db: f32) -> MixSend {
    MixSend { source: s.into(), output: o.into(), gain_db, muted: false }
}

fn no_levels() -> BTreeMap<String, AppLevel> {
    BTreeMap::new()
}

// ---------------------------------------------------------------------------
// Routing rules

#[test]
fn route_resolves_target_by_uid_then_name() {
    let inv = inventory();
    let by_uid = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::uid("PowerMacAudio:mix1", "stale name")));
    let p = plan(&by_uid, &no_levels(), &inv, true);
    let t = p.topology.unwrap();
    assert_eq!(t.clock, "PowerMacAudio:mix1");
    assert_eq!(t.taps, vec![TapSpec { bundle_id: "com.spotify.client".into(), objects: vec![40, 41], mute_original: true }]);
    assert_eq!(t.sources[0].key, "route:com.spotify.client");
    assert_eq!(t.outputs[0].key, "route-out:PowerMacAudio:mix1");
    assert!(p.state.routes[0].active);
    assert!(p.exclusive_apps.contains("com.spotify.client"));

    // uid changed (device moved ports): exact name fallback.
    let by_name = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::uid("old-uid", "scarlett 4i4")));
    let t = plan(&by_name, &no_levels(), &inv, true).topology.unwrap();
    assert_eq!(t.outputs[0].uid, "USB-Interface");
    // name pattern (no uid): substring.
    let pattern = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::name("Speakers")));
    assert_eq!(plan(&pattern, &no_levels(), &inv, true).topology.unwrap().outputs[0].uid, "BuiltInSpeakerDevice");
}

#[test]
fn route_waits_for_app_and_device() {
    let inv = inventory();
    let not_running = route_cfg("com.example.app", AppRoute::to(DeviceMatch::uid("PowerMacAudio:main", "")));
    let p = plan(&not_running, &no_levels(), &inv, true);
    assert!(p.topology.is_none(), "nothing to run");
    assert_eq!(p.state.routes[0].state, StripState::waiting("not running"));
    assert!(p.state.routes[0].targets[0].available);

    let missing = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::uid("AirPods-uid", "AirPods Pro")));
    let p = plan(&missing, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert_eq!(p.state.routes[0].state, StripState::waiting("no target device is connected"));
    assert_eq!(p.state.routes[0].targets[0].name, "AirPods Pro");
    assert!(!p.state.routes[0].targets[0].available);

    let disabled = route_cfg("com.spotify.client", AppRoute { enabled: false, ..AppRoute::to(DeviceMatch::name("PowerMac Audio")) });
    let p = plan(&disabled, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert_eq!(p.state.routes[0].state, StripState::Off);

    let empty = route_cfg("com.spotify.client", AppRoute::default());
    assert!(matches!(plan(&empty, &no_levels(), &inv, true).state.routes[0].state, StripState::Unavailable { .. }));

    let mut off = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::name("PowerMac Audio")));
    off.enabled = false;
    assert!(plan(&off, &no_levels(), &inv, true).topology.is_none());
}

#[test]
fn route_needs_taps_and_permission() {
    let cfg = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::name("PowerMac Audio")));
    let mut inv = inventory();
    inv.taps_supported = false;
    let p = plan(&cfg, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert!(matches!(&p.state.routes[0].state, StripState::Unavailable { reason } if reason.contains("14.4")));
    let mut inv = inventory();
    inv.capture_permission = CapturePermission::Denied;
    let p = plan(&cfg, &no_levels(), &inv, true);
    assert!(matches!(&p.state.routes[0].state, StripState::Unavailable { reason } if reason.contains("System Audio Recording")));
    // Not determined yet: planned (creating the tap asks).
    let mut inv = inventory();
    inv.capture_permission = CapturePermission::NotDetermined;
    assert!(plan(&cfg, &no_levels(), &inv, true).topology.is_some());
}

#[test]
fn route_keep_default_multi_target_gain_and_app_level() {
    let inv = inventory();
    let r = AppRoute {
        outputs: vec![DeviceMatch::name("PowerMac Audio"), DeviceMatch::name("Scarlett"), DeviceMatch::name("LG TV")],
        keep_default: true,
        gain_db: -6.0206,
        ..Default::default()
    };
    let cfg = route_cfg("com.spotify.client", r);
    let mut levels = BTreeMap::new();
    levels.insert("com.spotify.client".to_string(), AppLevel { volume: 0.5, muted: false });
    let p = plan(&cfg, &levels, &inv, true);
    let t = p.topology.unwrap();
    assert!(!t.taps[0].mute_original, "keep_default → CATapUnmuted");
    assert!(p.exclusive_apps.is_empty());
    // The TV runs at 44.1 kHz: left out with a reason, the others run.
    assert_eq!(t.outputs.len(), 2);
    let tv = p.state.outputs.iter().find(|o| o.device_uid.as_deref() == Some("HDMI-TV")).unwrap();
    assert!(matches!(&tv.state, StripState::Unavailable { reason } if reason.contains("44100")));
    assert!(p.state.routes[0].active);
    // Source gain = app level × route gain; one send per target.
    assert!((p.values.sources[0].gain - 0.25).abs() < 1e-4);
    assert_eq!(p.values.send(0, 0), 1.0);
    assert_eq!(p.values.send(0, 1), 1.0);
    // Muted app level silences the routed copy too.
    levels.insert("com.spotify.client".to_string(), AppLevel { volume: 1.0, muted: true });
    assert_eq!(plan(&cfg, &levels, &inv, true).values.sources[0].gain, 0.0);
}

#[test]
fn app_restart_changes_the_topology() {
    let cfg = route_cfg("com.spotify.client", AppRoute::to(DeviceMatch::name("PowerMac Audio")));
    let mut inv = inventory();
    let a = plan(&cfg, &no_levels(), &inv, true).topology.unwrap();
    // Same processes → same topology (no rebuild).
    assert_eq!(plan(&cfg, &no_levels(), &inv, true).topology.unwrap(), a);
    inv.apps[0].objects = vec![77];
    let b = plan(&cfg, &no_levels(), &inv, true).topology.unwrap();
    assert_ne!(a, b);
    assert_eq!(b.taps[0].objects, vec![77]);
    // A parameter change is not a topology change.
    let mut cfg2 = cfg.clone();
    cfg2.app_routes.get_mut("com.spotify.client").unwrap().gain_db = -12.0;
    assert_eq!(plan(&cfg2, &no_levels(), &inv, true).topology.unwrap(), b);
}

// ---------------------------------------------------------------------------
// Mixer planning and matrix math

fn mixer_cfg() -> Config {
    let mut c = Config::default();
    c.mixer = Mixer {
        enabled: true,
        sources: vec![
            src("music", SourceKind::App { bundle_id: "com.apple.Music".into() }),
            src("mic", SourceKind::Input { device: DeviceMatch::name("MacBook Pro Microphone") }),
            MixSource { first_channel: 2, ..src("guitar", SourceKind::Input { device: DeviceMatch::name("Scarlett") }) },
            src("loop", SourceKind::Loopback { device: DeviceMatch::uid("PowerMacAudio:main", "") }),
        ],
        outputs: vec![out("speakers", "BuiltInSpeakerDevice"), MixOutput { first_channel: 2, ..out("stream", "PowerMacAudio:mix1") }],
        sends: vec![send("music", "speakers", 0.0), send("music", "stream", -6.0206), send("mic", "stream", 0.0), send("guitar", "speakers", -20.0), send("loop", "stream", 0.0)],
    };
    c
}

#[test]
fn mixer_plan_structure() {
    let p = plan(&mixer_cfg(), &no_levels(), &inventory(), true);
    let t = p.topology.unwrap();
    assert_eq!(t.clock, "BuiltInSpeakerDevice");
    assert_eq!(t.sample_rate, 48_000.0);
    let subs: Vec<(&str, bool, bool)> = t.subdevices.iter().map(|s| (s.uid.as_str(), s.input, s.output)).collect();
    assert_eq!(
        subs,
        [
            ("BuiltInSpeakerDevice", false, true),
            ("PowerMacAudio:mix1", false, true),
            ("BuiltInMicrophoneDevice", true, false),
            ("USB-Interface", true, false),
            ("PowerMacAudio:main", true, false),
        ]
    );
    assert_eq!(t.taps.len(), 1);
    assert!(t.taps[0].mute_original, "mixer takes the app over by default");
    assert_eq!(t.sources[1].input, SourceInput::Device { uid: "BuiltInMicrophoneDevice".into(), first_channel: 0, mono: true }, "1-channel mic is mono");
    assert_eq!(t.sources[2].input, SourceInput::Device { uid: "USB-Interface".into(), first_channel: 2, mono: false });
    assert!(p.state.sources.iter().all(|s| s.state.is_active()), "{:?}", p.state.sources);
    assert!(p.state.errors.is_empty(), "{:?}", p.state.errors);
    // Sends.
    assert_eq!(p.values.send(0, 0), 1.0);
    assert!((p.values.send(0, 1) - 0.5).abs() < 1e-4);
    assert_eq!(p.values.send(1, 0), 0.0, "not connected");
    assert!((p.values.send(2, 0) - 0.1).abs() < 1e-4);
}

#[test]
fn gain_mute_solo_matrix_math() {
    let inv = inventory();
    let mut c = mixer_cfg();
    c.mixer.sources[0].gain_db = -6.0206;
    c.mixer.outputs[0].gain_db = 6.0206;
    let p = plan(&c, &no_levels(), &inv, true);
    assert!((p.values.path_gain(0, 0) - 1.0).abs() < 1e-3, "−6 dB fader + 0 dB send + 6 dB master");
    assert!((p.values.path_gain(0, 1) - 0.25).abs() < 1e-3);

    // Solo the mic: everything else is silent; the mic keeps its gain.
    c.mixer.sources[1].solo = true;
    let p = plan(&c, &no_levels(), &inv, true);
    let gains: Vec<f32> = p.values.sources.iter().map(|v| v.gain).collect();
    assert_eq!(gains, [0.0, 1.0, 0.0, 0.0]);
    // Two solos.
    c.mixer.sources[3].solo = true;
    let gains: Vec<f32> = plan(&c, &no_levels(), &inv, true).values.sources.iter().map(|v| v.gain).collect();
    assert_eq!(gains, [0.0, 1.0, 0.0, 1.0]);
    // Mute beats solo.
    c.mixer.sources[1].muted = true;
    let gains: Vec<f32> = plan(&c, &no_levels(), &inv, true).values.sources.iter().map(|v| v.gain).collect();
    assert_eq!(gains, [0.0, 0.0, 0.0, 1.0]);
    // Send mute and output mute.
    let mut c = mixer_cfg();
    c.mixer.sends[0].muted = true;
    c.mixer.outputs[1].muted = true;
    let p = plan(&c, &no_levels(), &inv, true);
    assert_eq!(p.values.path_gain(0, 0), 0.0);
    assert_eq!(p.values.path_gain(0, 1), 0.0);
    assert_eq!(p.values.path_gain(2, 0), p.values.send(2, 0));
    // App level folds into an app source.
    let mut levels = BTreeMap::new();
    levels.insert("com.apple.Music".to_string(), AppLevel { volume: 1.5, muted: false });
    assert!((plan(&mixer_cfg(), &levels, &inv, true).values.sources[0].gain - 1.5).abs() < 1e-6);
    // Pure helper.
    let s = MixSource { gain_db: 0.0, ..Default::default() };
    assert_eq!(source_gain(&s, true, None), 0.0);
    assert_eq!(source_gain(&MixSource { solo: true, ..s.clone() }, true, None), 1.0);
    assert_eq!(source_gain(&s, false, Some(&AppLevel { volume: 0.3, muted: true })), 0.0);
}

#[test]
fn mixer_unavailable_strips_are_explained() {
    let inv = inventory();
    let mut c = mixer_cfg();
    c.mixer.sources.push(src("tv", SourceKind::App { bundle_id: "com.example.notrunning".into() }));
    c.mixer.sources.push(src("agg", SourceKind::Input { device: DeviceMatch::name("My Aggregate") }));
    c.mixer.sources.push(src("tvin", SourceKind::Input { device: DeviceMatch::name("LG TV") }));
    c.mixer.sources.push(MixSource { first_channel: 9, ..src("hi", SourceKind::Input { device: DeviceMatch::name("Scarlett") }) });
    c.mixer.sources.push(src("music", SourceKind::Input { device: DeviceMatch::name("Scarlett") }));
    c.mixer.outputs.push(out("gone", "nope"));
    c.mixer.outputs.push(out("tv", "HDMI-TV"));
    let p = plan(&c, &no_levels(), &inv, true);
    let st = |id: &str| p.state.sources.iter().find(|s| s.id == id).unwrap().state.clone();
    assert_eq!(st("tv"), StripState::waiting("not running"));
    assert!(matches!(st("agg"), StripState::Unavailable { reason } if reason.contains("aggregate")));
    assert!(matches!(st("tvin"), StripState::Unavailable { reason } if reason.contains("no inputs")));
    assert!(matches!(st("hi"), StripState::Unavailable { reason } if reason.contains("4 input channels")));
    assert_eq!(p.state.sources.iter().filter(|s| s.id == "music").count(), 2);
    assert!(p.state.sources.iter().any(|s| s.id == "music" && matches!(&s.state, StripState::Unavailable { reason } if reason.contains("duplicate"))));
    let o = |id: &str| p.state.outputs.iter().find(|s| s.id == id).unwrap().state.clone();
    assert!(matches!(o("gone"), StripState::Waiting { .. }));
    assert!(matches!(o("tv"), StripState::Unavailable { reason } if reason.contains("44100")));
    assert_eq!(p.topology.unwrap().outputs.len(), 2);

    // Disabled mixer: everything off, nothing to run.
    let mut off = mixer_cfg();
    off.mixer.enabled = false;
    let p = plan(&off, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert!(p.state.sources.iter().all(|s| s.state == StripState::Off));
    // Unsupported platform.
    let p = plan(&mixer_cfg(), &no_levels(), &inv, false);
    assert!(p.topology.is_none() && !p.state.supported);
}

#[test]
fn feedback_and_empty_mixers() {
    let inv = inventory();
    let mut c = Config::default();
    c.mixer = Mixer {
        enabled: true,
        sources: vec![src("loop", SourceKind::Loopback { device: DeviceMatch::uid("PowerMacAudio:main", "") })],
        outputs: vec![out("o", "PowerMacAudio:main")],
        sends: vec![send("loop", "o", 0.0)],
    };
    let p = plan(&c, &no_levels(), &inv, true);
    assert!(p.state.errors.iter().any(|e| e.contains("feedback")));
    let t = p.topology.unwrap();
    assert_eq!(t.subdevices.len(), 1);
    assert!(t.subdevices[0].input && t.subdevices[0].output);

    // Outputs but no sources: nothing runs, outputs wait.
    c.mixer.sources.clear();
    let p = plan(&c, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert_eq!(p.state.outputs[0].state, StripState::waiting("no active sources"));
    // No outputs at all.
    c.mixer.outputs.clear();
    c.mixer.sources.push(src("mic", SourceKind::Input { device: DeviceMatch::name("Microphone") }));
    let p = plan(&c, &no_levels(), &inv, true);
    assert!(p.topology.is_none());
    assert!(p.state.errors.iter().any(|e| e.contains("no outputs")));
}

#[test]
fn routes_and_mixer_share_one_tap() {
    let mut c = mixer_cfg();
    c.mixer.sources[0].keep_original = true;
    c.app_routes.insert("com.apple.Music".into(), AppRoute { keep_default: true, ..AppRoute::to(DeviceMatch::name("Scarlett")) });
    let p = plan(&c, &no_levels(), &inventory(), true);
    let t = p.topology.unwrap();
    assert_eq!(t.taps.len(), 1);
    assert!(!t.taps[0].mute_original);
    let route_src = t.sources.iter().position(|s| s.key == "route:com.apple.Music").unwrap();
    assert_eq!(t.sources[route_src].input, SourceInput::Tap(0));
    // Solo in the mixer does not touch the route.
    let mut c2 = c.clone();
    c2.mixer.sources[1].solo = true;
    let p2 = plan(&c2, &no_levels(), &inventory(), true);
    assert_eq!(p2.values.sources[route_src].gain, 1.0);
    assert_eq!(p2.values.sources[0].gain, 0.0);
    // An exclusive route wins over keep_original.
    c.app_routes.get_mut("com.apple.Music").unwrap().keep_default = false;
    assert!(plan(&c, &no_levels(), &inventory(), true).topology.unwrap().taps[0].mute_original);
}

#[test]
fn eq_and_limiter_values() {
    let mut c = mixer_cfg();
    let mut eq = EqConfig::flat(4);
    eq.bands[1].gain_db = 6.0;
    eq.bands[2].enabled = false;
    eq.bands[2].gain_db = 3.0;
    c.mixer.outputs[0].effects = Effects { gain_db: -3.0, eq, limiter: LimiterConfig { enabled: true, ceiling_db: -2.0, ..Default::default() } };
    let p = plan(&c, &no_levels(), &inventory(), true);
    let v = &p.values.outputs[0];
    assert_eq!(v.eq.iter().filter(|b| b.is_some()).count(), 1, "flat and disabled bands are skipped");
    assert!(v.limiter_enabled && v.ceiling_db == -2.0);
    assert!((v.trim - db_to_gain(-3.0)).abs() < 1e-6);
    // Disabled EQ: no bands.
    c.mixer.outputs[0].effects.eq.enabled = false;
    assert!(plan(&c, &no_levels(), &inventory(), true).values.outputs[0].eq.iter().all(|b| b.is_none()));
}

// ---------------------------------------------------------------------------
// Layout

#[test]
fn layout_maps_channels_across_streams() {
    let inv = inventory();
    let p = plan(&mixer_cfg(), &no_levels(), &inv, true);
    let t = p.topology.unwrap();
    let real = Realized::expected(&t, &inv);
    // Inputs: speaker(none), mix1 [8], mic [1], usb [2,2], main [2], then tap.
    assert_eq!(real.input_streams(), vec![8, 1, 2, 2, 2, 2]);
    assert_eq!(real.output_streams(), vec![2, 8, 2, 2, 2]);
    let l = resolve_layout(&t, &real).unwrap();
    assert_eq!(l.sources[0], [Some(ChanLoc::new(5, 0)), Some(ChanLoc::new(5, 1))], "tap is after every device input");
    assert_eq!(l.sources[1], [Some(ChanLoc::new(1, 0)), None], "mono mic");
    assert_eq!(l.sources[2], [Some(ChanLoc::new(3, 0)), Some(ChanLoc::new(3, 1))], "channel 2 of the USB is its second stream");
    assert_eq!(l.sources[3], [Some(ChanLoc::new(4, 0)), Some(ChanLoc::new(4, 1))]);
    assert_eq!(l.outputs[0], [Some(ChanLoc::new(0, 0)), Some(ChanLoc::new(0, 1))]);
    assert_eq!(l.outputs[1], [Some(ChanLoc::new(1, 2)), Some(ChanLoc::new(1, 3))], "mix1 channels 3-4");

    // The HAL reordered sub-devices and taps: the layout follows.
    let mut swapped = real.clone();
    swapped.subdevices.swap(0, 1);
    let l2 = resolve_layout(&t, &swapped).unwrap();
    assert_eq!(l2.outputs[0], [Some(ChanLoc::new(1, 0)), Some(ChanLoc::new(1, 1))]);
    assert_eq!(l2.outputs[1], [Some(ChanLoc::new(0, 2)), Some(ChanLoc::new(0, 3))]);
    // A missing tap is an error, not a silent mis-mapping.
    let mut no_tap = real.clone();
    no_tap.taps.clear();
    assert!(resolve_layout(&t, &no_tap).is_err());
    // Odd channel split across streams: [1, 3] channel 0-1 straddles.
    let mut inv2 = inv.clone();
    inv2.devices[4].input_streams = vec![1, 3];
    let mut c = Config::default();
    c.mixer = Mixer {
        enabled: true,
        sources: vec![src("x", SourceKind::Input { device: DeviceMatch::name("Scarlett") })],
        outputs: vec![out("o", "BuiltInSpeakerDevice")],
        sends: vec![],
    };
    let t = plan(&c, &no_levels(), &inv2, true).topology.unwrap();
    let l = resolve_layout(&t, &Realized::expected(&t, &inv2)).unwrap();
    assert_eq!(l.sources[0], [Some(ChanLoc::new(0, 0)), Some(ChanLoc::new(1, 0))]);
}

// ---------------------------------------------------------------------------
// Graph

fn values(ns: usize, no: usize) -> ParamValues {
    ParamValues { sources: vec![StripValues::default(); ns], outputs: vec![StripValues::default(); no], sends: vec![0.0; ns * no] }
}

/// Graph with stereo source streams 0..ns and stereo output streams 0..no.
fn simple_graph(ns: usize, no: usize, v: &ParamValues) -> (Graph, Arc<Params>) {
    let params = Arc::new(Params::new(ns, no));
    params.store(v, None);
    let layout = Layout {
        sources: (0..ns as u32).map(|i| [Some(ChanLoc::new(i, 0)), Some(ChanLoc::new(i, 1))]).collect(),
        outputs: (0..no as u32).map(|i| [Some(ChanLoc::new(i, 0)), Some(ChanLoc::new(i, 1))]).collect(),
    };
    (Graph::new(params.clone(), layout, 48_000.0), params)
}

fn dc_io(frames: usize, srcs: &[Frame], outs: usize) -> VecIo {
    let mut io = VecIo::new(frames, &vec![2; srcs.len()], &vec![2; outs]);
    for (s, v) in srcs.iter().enumerate() {
        for f in 0..frames {
            *io.input_mut(s, 0, f) = v[0];
            *io.input_mut(s, 1, f) = v[1];
        }
    }
    io
}

#[test]
fn graph_matrix_sums_sources_into_outputs() {
    let mut v = values(2, 2);
    v.sources[0].gain = 0.5;
    v.outputs[1].gain = 2.0;
    v.sends = vec![1.0, 0.25, 0.5, 1.0]; // s0→o0, s0→o1, s1→o0, s1→o1
    let (mut g, _) = simple_graph(2, 2, &v);
    let mut io = dc_io(512, &[[0.4, -0.4], [0.1, 0.2]], 2);
    g.process(&mut io, 512); // ramps in from silence
    g.process(&mut io, 512);
    let o0 = [io.out(0, 0, 400), io.out(0, 1, 400)];
    let o1 = [io.out(1, 0, 400), io.out(1, 1, 400)];
    // o0 = 0.5·1·s0 + 1·0.5·s1 ; o1 = 2·(0.5·0.25·s0 + 1·1·s1)
    assert!((o0[0] - (0.2 + 0.05)).abs() < 1e-6 && (o0[1] - (-0.2 + 0.1)).abs() < 1e-6, "{o0:?}");
    assert!((o1[0] - 2.0 * (0.05 + 0.1)).abs() < 1e-6 && (o1[1] - 2.0 * (-0.05 + 0.2)).abs() < 1e-6, "{o1:?}");
}

#[test]
fn graph_ramps_gain_changes() {
    let mut v = values(1, 1);
    v.sends = vec![1.0];
    let (mut g, p) = simple_graph(1, 1, &v);
    let mut io = dc_io(CHUNK, &[[1.0, 1.0]], 1);
    g.process(&mut io, CHUNK);
    // First block fades in from silence (no click on start).
    assert!(io.out(0, 0, 0) < 0.01 && (io.out(0, 0, CHUNK - 1) - 1.0).abs() < 1e-6);
    g.process(&mut io, CHUNK);
    assert_eq!(io.out(0, 0, 0), 1.0);
    p.sources[0].gain.store(0.0);
    g.process(&mut io, CHUNK);
    let samples: Vec<f32> = (0..CHUNK).map(|f| io.out(0, 0, f)).collect();
    let max_step = samples.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(max_step <= 1.0 / CHUNK as f32 + 1e-6, "{max_step}");
    assert!(samples[CHUNK - 1].abs() < 1e-6);
    // Sends ramp too.
    p.sources[0].gain.store(1.0);
    g.process(&mut io, CHUNK);
    g.process(&mut io, CHUNK);
    p.send(0, 0).store(0.0);
    g.process(&mut io, CHUNK);
    assert!(io.out(0, 0, 0) > 0.99 && io.out(0, 0, CHUNK - 1).abs() < 1e-6);
}

#[test]
fn graph_mono_sources_outputs_and_odd_frame_counts() {
    let params = Arc::new(Params::new(1, 2));
    let mut v = values(1, 2);
    v.sends = vec![1.0, 1.0];
    params.store(&v, None);
    let layout = Layout {
        sources: vec![[Some(ChanLoc::new(0, 1)), None]],                                       // mono from channel 1
        outputs: vec![[Some(ChanLoc::new(0, 2)), Some(ChanLoc::new(1, 0))], [Some(ChanLoc::new(1, 1)), None]], // straddles; mono out
    };
    let mut g = Graph::new(params, layout, 48_000.0);
    let frames = 3 * CHUNK + 17;
    let mut io = VecIo::new(frames, &[2], &[3, 2]);
    for f in 0..frames {
        *io.input_mut(0, 0, f) = 9.0; // ignored channel
        *io.input_mut(0, 1, f) = 0.3;
    }
    g.process(&mut io, frames);
    g.process(&mut io, frames);
    let f = frames - 1;
    assert_eq!(io.out(0, 0, f), 0.0, "untouched channels are zeroed");
    assert!((io.out(0, 2, f) - 0.3).abs() < 1e-6);
    assert!((io.out(1, 0, f) - 0.3).abs() < 1e-6);
    assert!((io.out(1, 1, f) - 0.3).abs() < 1e-6, "mono out = (L+R)/2");
}

#[test]
fn graph_eq_and_limiter_through_params() {
    let fs = 48_000.0;
    let mut v = values(1, 1);
    v.sends = vec![1.0];
    v.sources[0].eq[0] = Some(Coeffs::design(BandKind::Peak, fs, 1000.0, 6.0, 1.0));
    v.outputs[0].gain = db_to_gain(12.0);
    v.outputs[0].limiter_enabled = true;
    v.outputs[0].ceiling_db = -3.0;
    v.outputs[0].lookahead_ms = 2.0;
    let (mut g, p) = simple_graph(1, 1, &v);
    let n = 48_000;
    let mut io = VecIo::new(n, &[2], &[2]);
    for f in 0..n {
        let x = (2.0 * std::f64::consts::PI * 1000.0 * f as f64 / fs).sin() as f32 * 0.1;
        *io.input_mut(0, 0, f) = x;
        *io.input_mut(0, 1, f) = x;
    }
    g.process(&mut io, n);
    // 0.1 · 2 (EQ) · 4 (master) = 0.8 > ceiling 0.708: limited.
    let ceiling = db_to_gain(-3.0);
    let max = (0..n).map(|f| io.out(0, 0, f).abs()).fold(0f32, f32::max);
    assert!(max <= ceiling * 1.000001, "{max}");
    let m = p.outputs[0].meter.read();
    assert!(m.limiter_gain < 0.95, "{m:?}");
    // Limiter off: EQ + master give ≈0.8 peak.
    p.outputs[0].limiter.enabled.store(false, Ordering::Relaxed);
    g.process(&mut io, n);
    let max = (n / 2..n).map(|f| io.out(0, 0, f).abs()).fold(0f32, f32::max);
    assert!((max - 0.1 * 2.0 * 4.0).abs() < 0.01, "{max}");
    // EQ removed through the seqlock.
    p.sources[0].eq.publish(&[None; 10]);
    g.process(&mut io, n);
    let max = (n / 2..n).map(|f| io.out(0, 0, f).abs()).fold(0f32, f32::max);
    assert!((max - 0.4).abs() < 0.01, "{max}");
}

#[test]
fn graph_meters_and_bad_input() {
    let mut v = values(1, 1);
    v.sends = vec![1.0];
    let (mut g, p) = simple_graph(1, 1, &v);
    let mut io = dc_io(4800, &[[0.5, f32::NAN]], 1);
    for _ in 0..20 {
        g.process(&mut io, 4800);
    }
    let m = p.sources[0].meter.read();
    assert!((m.peak[0] - 0.5).abs() < 1e-3 && (m.rms[0] - 0.5).abs() < 1e-2, "{m:?}");
    assert_eq!(m.peak[1], 0.0, "NaN is silenced");
    assert!(io.out(0, 1, 100).is_finite());
    assert_eq!(p.stats.callbacks.load(Ordering::Relaxed), 20);
    assert_eq!(p.stats.layout_mismatches.load(Ordering::Relaxed), 0);
    // Fewer streams than the layout expects: silence + a mismatch count.
    let mut short = VecIo::new(64, &[], &[2]);
    g.process(&mut short, 64);
    assert_eq!(p.stats.layout_mismatches.load(Ordering::Relaxed), 1);
}

/// The audio-thread entry point does not allocate, with every feature on
/// and parameters changing between callbacks (including EQ republishing
/// and limiter look-ahead changes).
#[test]
fn graph_process_is_allocation_free() {
    let ns = 6;
    let no = 3;
    let mut v = values(ns, no);
    for s in 0..ns {
        for o in 0..no {
            v.sends[s * no + o] = 0.3;
        }
        v.sources[s].limiter_enabled = s % 2 == 0;
        for b in 0..10 {
            v.sources[s].eq[b] = Some(Coeffs::design(BandKind::Peak, 48_000.0, 100.0 * (b + 1) as f64, 3.0, 1.0));
        }
    }
    for o in 0..no {
        v.outputs[o].limiter_enabled = true;
        v.outputs[o].eq[0] = Some(Coeffs::design(BandKind::HighShelf, 48_000.0, 8000.0, -3.0, 0.7));
    }
    let (mut g, p) = simple_graph(ns, no, &v);
    let mut io = dc_io(1024, &vec![[0.7, -0.7]; ns], no);
    let ((), n) = count_allocs(|| {
        for i in 0..50 {
            g.process(&mut io, 1024 - (i % 7) * 13);
        }
    });
    assert_eq!(n, 0, "allocations in steady state");
    // Parameter changes from the control thread between callbacks.
    let mut v2 = v.clone();
    for (i, s) in v2.sources.iter_mut().enumerate() {
        s.gain = 0.5;
        s.lookahead_ms = (i as f32) * 3.0;
        s.eq[3] = None;
    }
    p.store(&v2, Some(&v));
    let ((), n) = count_allocs(|| {
        for _ in 0..10 {
            g.process(&mut io, 512);
        }
    });
    assert_eq!(n, 0, "allocations after parameter changes");
}

/// The real macOS IOProc on hand-built `AudioBufferList`s: same results as
/// the graph and no allocation.
#[cfg(target_os = "macos")]
#[test]
fn macos_io_proc_on_buffer_lists() {
    use crate::macos::ffi::{AudioBuffer, AudioBufferList};
    let mut v = values(1, 1);
    v.sends = vec![1.0];
    v.sources[0].gain = 0.5;
    let (g, _p) = simple_graph(1, 1, &v);
    let frames = 512usize;
    let mut inp = vec![0.8f32; frames * 2];
    let mut outp = vec![9.0f32; frames * 2];
    #[repr(C)]
    struct Abl1 {
        n: u32,
        b: [AudioBuffer; 1],
    }
    let in_abl = Abl1 { n: 1, b: [AudioBuffer { mNumberChannels: 2, mDataByteSize: (frames * 8) as u32, mData: inp.as_mut_ptr() as *mut _ }] };
    let mut out_abl = Abl1 { n: 1, b: [AudioBuffer { mNumberChannels: 2, mDataByteSize: (frames * 8) as u32, mData: outp.as_mut_ptr() as *mut _ }] };
    let in_ptr = &in_abl as *const Abl1 as *const AudioBufferList;
    let out_ptr = &mut out_abl as *mut Abl1 as *mut AudioBufferList;
    let g = crate::macos::mixer::run_io_proc_for_test(Box::new(g), in_ptr, out_ptr);
    // Allocation check on the bare callback.
    let ctx = crate::macos::mixer::test_ctx(g);
    let ((), n) = count_allocs(|| unsafe {
        for _ in 0..20 {
            crate::macos::mixer::mix_io_proc(0, std::ptr::null(), in_ptr, std::ptr::null(), out_ptr, std::ptr::null(), ctx);
        }
    });
    crate::macos::mixer::free_test_ctx(ctx);
    assert_eq!(n, 0);
    assert!((outp[frames * 2 - 1] - 0.4).abs() < 1e-6, "{}", outp[frames * 2 - 1]);
    assert_eq!(crate::macos::mixer::callback_frames(unsafe { AudioBufferList::buffers(in_ptr) }, &[]), frames);
}

// ---------------------------------------------------------------------------
// Engine against a fake device layer

#[derive(Default)]
struct FakeState {
    inventory: Inventory,
    opened: Vec<Topology>,
    graph: Option<Box<Graph>>,
    applied_levels: Vec<BTreeMap<String, AppLevel>>,
    open_fails: bool,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

struct FakeDevice {
    fake: Fake,
    realized: Realized,
}

impl MixDevice for FakeDevice {
    fn realized(&self) -> &Realized {
        &self.realized
    }
    fn start(&mut self, graph: Box<Graph>) -> Result<()> {
        self.fake.0.lock().unwrap().graph = Some(graph);
        Ok(())
    }
}

impl Drop for FakeDevice {
    fn drop(&mut self) {
        self.fake.0.lock().unwrap().graph = None;
    }
}

impl Backend for Fake {
    fn snapshot(&self, _levels: bool) -> Result<Snapshot> {
        Ok(Snapshot::default())
    }
    fn set_default(&self, _kind: DeviceKind, _uid: &str) -> Result<()> {
        Ok(())
    }
    fn set_volume(&self, _uid: &str, _volume: f32) -> Result<()> {
        Ok(())
    }
    fn set_mute(&self, _uid: &str, _mute: bool) -> Result<()> {
        Ok(())
    }
    fn watch(&self, _notify: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Send>> {
        Ok(Box::new(()))
    }
    fn per_app_supported(&self) -> bool {
        true
    }
    fn max_app_volume(&self) -> f32 {
        1.5
    }
    fn apps(&self, _levels: &BTreeMap<String, AppLevel>) -> Result<Vec<AudioApp>> {
        Ok(Vec::new())
    }
    fn apply_app_levels(&self, levels: &BTreeMap<String, AppLevel>, enabled: bool) -> Result<()> {
        if enabled {
            self.0.lock().unwrap().applied_levels.push(levels.clone());
        }
        Ok(())
    }
    fn capture_permission(&self) -> CapturePermission {
        CapturePermission::Granted
    }
    fn request_capture_permission(&self) -> Result<bool> {
        Ok(true)
    }
    fn mixer_supported(&self) -> bool {
        true
    }
    fn mix_inventory(&self) -> Result<Inventory> {
        Ok(self.0.lock().unwrap().inventory.clone())
    }
    fn mix_open(&self, topo: &Topology) -> Result<Box<dyn MixDevice>> {
        let mut st = self.0.lock().unwrap();
        if st.open_fails {
            anyhow::bail!("fake open failure");
        }
        st.opened.push(topo.clone());
        Ok(Box::new(FakeDevice { fake: self.clone(), realized: Realized::expected(topo, &st.inventory) }))
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run the fake device's graph for `frames` with every input stream at a
/// DC value; returns the output buffers.
fn pump(fake: &Fake, value: f32, frames: usize) -> Option<VecIo> {
    let mut st = fake.0.lock().unwrap();
    let topo = st.opened.last()?.clone();
    let real = Realized::expected(&topo, &st.inventory);
    let ins: Vec<usize> = real.input_streams().iter().map(|c| *c as usize).collect();
    let outs: Vec<usize> = real.output_streams().iter().map(|c| *c as usize).collect();
    let g = st.graph.as_mut()?;
    let mut io = VecIo::new(frames, &ins, &outs);
    for (b, _) in io.inputs.iter_mut() {
        b.fill(value);
    }
    g.process(&mut io, frames);
    Some(io)
}

#[test]
fn engine_routes_follow_config_apps_and_devices() {
    let fake = Fake::default();
    fake.0.lock().unwrap().inventory = inventory();
    let mut cfg = Config::default();
    cfg.per_app_volume = true;
    cfg.app_volumes.insert("com.spotify.client".into(), AppLevel { volume: 0.5, muted: false });
    cfg.app_volumes.insert("com.apple.Music".into(), AppLevel { volume: 0.7, muted: false });
    let h = crate::AudioControl::start_with_backend(cfg, Arc::new(fake.clone())).unwrap();
    let rx = h.subscribe();
    assert!(!h.mixer_state().running);

    // Route Spotify to PowerMac Mix 1, exclusively.
    h.set_route("com.spotify.client", Some(AppRoute::to(DeviceMatch::uid("PowerMacAudio:mix1", "PowerMac Mix 1")))).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::RoutesChanged);
    wait_for("the route to start", || h.mixer_state().running);
    assert_eq!(h.routes().len(), 1);
    let st = h.mixer_state();
    assert!(st.routes[0].active, "{st:?}");
    assert_eq!(fake.0.lock().unwrap().opened.len(), 1);
    // Per-app volume no longer handles Spotify (the mixer applies its level).
    wait_for("levels without the routed app", || {
        fake.0.lock().unwrap().applied_levels.last().is_some_and(|l| !l.contains_key("com.spotify.client") && l.contains_key("com.apple.Music"))
    });
    // Audio flows: DC 1.0 in → 0.5 (app level) out on mix1 channels 0/1.
    pump(&fake, 1.0, CHUNK).unwrap();
    let io = pump(&fake, 1.0, CHUNK).unwrap();
    assert!((io.out(0, 0, 10) - 0.5).abs() < 1e-6 && (io.out(0, 1, 10) - 0.5).abs() < 1e-6);
    assert_eq!(io.out(0, 2, 10), 0.0);
    let m = h.meters();
    assert_eq!(m.sources[0].id, "route:com.spotify.client");
    assert_eq!(m.outputs[0].id, "route-out:PowerMacAudio:mix1");
    assert!((m.outputs[0].peak[0] - 0.5).abs() < 1e-3);
    let graph_ptr = fake.0.lock().unwrap().graph.as_ref().map(|g| &**g as *const Graph as usize);

    // Changing the per-app level is a parameter update (no rebuild).
    h.set_app_volume("com.spotify.client", 1.0).unwrap();
    pump(&fake, 1.0, CHUNK).unwrap();
    let io = pump(&fake, 1.0, CHUNK).unwrap();
    assert!((io.out(0, 0, 10) - 1.0).abs() < 1e-6);
    assert_eq!(fake.0.lock().unwrap().opened.len(), 1);
    assert_eq!(fake.0.lock().unwrap().graph.as_ref().map(|g| &**g as *const Graph as usize), graph_ptr);

    // The app restarts (new process objects): rebuilt by itself on the next
    // device/process notification.
    fake.0.lock().unwrap().inventory.apps[0].objects = vec![90];
    h.shared.wake();
    wait_for("rebuild after app restart", || fake.0.lock().unwrap().opened.len() == 2);
    assert_eq!(fake.0.lock().unwrap().opened[1].taps[0].objects, vec![90]);

    // The app quits: the mixer stops, the route waits.
    fake.0.lock().unwrap().inventory.apps.remove(0);
    h.shared.wake();
    wait_for("stop after the app quits", || !h.mixer_state().running);
    assert_eq!(h.mixer_state().routes[0].state, StripState::waiting("not running"));
    assert!(fake.0.lock().unwrap().graph.is_none(), "device dropped");

    // Remove the route.
    h.set_route("com.spotify.client", None).unwrap();
    wait_for("route removed", || h.mixer_state().routes.is_empty());
    assert!(h.config().app_routes.is_empty());
    h.stop();
}

#[test]
fn engine_mixer_param_updates_do_not_rebuild() {
    let fake = Fake::default();
    fake.0.lock().unwrap().inventory = inventory();
    let h = crate::AudioControl::start_with_backend(Config::default(), Arc::new(fake.clone())).unwrap();
    let mut m = Mixer {
        enabled: true,
        sources: vec![src("mic", SourceKind::Input { device: DeviceMatch::name("MacBook Pro Microphone") })],
        outputs: vec![out("spk", "BuiltInSpeakerDevice")],
        sends: vec![send("mic", "spk", 0.0)],
    };
    h.set_mixer(m.clone()).unwrap();
    wait_for("mixer start", || h.mixer_state().running);
    let io = pump(&fake, 0.25, CHUNK).unwrap();
    assert!((io.out(0, 0, CHUNK - 1) - 0.25).abs() < 1e-6);
    // Fader, send, EQ and limiter changes: same device, new values.
    m.sources[0].gain_db = -6.0206;
    m.outputs[0].effects.limiter.enabled = true;
    m.outputs[0].effects.eq = EqConfig::flat(5);
    m.set_send("mic", "spk", 6.0206, false);
    h.set_mixer(m.clone()).unwrap();
    pump(&fake, 0.25, CHUNK).unwrap();
    let io = pump(&fake, 0.25, CHUNK).unwrap();
    assert!((io.out(0, 0, CHUNK - 1) - 0.25).abs() < 1e-3, "{}", io.out(0, 0, CHUNK - 1));
    assert_eq!(fake.0.lock().unwrap().opened.len(), 1);
    // Mute: silence, still no rebuild.
    m.sources[0].muted = true;
    h.set_mixer(m.clone()).unwrap();
    pump(&fake, 0.25, CHUNK).unwrap();
    pump(&fake, 0.25, CHUNK).unwrap(); // the output limiter's 5 ms look-ahead drains
    assert_eq!(pump(&fake, 0.25, CHUNK).unwrap().out(0, 0, 5), 0.0);
    // Adding an output is structural: rebuilt.
    m.outputs.push(out("loop", "PowerMacAudio:main"));
    h.set_mixer(m.clone()).unwrap();
    wait_for("rebuild for a new output", || fake.0.lock().unwrap().opened.len() == 2);
    assert_eq!(h.mixer().outputs.len(), 2);
    assert!(h.set_mixer(Mixer { sources: vec![src("route:x", SourceKind::default())], ..Default::default() }).is_err());
    // Disabling stops it.
    m.enabled = false;
    h.set_mixer(m).unwrap();
    wait_for("mixer stop", || !h.mixer_state().running);
    h.stop();
    assert!(fake.0.lock().unwrap().graph.is_none());
}

#[test]
fn engine_failed_start_is_reported_and_not_retried_at_once() {
    let fake = Fake::default();
    {
        let mut st = fake.0.lock().unwrap();
        st.inventory = inventory();
        st.open_fails = true;
    }
    let h = crate::AudioControl::start_with_backend(Config::default(), Arc::new(fake.clone())).unwrap();
    let rx = h.subscribe();
    h.set_route("com.apple.Music", Some(AppRoute::to(DeviceMatch::name("PowerMac Audio")))).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let mut got_error = false;
    while Instant::now() < end && !got_error {
        if let Ok(Event::Error { message }) = rx.recv_timeout(Duration::from_millis(100)) {
            got_error = message.contains("fake open failure");
        }
    }
    assert!(got_error);
    let st = h.mixer_state();
    assert!(!st.running);
    assert!(!st.routes[0].active);
    assert!(matches!(st.routes[0].state, StripState::Waiting { .. }));
    // Further notifications don't hammer the device layer.
    h.shared.wake();
    h.shared.wake();
    std::thread::sleep(Duration::from_millis(200));
    assert!(h.mixer_state().errors.iter().any(|e| e.contains("retrying")));
    h.stop();
}
