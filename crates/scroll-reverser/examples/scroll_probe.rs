//! Print every scroll event's classification for N seconds (default 15).
//!
//! ```text
//! cargo run -p scroll-reverser --example scroll_probe -- 20          # observe + reverse (default config)
//! cargo run -p scroll-reverser --example scroll_probe -- 20 --dry    # observe only, reverse nothing
//! ```

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use scroll_reverser::{Config, ScrollEventInfo, ScrollReverser};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(15);
    let dry = args.iter().any(|a| a == "--dry");

    let config = if dry { Config { enabled: false, ..Config::default() } } else { Config::default() };
    println!("natural scrolling (system): {:?}", scroll_reverser::system_natural_scrolling());
    println!("needs permission: {}", scroll_reverser::needs_permission());
    println!("config: {config:?}");

    let (tx, rx) = mpsc::channel::<ScrollEventInfo>();
    let observer = Arc::new(move |e: &ScrollEventInfo| {
        let _ = tx.send(*e);
    });
    let mut handle = match ScrollReverser::start_with_observer(config, Some(observer)) {
        Ok(h) => h,
        Err(e) if scroll_reverser::is_permission_error(&e) => {
            eprintln!("cannot start: {e}");
            eprintln!("grant Accessibility to the app running this (e.g. your terminal) and retry");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("cannot start: {e:#}");
            std::process::exit(1);
        }
    };

    println!("scroll now; listening for {secs}s");
    let deadline = Instant::now() + Duration::from_secs(secs);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => println!(
                "{:<9} v={:>8.2} h={:>8.2} cont={} phase={:<3} momentum={} reversed(v={}, h={})",
                format!("{:?}", e.device),
                e.vertical,
                e.horizontal,
                e.continuous as u8,
                e.scroll_phase,
                e.momentum_phase,
                e.reversed.vertical as u8,
                e.reversed.horizontal as u8,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    handle.stop();
    println!("stopped");
}
