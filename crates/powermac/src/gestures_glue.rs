//! Gestures glue: the settings window (another process) records gestures
//! and sees the engine's state through `gestures-status.json` (written
//! every 2 s while the engine runs, and right after a recorded gesture) and
//! `gestures-request.json` (polled every 150 ms).
//!
//! Recording: the settings window writes `Record { on: true }` and renews
//! it every couple of seconds while its "Record gesture" button is active;
//! the app turns recording on (`Handle::set_recording`: the trackpad is read
//! even with nothing bound, no action runs) and reports each gesture. When
//! the window stops renewing (closed, crashed) recording ends by itself
//! after [`GESTURES_RECORD_KEEPALIVE_MS`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use gestures::Status;
use pm_config::bridge::{
    self, GesturesRequest, GesturesStatus, RecordedGesture, RequestReader, GESTURES_RECORD_KEEPALIVE_MS, GESTURES_REQUEST_FILE,
    GESTURES_STATUS_FILE,
};
use pm_config::AppConfig;

use crate::RUNNING;

/// Gestures seen while recording (newest last).
static RECORDED: Mutex<VecDeque<RecordedGesture>> = Mutex::new(VecDeque::new());
/// Something new to report: write the status file now.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// The settings window is recording.
static RECORDING: AtomicBool = AtomicBool::new(false);
const KEEP_RECORDED: usize = 8;

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Hook up a freshly started engine: gestures performed while recording
/// are kept for the status file.
pub fn attach(h: &gestures::Handle) {
    h.set_listener(Some(Box::new(|ev| {
        // While recording no action runs; outside recording the listener
        // sees every gesture, which is only interesting for the log.
        if RECORDING.load(Ordering::Relaxed) {
            let mut r = RECORDED.lock().unwrap_or_else(|p| p.into_inner());
            r.push_back(RecordedGesture { at: now_ms(), gesture: ev.gesture.to_string() });
            while r.len() > KEEP_RECORDED {
                r.pop_front();
            }
            DIRTY.store(true, Ordering::Relaxed);
        } else {
            log::debug!("gestures: {} → {}", ev.gesture, ev.action.as_ref().map(|a| a.label()).unwrap_or("nothing"));
        }
    })));
}

/// Start the status / request thread (once per process).
pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("gestures-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, GESTURES_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    // When recording was last asked for (None: not recording).
    let mut recording_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(GesturesRequest::Record { on }) = reader.poll::<GesturesRequest>(path, GESTURES_REQUEST_FILE) {
            if on && recording_since.is_none() {
                RECORDED.lock().unwrap_or_else(|p| p.into_inner()).clear();
                log::info!("gestures: recording (settings)");
            }
            if !on && recording_since.is_some() {
                log::info!("gestures: recording stopped");
            }
            if on != recording_since.is_some() {
                set_recording(on);
                DIRTY.store(true, Ordering::Relaxed);
            }
            recording_since = on.then(Instant::now);
        }
        if recording_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(GESTURES_RECORD_KEEPALIVE_MS)) {
            log::info!("gestures: recording ended (the settings window stopped asking)");
            recording_since = None;
            set_recording(false);
            DIRTY.store(true, Ordering::Relaxed);
        }
        if DIRTY.swap(false, Ordering::Relaxed) || written.elapsed() >= Duration::from_secs(2) {
            written = Instant::now();
            let st = {
                let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_ref().and_then(|r| r.gestures.as_ref()).map(|h| snapshot(&h.status(), recording_since.is_some()))
            };
            match st {
                Some(st) => {
                    if let Err(e) = bridge::write(path, GESTURES_STATUS_FILE, &st) {
                        log::debug!("gestures: status file: {e:#}");
                    }
                }
                None => recording_since = None,
            }
        }
    }
}

fn set_recording(on: bool) {
    RECORDING.store(on, Ordering::Relaxed);
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = guard.as_ref().and_then(|r| r.gestures.as_ref()) {
        h.set_recording(on);
    }
}

/// The status file contents for an engine status.
pub(crate) fn snapshot(s: &Status, recording: bool) -> GesturesStatus {
    let recorded = RECORDED.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
    let (state, detail, devices, clicks_error) = match s {
        Status::Off => ("off", String::new(), 0, None),
        Status::Running { devices, clicks } => (
            "running",
            format!("{} trackpad{}", devices.len(), if devices.len() == 1 { "" } else { "s" }),
            devices.len(),
            clicks.as_ref().and_then(|c| c.as_ref().err().cloned()),
        ),
        Status::Unavailable(e) => ("unavailable", e.clone(), 0, None),
    };
    GesturesStatus { state: state.into(), detail, devices, clicks_error, recording, recorded }
}

/// Tell the settings window the engine is off.
pub fn write_stopped() {
    if let Some(p) = config_path() {
        let _ = bridge::write(&p, GESTURES_STATUS_FILE, &GesturesStatus { state: "off".into(), ..Default::default() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let s = snapshot(&Status::Unavailable("gestures unavailable: no framework".into()), false);
        assert_eq!(s.state, "unavailable");
        assert!(s.detail.contains("no framework"));
        let s = snapshot(&Status::Running { devices: vec![], clicks: Some(Err("no hook".into())) }, true);
        assert_eq!((s.state.as_str(), s.detail.as_str(), s.recording), ("running", "0 trackpads", true));
        assert_eq!(s.clicks_error.as_deref(), Some("no hook"));
        assert_eq!(snapshot(&Status::Off, false).state, "off");
    }
}
