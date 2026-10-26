//! List displays and their brightness / contrast / volume.
//!
//! ```text
//! cargo run -p display-control --example display_probe                 # list (+ macOS diagnostics)
//! cargo run -p display-control --example display_probe -- --json       # list as JSON
//! cargo run -p display-control --example display_probe -- --set 1 brightness 40
//! cargo run -p display-control --example display_probe -- --set "DELL U2720Q#ABC" volume 20
//! cargo run -p display-control --example display_probe -- --noop-writes  # macOS: write paths, no visible change
//! cargo run -p display-control --example display_probe -- --osd          # macOS: show the OSD on each display
//! cargo run -p display-control --example display_probe -- --keys 30      # take over the keys for 30 s
//! ```
//!
//! `--set` changes the monitor and leaves it changed. Feature names:
//! brightness, contrast, volume, mute (0/1).

use std::time::Duration;

use display_control::{Config, DisplayControl, DisplayInfo, Feature};

fn usage() -> ! {
    eprintln!("usage: display_probe [--json] [--set <index|id> <brightness|contrast|volume|mute> <value>] [--noop-writes] [--osd] [--keys <secs>]");
    std::process::exit(2)
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
}

fn print_table(ds: &[DisplayInfo]) {
    for (i, d) in ds.iter().enumerate() {
        println!(
            "[{i}] {:?}\n    name={:?} builtin={} bounds={},{} {}x{} ddc={} method={:?}\n    brightness={}% (max {}) contrast={}% (max {}) volume={}% (max {}) muted={} input={}",
            d.id,
            d.name,
            d.is_builtin,
            d.bounds.x,
            d.bounds.y,
            d.bounds.width,
            d.bounds.height,
            d.supports_ddc,
            d.brightness_method,
            fmt_opt(d.brightness),
            fmt_opt(d.max_brightness),
            fmt_opt(d.contrast),
            fmt_opt(d.max_contrast),
            fmt_opt(d.volume),
            fmt_opt(d.max_volume),
            fmt_opt(d.muted),
            d.input_source.map(|v| format!("0x{v:02X} ({})", display_control::input_source_name(v))).unwrap_or_else(|| "-".into()),
        );
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut set: Option<(String, String, u8)> = None;
    let mut noop = false;
    let mut osd = false;
    let mut keys: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--noop-writes" => noop = true,
            "--osd" => osd = true,
            "--keys" => {
                keys = Some(args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()));
                i += 1;
            }
            "--set" => {
                let (Some(t), Some(f), Some(v)) = (args.get(i + 1), args.get(i + 2), args.get(i + 3).and_then(|v| v.parse().ok())) else { usage() };
                set = Some((t.clone(), f.clone(), v));
                i += 3;
            }
            _ => usage(),
        }
        i += 1;
    }

    #[cfg(target_os = "macos")]
    if !json {
        println!("--- macOS diagnostics ---\n{}", display_control::debug_report());
    }
    #[cfg(target_os = "macos")]
    if noop {
        for l in display_control::debug_noop_writes() {
            println!("{l}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = noop;

    // Keys off unless --keys: the probe must not interfere with the system.
    let mut cfg = Config { take_over_brightness_keys: false, take_over_volume_keys: false, ..Default::default() };
    if keys.is_some() {
        cfg.take_over_brightness_keys = true;
        cfg.take_over_volume_keys = true;
    }
    let h = DisplayControl::start(cfg)?;
    if !h.refresh_blocking(Duration::from_secs(30)) {
        eprintln!("warning: refresh timed out");
    }
    let ds = h.displays();
    if json {
        println!("{}", serde_json::to_string_pretty(&ds)?);
    } else {
        println!("--- displays ---");
        print_table(&ds);
    }

    if let Some((target, feature, value)) = set {
        let d = target.parse::<usize>().ok().and_then(|i| ds.get(i)).or_else(|| ds.iter().find(|d| d.id == target)).ok_or_else(|| anyhow::anyhow!("no display {target:?}"))?;
        let f = match feature.as_str() {
            "brightness" => Feature::Brightness,
            "contrast" => Feature::Contrast,
            "volume" => Feature::Volume,
            "mute" => Feature::Mute,
            _ => usage(),
        };
        match f {
            Feature::Brightness => h.set_brightness(&d.id, value)?,
            Feature::Contrast => h.set_contrast(&d.id, value)?,
            Feature::Volume => h.set_volume(&d.id, value)?,
            _ => h.set_mute(&d.id, value != 0)?,
        }
        h.flush(Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(300));
        h.refresh_blocking(Duration::from_secs(30));
        println!("--- after --set ---");
        print_table(&h.displays().into_iter().filter(|x| x.id == d.id).collect::<Vec<_>>());
    }

    #[cfg(target_os = "macos")]
    if osd {
        for (n, d) in ds.iter().enumerate() {
            let kind = if n % 2 == 0 { display_control::OsdKind::Brightness } else { display_control::OsdKind::Volume };
            display_control::debug_show_osd(d.bounds, kind, d.brightness.unwrap_or(50));
        }
        display_control::run_main_loop_for(Duration::from_secs(2));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = osd;

    if let Some(secs) = keys {
        println!("keys active: {} — press brightness / volume keys with the pointer on an external display ({secs} s)", h.keys_active());
        #[cfg(target_os = "macos")]
        display_control::run_main_loop_for(Duration::from_secs(secs));
        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(Duration::from_secs(secs));
    }
    h.stop();
    Ok(())
}
