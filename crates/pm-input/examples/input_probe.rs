//! Manual test for pm-input: prints screens, permission state and every captured
//! event for a few seconds.
//!
//! ```text
//! cargo run -p pm-input --example input_probe -- [--secs N] [--inject] [--prompt]
//! ```
//! `--inject` moves the cursor in a small circle (and back) after one second.
//! Injected events are tagged, so they must NOT show up in the captured output;
//! only the "injected:" summary line is printed for them.
//! `--prompt` asks macOS to show the Accessibility / Input Monitoring prompts.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pm_input::{Disposition, InputEvent, MouseEvent};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inject = args.iter().any(|a| a == "--inject");
    let prompt = args.iter().any(|a| a == "--prompt");
    let secs: u64 = args
        .iter()
        .position(|a| a == "--secs")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let be = pm_input::backend()?;
    for (i, s) in be.screens()?.iter().enumerate() {
        println!("screen {i}: {s:?} ({}x{})", s.width(), s.height());
    }
    println!("desktop: {:?}", be.desktop_bounds()?);
    println!("cursor: {:?}", be.cursor_pos()?);
    let perms = be.has_permissions(prompt);
    println!("permissions: {perms}");

    let count = Arc::new(AtomicU64::new(0));
    let c2 = count.clone();
    let cap = match be.start_capture(Arc::new(move |ev: &InputEvent| {
        c2.fetch_add(1, Ordering::Relaxed);
        match ev {
            InputEvent::Mouse(MouseEvent::Move { x, y, dx, dy }) => {
                println!("move  ({x},{y}) d=({dx},{dy})")
            }
            other => println!("{other:?}"),
        }
        Disposition::Pass
    })) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("capture failed: {e:#}");
            if !perms {
                eprintln!(
                    "hint: grant Accessibility (and Input Monitoring) to this terminal in \
                     System Settings > Privacy & Security, or re-run with --prompt"
                );
            }
            if !inject {
                std::process::exit(2);
            }
            None
        }
    };
    if cap.is_some() {
        println!("capturing for {secs}s ...");
    }

    let start = Instant::now();
    if inject {
        std::thread::sleep(Duration::from_secs(1));
        let (cx, cy) = be.cursor_pos()?;
        let before = count.load(Ordering::Relaxed);
        let n = 60;
        let mut errors = 0;
        for i in 0..=n {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            let x = cx + (100.0 * a.cos()) as i32 - 100;
            let y = cy + (100.0 * a.sin()) as i32;
            if let Err(e) = be.inject_mouse(MouseEvent::Move { x, y, dx: 0, dy: 0 }) {
                errors += 1;
                if errors == 1 {
                    eprintln!("inject failed: {e:#}");
                }
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        be.set_cursor_pos(cx, cy)?;
        std::thread::sleep(Duration::from_millis(100));
        let leaked = count.load(Ordering::Relaxed) - before;
        println!(
            "injected: {} moves, {errors} errors, cursor back at {:?}, events captured meanwhile: {leaked} \
             (should be ~0 unless you moved the mouse)",
            n + 1,
            be.cursor_pos()?
        );
    }
    if cap.is_none() {
        std::process::exit(2);
    }
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(cap);
    println!("done; {} events captured", count.load(Ordering::Relaxed));
    Ok(())
}
