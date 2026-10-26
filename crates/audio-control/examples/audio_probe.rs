//! Probe the audio-control crate on this machine.
//!
//! ```text
//! cargo run -p audio-control --example audio_probe            # list devices, apps, support
//! cargo run -p audio-control --example audio_probe -- watch 10  # print events for 10 s
//! cargo run -p audio-control --example audio_probe -- volume    # nudge + restore default output volume
//! cargo run -p audio-control --example audio_probe -- input     # switch default input away and back
//! cargo run -p audio-control --example audio_probe -- revert    # macOS: simulate an auto-switch to a blocked device
//! cargo run -p audio-control --example audio_probe -- tap       # macOS: per-app volume on our own very quiet tone
//! ```
//! Every command restores what it changes.

use audio_control::*;
use std::time::{Duration, Instant};

fn print_devices(title: &str, devs: &[AudioDevice]) {
    println!("{title}:");
    for d in devs {
        println!(
            "  {} {:<34} {:<12} ch={} vol={} mute={}{}  uid={}",
            if d.is_default { "*" } else { " " },
            d.name,
            d.transport.label(),
            d.channels,
            d.volume.map(|v| format!("{v:.3}")).unwrap_or_else(|| "-".into()),
            d.muted.map(|m| m.to_string()).unwrap_or_else(|| "-".into()),
            if d.is_default_system { " [alerts]" } else { "" },
            d.uid
        );
    }
}

fn list() -> anyhow::Result<()> {
    print_devices("Outputs", &list_devices(DeviceKind::Output)?);
    print_devices("Inputs", &list_devices(DeviceKind::Input)?);
    #[cfg(target_os = "macos")]
    println!("macOS {:?}", macos_testing::os_version());
    println!("per-app volume supported: {}", per_app_volume_supported());
    println!("audio capture permission: {:?}", capture_permission());
    let h = AudioControl::start(Config::default())?;
    println!("Apps producing audio:");
    for a in h.apps() {
        println!("  {:<28} {:<40} pids={:?} playing={}", a.name, a.bundle_id, a.pids, a.playing);
    }
    h.stop();
    Ok(())
}

fn watch(secs: u64) -> anyhow::Result<()> {
    let h = AudioControl::start(Config::default())?;
    let rx = h.subscribe();
    let end = Instant::now() + Duration::from_secs(secs);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(ev) = rx.recv_timeout(left) {
            println!("{ev:?}");
        }
    }
    h.stop();
    Ok(())
}

fn volume() -> anyhow::Result<()> {
    let h = AudioControl::start(Config::default())?;
    let Some(d) = h.outputs().into_iter().find(|d| d.is_default) else { anyhow::bail!("no default output") };
    let Some(before) = d.volume else {
        println!("{} has no volume control", d.name);
        return Ok(());
    };
    let nudged = if before > 0.5 { before - 0.02 } else { before + 0.02 };
    h.set_device_volume(&d.uid, nudged)?;
    std::thread::sleep(Duration::from_millis(100));
    let read = h.outputs().into_iter().find(|x| x.uid == d.uid).and_then(|x| x.volume);
    h.set_device_volume(&d.uid, before)?;
    std::thread::sleep(Duration::from_millis(100));
    let after = h.outputs().into_iter().find(|x| x.uid == d.uid).and_then(|x| x.volume);
    println!("{}: before {before:.4}, set {nudged:.4}, read {read:?}, restored {after:?}", d.name);
    if let Some(m) = d.muted {
        // Toggle mute only when it is already muted? No: re-setting the same
        // value exercises the call without any audible change.
        h.set_device_mute(&d.uid, m)?;
        println!("mute re-set to {m}");
    }
    h.stop();
    Ok(())
}

fn input() -> anyhow::Result<()> {
    let h = AudioControl::start(Config::default())?;
    let rx = h.subscribe();
    let ins = h.inputs();
    let Some(cur) = ins.iter().find(|d| d.is_default).cloned() else { anyhow::bail!("no default input") };
    let Some(other) = ins.iter().find(|d| !d.is_default).cloned() else {
        println!("only one input; nothing to switch");
        return Ok(());
    };
    h.set_default_input(&other.uid)?;
    std::thread::sleep(Duration::from_millis(300));
    let mid = h.default_device(DeviceKind::Input).map(|d| d.name);
    h.set_default_input(&cur.uid)?;
    std::thread::sleep(Duration::from_millis(300));
    let end = h.default_device(DeviceKind::Input).map(|d| d.name);
    println!("input: {} -> {:?} -> {:?}", cur.name, mid, end);
    for ev in rx.try_iter() {
        println!("  {ev:?}");
    }
    h.stop();
    Ok(())
}

#[cfg(target_os = "macos")]
fn revert() -> anyhow::Result<()> {
    let original = macos_testing::default_uid(DeviceKind::Output).ok_or_else(|| anyhow::anyhow!("no default output"))?;
    let original_sys = macos_testing::default_uid(DeviceKind::SystemOutput);
    let name = "PowerMac Probe Blocked";
    let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name(name)], ..Default::default() };
    let h = AudioControl::start(cfg)?;
    let rx = h.subscribe();
    std::thread::sleep(Duration::from_millis(300));
    let result = (|| -> anyhow::Result<()> {
        let dev = macos_testing::TestDevice::create(name)?;
        // Let the engine see it appear, then act like macOS switching to it.
        std::thread::sleep(Duration::from_millis(150));
        let t0 = Instant::now();
        set_default_device(DeviceKind::Output, &dev.uid)?;
        let mut reverted = None;
        while t0.elapsed() < Duration::from_secs(2) {
            if macos_testing::default_uid(DeviceKind::Output).as_deref() == Some(original.as_str()) {
                reverted = Some(t0.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        match reverted {
            Some(t) => println!("blocked auto-switch reverted after {} ms", t.as_millis()),
            None => println!("NOT reverted within 2 s"),
        }
        drop(dev);
        Ok(())
    })();
    std::thread::sleep(Duration::from_millis(300));
    h.stop();
    for ev in rx.try_iter() {
        println!("  {ev:?}");
    }
    // Belt and braces: put everything back exactly.
    if macos_testing::default_uid(DeviceKind::Output).as_deref() != Some(original.as_str()) {
        set_default_device(DeviceKind::Output, &original)?;
    }
    if let Some(s) = original_sys {
        if macos_testing::default_uid(DeviceKind::SystemOutput).as_deref() != Some(s.as_str()) {
            set_default_device(DeviceKind::SystemOutput, &s)?;
        }
    }
    println!(
        "restored: output {} / system {:?}",
        macos_testing::default_uid(DeviceKind::Output).as_deref() == Some(original.as_str()),
        macos_testing::default_uid(DeviceKind::SystemOutput)
    );
    result
}

#[cfg(target_os = "macos")]
fn tap(force: bool) -> anyhow::Result<()> {
    println!("per-app supported: {}, permission: {:?}", per_app_volume_supported(), capture_permission());
    if !per_app_volume_supported() {
        return Ok(());
    }
    if capture_permission() != CapturePermission::Granted && !force {
        println!("audio capture permission not granted; skipping the live tap (pass --force to try, which may prompt)");
        let h = AudioControl::start(Config::default())?;
        let (apps, frames) = macos_testing::with_quiet_tone(0.0005, || {
            std::thread::sleep(Duration::from_millis(400));
            h.apps()
        })?;
        println!("while playing a quiet tone ({frames} frames), apps: {apps:#?}");
        h.stop();
        return Ok(());
    }
    let cfg = Config { per_app_volume: true, ..Default::default() };
    let h = AudioControl::start(cfg)?;
    let ((), frames) = macos_testing::with_quiet_tone(0.0005, || {
        std::thread::sleep(Duration::from_millis(500));
        let me = h.apps().into_iter().find(|a| a.pids.contains(&(std::process::id() as i32)));
        println!("our process as an app: {me:?}");
        if let Some(me) = me {
            match h.set_app_volume(&me.bundle_id, 0.5) {
                Ok(()) => println!("set {} to 50%", me.bundle_id),
                Err(e) => println!("set_app_volume failed: {e:#}"),
            }
            std::thread::sleep(Duration::from_millis(1500));
            for l in h.per_app_diagnostics() {
                println!("  {l}");
            }
            let _ = h.set_app_mute(&me.bundle_id, true);
            std::thread::sleep(Duration::from_millis(500));
            for l in h.per_app_diagnostics() {
                println!("  {l}");
            }
            let _ = h.set_app_mute(&me.bundle_id, false);
            let _ = h.set_app_volume(&me.bundle_id, 1.0);
            std::thread::sleep(Duration::from_millis(200));
            println!("after reset: {:?}", h.per_app_diagnostics());
        }
    })?;
    println!("tone frames rendered: {frames}");
    h.stop();
    Ok(())
}

/// Mute-only tap (CATapMuted, nobody reads it) on our own quiet tone.
#[cfg(target_os = "macos")]
fn tap_muted() -> anyhow::Result<()> {
    let h = AudioControl::start(Config { per_app_volume: true, ..Default::default() })?;
    let ((), frames) = macos_testing::with_quiet_tone(0.0005, || {
        std::thread::sleep(Duration::from_millis(400));
        let Some(me) = h.apps().into_iter().find(|a| a.pids.contains(&(std::process::id() as i32))) else {
            println!("our process is not listed");
            return;
        };
        println!("mute {}: {:?}", me.bundle_id, h.set_app_mute(&me.bundle_id, true));
        std::thread::sleep(Duration::from_millis(600));
        println!("diagnostics: {:?}", h.per_app_diagnostics());
        println!("apps: {:?}", h.apps().iter().map(|a| (&a.bundle_id, a.muted)).collect::<Vec<_>>());
        println!("unmute: {:?}", h.set_app_mute(&me.bundle_id, false));
        println!("diagnostics after: {:?}", h.per_app_diagnostics());
    })?;
    println!("tone frames rendered: {frames}; permission now {:?}", capture_permission());
    h.stop();
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("list") => list(),
        Some("watch") => watch(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10)),
        Some("volume") => volume(),
        Some("input") => input(),
        #[cfg(target_os = "macos")]
        Some("revert") => revert(),
        #[cfg(target_os = "macos")]
        Some("tap-muted") => tap_muted(),
        #[cfg(target_os = "macos")]
        Some("tap") => tap(args.iter().any(|a| a == "--force")),
        Some(other) => anyhow::bail!("unknown command {other}"),
    }
}
