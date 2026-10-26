//! Display Control glue for the settings window, which runs as another
//! process and never talks to the monitors itself: this app writes
//! `display-status.json` (the displays and their current values) every 2 s
//! and applies the brightness / contrast / volume / mute values the settings
//! window writes into `display-set.json` (polled every 150 ms, so sliders
//! feel live). Only fresh requests (< 3 s old) are applied.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime};

use pm_config::{AppConfig, DisplayRequest, DisplayStatus};

use crate::RUNNING;

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Start the status / request thread (once per process). It idles while
/// Display Control is off.
pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new()
            .name("display-status".into())
            .spawn(move || run(&path));
    });
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn run(path: &Path) {
    let req_path = DisplayRequest::path_for(path);
    let mut req_last = mtime(&req_path);
    let mut written = Instant::now() - Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let now = mtime(&req_path);
        if now != req_last {
            req_last = now;
            if now.is_some() {
                if let Some(r) = DisplayRequest::load_for(path).filter(|r| r.is_fresh(3_000)) {
                    apply_request(&r);
                    // Show the new value in the settings window soon.
                    written = Instant::now() - Duration::from_millis(1_600);
                }
            }
        }
        if written.elapsed() >= Duration::from_secs(2) {
            written = Instant::now();
            write_status(path);
        }
    }
}

fn apply_request(r: &DisplayRequest) {
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(h) = guard.as_ref().and_then(|x| x.display.as_ref()) else { return };
    let mut res = Vec::new();
    if let Some(v) = r.brightness {
        res.push(h.set_brightness(&r.id, v));
    }
    if let Some(v) = r.contrast {
        res.push(h.set_contrast(&r.id, v));
    }
    if let Some(v) = r.volume {
        res.push(h.set_volume(&r.id, v));
    }
    if let Some(m) = r.muted {
        res.push(h.set_mute(&r.id, m));
    }
    for e in res.into_iter().filter_map(Result::err) {
        log::warn!("display control: {}: {e:#}", r.id);
    }
}

fn write_status(path: &Path) {
    let st = {
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref().and_then(|x| x.display.as_ref()) {
            Some(h) => DisplayStatus { running: true, keys_active: h.keys_active(), displays: h.displays(), at: 0 },
            None => return,
        }
    };
    if let Err(e) = st.stamp().save_for(path) {
        log::debug!("display control: status file: {e:#}");
    }
}

/// Tell the settings window Display Control is off.
pub fn write_stopped() {
    if let Some(p) = config_path() {
        let _ = DisplayStatus::default().stamp().save_for(&p);
    }
}
