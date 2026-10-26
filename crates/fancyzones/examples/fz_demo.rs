//! FancyZones demo.
//!
//! ```text
//! cargo run -p fancyzones --example fz_demo                       # list monitors + zones
//! cargo run -p fancyzones --example fz_demo -- --layout grid --zones 4 --spacing 8
//! cargo run -p fancyzones --example fz_demo -- --snap 1           # focused window -> zone 1 (0-based)
//! cargo run -p fancyzones --example fz_demo -- --snap-dir right   # keyboard-style snap
//! cargo run -p fancyzones --example fz_demo -- --overlay 3        # draw zones overlay for 3 s
//! cargo run -p fancyzones --example fz_demo -- --watch 30         # run hooks (drag w/ Shift, hotkeys) for 30 s
//! cargo run -p fancyzones --example fz_demo -- --config c.json --save out.json --watch 30
//! cargo run -p fancyzones --example fz_demo -- --import-custom custom-layouts.json
//! ```
//! The config is read from `--config` or the default path; it is only
//! written to `--save <path>`.

use std::time::Duration;

use anyhow::{Context, Result};
use fancyzones::engine::{OverlayMonitor, OverlayState};
use fancyzones::platform::{self, NativeWindowSystem};
use fancyzones::{AppliedLayout, Config, Direction, Engine, FancyZones, LayoutType, StartOptions, WindowSystem};

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("fancyzones=info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let loaded = match arg(&args, "--config") {
        Some(p) => Config::load_from(std::path::Path::new(p)),
        None => Config::load_or_default(),
    };
    let mut config = loaded.unwrap_or_else(|e| {
        eprintln!("warning: could not load config ({e:#}); using defaults");
        Config::default()
    });

    if let Some(path) = arg(&args, "--import-custom") {
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let n = config.import_fancyzones_custom_layouts(&s)?;
        println!("imported {n} custom layouts:");
        for c in &config.custom_layouts {
            println!("  {} {:?} ({} zones)", c.uuid, c.name, c.zone_count());
        }
    }

    // Layout override for all monitors.
    if let Some(kind) = arg(&args, "--layout") {
        let kind: LayoutType = kind.parse().map_err(anyhow::Error::msg)?;
        let zones = arg(&args, "--zones").map(|z| z.parse()).transpose()?.unwrap_or(3);
        let spacing: i32 = arg(&args, "--spacing").map(|z| z.parse()).transpose()?.unwrap_or(16);
        config.settings.default_layout = AppliedLayout::template(kind, zones).with_spacing(spacing != 0, spacing);
        config.applied_layouts.clear();
    }

    let ws = NativeWindowSystem::new();
    println!("accessibility/window-control permission: {}", platform::has_permissions());
    let monitors = ws.monitors()?;
    let mut engine = Engine::new(config.clone());
    println!("virtual desktops / Spaces: {:?}", ws.virtual_desktops(&monitors));
    let areas = engine.areas(&ws)?;
    for (i, a) in areas.iter().enumerate() {
        let m = &a.monitor;
        println!(
            "monitor #{i} id={} name={:?} primary={} scale={}\n  frame={:?}\n  work_area={:?}\n  layout={} zone-count={} spacing={} -> {} zones",
            m.id, m.name, m.primary, m.scale, m.frame, m.work_area, a.applied.kind, a.applied.zone_count, a.applied.effective_spacing(), a.layout.zones.len()
        );
        for (zi, z) in a.layout.zones.iter().enumerate() {
            println!("    zone {zi}: x={} y={} w={} h={}", z.left, z.top, z.width(), z.height());
        }
    }

    if let Some(n) = arg(&args, "--snap") {
        let zone: usize = n.parse()?;
        let monitor = arg(&args, "--monitor");
        match ws.focused_window() {
            Ok(Some(w)) => println!("focused window rect before: {:?}", ws.window_rect(&w)),
            Ok(None) => println!("no focused window"),
            Err(e) => println!("cannot read focused window: {e:#}"),
        }
        match engine.snap_focused_to_zone(&ws, monitor, zone) {
            Ok(true) => {
                let w = ws.focused_window()?.context("focus lost")?;
                println!("snapped focused window to zone {zone}; rect now {:?}", ws.window_rect(&w)?);
            }
            Ok(false) => println!("snap did nothing"),
            Err(e) => {
                println!("snap failed (graceful): {e:#}");
                std::process::exit(2);
            }
        }
    }

    if let Some(d) = arg(&args, "--snap-dir") {
        let dir: Direction = d.parse().map_err(anyhow::Error::msg)?;
        match engine.snap_focused(&ws, dir) {
            Ok(r) => println!("snap {dir:?}: moved={r}"),
            Err(e) => {
                println!("snap failed (graceful): {e:#}");
                std::process::exit(2);
            }
        }
    }

    if let Some(secs) = arg(&args, "--overlay") {
        let secs: f64 = secs.parse()?;
        let mut st = OverlayState {
            show_numbers: true,
            zone_color: (0xAA, 0xCD, 0xFF),
            border_color: (0xFF, 0xFF, 0xFF),
            highlight_color: (0x00, 0x8C, 0xFF),
            number_color: (0, 0, 0),
            opacity: 0.5,
            ..Default::default()
        };
        for a in &areas {
            st.monitors.push(OverlayMonitor { frame: a.monitor.frame, zones: a.layout.zones.clone(), highlighted: vec![0] });
        }
        println!("showing overlay for {secs}s");
        #[cfg(target_os = "macos")]
        {
            platform::show_overlay(st);
            platform::run_main_loop_for(Duration::from_secs_f64(secs));
            platform::hide_overlay();
            platform::run_main_loop_for(Duration::from_millis(100));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = st;
            println!("(--overlay preview is macOS only; use --watch and drag with Shift on Windows)");
        }
    }

    if let Some(secs) = arg(&args, "--watch") {
        let secs: f64 = secs.parse()?;
        // --save <path>: persist changes there (never to the default config)
        let opts = StartOptions {
            config_path: arg(&args, "--save").map(std::path::PathBuf::from),
            on_open_editor: Some(Box::new(|m| println!("editor hotkey: monitor {m:?}"))),
        };
        let mut handle = match FancyZones::start_with(config, opts) {
            Ok(h) => h,
            Err(e) => {
                println!("FancyZones::start failed (graceful): {e:#}");
                std::process::exit(2);
            }
        };
        println!("FancyZones running for {secs}s: drag a window holding Shift, or use the snap hotkeys");
        #[cfg(target_os = "macos")]
        platform::run_main_loop_for(Duration::from_secs_f64(secs));
        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(Duration::from_secs_f64(secs));
        handle.stop();
        println!("stopped");
    }
    drop(engine);
    Ok(())
}
