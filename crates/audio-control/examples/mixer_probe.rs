//! Probe the mixer / routing code paths on this machine.
//!
//! ```text
//! cargo run -p audio-control --example mixer_probe          # read-only: inventory + plans against real devices
//! cargo run -p audio-control --example mixer_probe -- silent # macOS: 2 s of silence through the real aggregate + IOProc path
//! ```
//! `list` creates nothing (no taps, no aggregate, no permission prompts).
//! `silent` creates one private aggregate on the default output when that
//! device has no inputs, writes zeros for two seconds and destroys it; no
//! device setting is changed and nothing is audible.

use audio_control::mixer::plan::{plan, resolve_layout};
use audio_control::mixer::{Inventory, Realized};
use audio_control::*;

fn show_inventory(inv: &Inventory) {
    println!("Devices (inputs / outputs = channels per stream):");
    for d in &inv.devices {
        println!(
            "  {:<34} {:<10} in {:<10} out {:<10} {:>6} Hz  uid={}",
            d.name,
            d.transport.label(),
            format!("{:?}", d.input_streams),
            format!("{:?}", d.output_streams),
            d.sample_rate,
            d.uid
        );
    }
    println!("Apps with audio processes:");
    for a in &inv.apps {
        println!("  {:<28} {:<40} objects={:?} pids={:?}", a.name, a.bundle_id, a.objects, a.pids);
    }
    println!("taps supported: {}  capture permission: {:?}", inv.taps_supported, inv.capture_permission);
}

fn show_plan(title: &str, cfg: &Config, inv: &Inventory) {
    println!("\n== {title}");
    let p = plan(cfg, &Default::default(), inv, true);
    for r in &p.state.routes {
        println!("  route {} → {:?}: {:?}", r.bundle_id, r.targets.iter().map(|t| (&t.name, t.available)).collect::<Vec<_>>(), r.state);
    }
    for s in &p.state.sources {
        println!("  source {:<12} {:?} {:?}", s.id, s.kind, s.state);
    }
    for s in &p.state.outputs {
        println!("  output {:<12} {:?} {:?}", s.id, s.device_name, s.state);
    }
    for e in &p.state.errors {
        println!("  ! {e}");
    }
    match &p.topology {
        None => println!("  (nothing to run)"),
        Some(t) => {
            println!("  clock {} @ {} Hz", t.clock, t.sample_rate);
            println!("  sub-devices {:?}", t.subdevices.iter().map(|s| (&s.uid, s.input, s.output)).collect::<Vec<_>>());
            println!("  taps {:?}", t.taps.iter().map(|t| (&t.bundle_id, &t.objects, t.mute_original)).collect::<Vec<_>>());
            let real = Realized::expected(t, inv);
            println!("  expected aggregate streams: in {:?} out {:?}", real.input_streams(), real.output_streams());
            match resolve_layout(t, &real) {
                Ok(l) => {
                    for (s, loc) in t.sources.iter().zip(l.sources.iter()) {
                        println!("    {:<28} reads  {:?}", s.key, loc);
                    }
                    for (o, loc) in t.outputs.iter().zip(l.outputs.iter()) {
                        println!("    {:<28} writes {:?}", o.key, loc);
                    }
                }
                Err(e) => println!("  layout error: {e:#}"),
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn inventory() -> Inventory {
    macos_testing::mixer_inventory()
}

#[cfg(not(target_os = "macos"))]
fn inventory() -> Inventory {
    Inventory::default()
}

fn list() {
    let inv = inventory();
    show_inventory(&inv);
    let out = inv.devices.iter().find(|d| d.output_channels() > 0);
    let input = inv.devices.iter().find(|d| d.input_channels() > 0);
    let virt = inv.devices.iter().find(|d| d.uid.starts_with("PowerMacAudio:"));
    let app = inv.apps.first();

    if let (Some(a), Some(o)) = (app, out) {
        let mut c = Config::default();
        c.app_routes.insert(a.bundle_id.clone(), AppRoute::to(DeviceMatch::uid(o.uid.clone(), o.name.clone())));
        show_plan(&format!("route {} → {}", a.name, o.name), &c, &inv);
    }
    let mut c = Config::default();
    c.app_routes.insert("com.example.notrunning".into(), AppRoute::to(DeviceMatch::name("PowerMac Audio")));
    show_plan("route a non-running app to PowerMac Audio", &c, &inv);

    let mut c = Config::default();
    let mut sources = Vec::new();
    if let Some(i) = input {
        sources.push(MixSource { id: "in".into(), kind: SourceKind::Input { device: DeviceMatch::uid(i.uid.clone(), "") }, ..Default::default() });
    }
    if let Some(v) = virt {
        sources.push(MixSource { id: "loop".into(), kind: SourceKind::Loopback { device: DeviceMatch::uid(v.uid.clone(), "") }, ..Default::default() });
    }
    if let Some(a) = app {
        sources.push(MixSource { id: "app".into(), kind: SourceKind::App { bundle_id: a.bundle_id.clone() }, ..Default::default() });
    }
    let outputs: Vec<MixOutput> = inv
        .devices
        .iter()
        .filter(|d| d.output_channels() > 0)
        .take(2)
        .enumerate()
        .map(|(i, d)| MixOutput { id: format!("out{i}"), device: DeviceMatch::uid(d.uid.clone(), ""), ..Default::default() })
        .collect();
    let mut m = Mixer { enabled: true, sources, outputs, sends: Vec::new() };
    let pairs: Vec<(String, String)> = m.sources.iter().flat_map(|s| m.outputs.iter().map(move |o| (s.id.clone(), o.id.clone()))).collect();
    for (s, o) in pairs {
        m.set_send(&s, &o, -6.0, false);
    }
    c.mixer = m;
    show_plan("mixer: every source → first two outputs", &c, &inv);
}

#[cfg(target_os = "macos")]
fn silent() -> anyhow::Result<()> {
    let before_default = macos_testing::default_uid(DeviceKind::Output);
    let before_devices: Vec<String> = list_devices(DeviceKind::Output)?.iter().map(|d| d.uid.clone()).collect();
    let before_vol = list_devices(DeviceKind::Output)?.into_iter().find(|d| d.is_default).and_then(|d| d.volume);
    let uid = before_default.clone().ok_or_else(|| anyhow::anyhow!("no default output"))?;
    println!("silent mixer run on {uid} for 2 s …");
    let r = macos_testing::silent_mixer_run(&uid, std::time::Duration::from_secs(2))?;
    println!("  realized: {:?}", r.realized);
    println!("  layout: {:?}", r.layout);
    println!("  callbacks {}  max frames {}  layout mismatches {}", r.callbacks, r.max_frames, r.layout_mismatches);
    for d in &r.diagnostics {
        println!("  {d}");
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let after_devices: Vec<String> = list_devices(DeviceKind::Output)?.iter().map(|d| d.uid.clone()).collect();
    let after_vol = list_devices(DeviceKind::Output)?.into_iter().find(|d| d.is_default).and_then(|d| d.volume);
    println!("  default output unchanged: {}", macos_testing::default_uid(DeviceKind::Output) == before_default);
    println!("  device list unchanged: {}", before_devices == after_devices);
    println!("  volume unchanged: {} ({before_vol:?})", before_vol == after_vol);
    anyhow::ensure!(r.callbacks > 10, "the IOProc did not run");
    anyhow::ensure!(r.layout_mismatches == 0, "layout mismatches");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    match std::env::args().nth(1).as_deref() {
        None | Some("list") => list(),
        #[cfg(target_os = "macos")]
        Some("silent") => silent()?,
        Some(other) => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}
