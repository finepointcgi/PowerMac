//! Sound glue: the settings window (another process) sees the devices,
//! the apps playing audio and the capture permission through
//! `audio-status.json` (written every 2 s while the module runs) and changes
//! the default device, device levels and per-app levels through
//! `audio-request.json` (polled every 150 ms). Per-app levels changed
//! anywhere (tray presets, settings sliders) are saved into the config's
//! `appVolumes` a moment after the last change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use audio_control::{CapturePermission, DeviceKind, Event};
use pm_config::bridge::{self, AudioRequest, AudioStatus, RequestReader, AUDIO_REQUEST_FILE, AUDIO_STATUS_FILE};
use pm_config::AppConfig;

use crate::RUNNING;

/// A per-app level changed and isn't saved yet.
static LEVELS_DIRTY: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Hook up a freshly started module's events.
pub fn attach(h: &audio_control::Handle) {
    h.set_event_callback(|ev| match ev {
        Event::AppLevelChanged { .. } => LEVELS_DIRTY.store(true, Ordering::Relaxed),
        Event::RuleApplied { kind, name, reason, .. } => {
            log::info!("sound: switched the {} to {name} ({reason:?})", kind_name(*kind))
        }
        Event::Error { message } => log::warn!("sound: {message}"),
        _ => {}
    });
}

fn kind_name(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Output => "output",
        DeviceKind::Input => "input",
        DeviceKind::SystemOutput => "sound-effects output",
    }
}

/// Start the status / request thread (once per process). It idles while
/// the module is off.
pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("audio-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, AUDIO_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    let mut dirty_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(req) = reader.poll::<AudioRequest>(path, AUDIO_REQUEST_FILE) {
            apply_request(req);
            written = Instant::now() - Duration::from_millis(1_700);
        }
        if LEVELS_DIRTY.swap(false, Ordering::Relaxed) {
            dirty_since = Some(Instant::now());
        }
        if dirty_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(600)) {
            dirty_since = None;
            save_levels();
        }
        if REFRESH.swap(false, Ordering::Relaxed) {
            written = Instant::now() - Duration::from_millis(1_700);
        }
        if written.elapsed() >= Duration::from_secs(2) {
            written = Instant::now();
            write_status(path);
        }
    }
}

fn set_error(r: anyhow::Result<()>) {
    let e = r.err().map(|e| {
        log::warn!("sound: {e:#}");
        format!("{e:#}")
    });
    *LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = e;
}

fn apply_request(req: AudioRequest) {
    if req == AudioRequest::RequestPermission {
        // Blocks until the user answers: never while holding RUNNING.
        std::thread::spawn(|| match audio_control::request_capture_permission() {
            Ok(granted) => log::info!("sound: audio capture permission {}", if granted { "granted" } else { "denied" }),
            Err(e) => log::warn!("sound: asking for the audio capture permission: {e:#}"),
        });
        return;
    }
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(h) = guard.as_ref().and_then(|r| r.audio.as_ref()) else { return };
    let r = match req {
        AudioRequest::SetDefault { kind: DeviceKind::Output, uid } => h.set_default_output(&uid),
        AudioRequest::SetDefault { kind: DeviceKind::Input, uid } => h.set_default_input(&uid),
        AudioRequest::SetDefault { kind: DeviceKind::SystemOutput, uid } => h.set_default_system_output(&uid),
        AudioRequest::DeviceVolume { uid, volume } => h.set_device_volume(&uid, volume),
        AudioRequest::DeviceMute { uid, muted } => h.set_device_mute(&uid, muted),
        AudioRequest::AppVolume { bundle_id, volume } => h.set_app_volume(&bundle_id, volume),
        AudioRequest::AppMute { bundle_id, muted } => h.set_app_mute(&bundle_id, muted),
        AudioRequest::RequestPermission => Ok(()),
    };
    set_error(r);
}

/// Save the running module's per-app levels into `audio.appVolumes`.
fn save_levels() {
    let levels = {
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref().and_then(|r| r.audio.as_ref()) {
            Some(h) => h.app_levels(),
            None => return,
        }
    };
    let r = crate::edit_config(|c| {
        if c.audio.app_volumes != levels {
            c.audio.app_volumes = levels;
        }
    });
    if let Err(e) = r {
        log::warn!("sound: saving app levels: {e:#}");
    }
}

/// What the tray and the status file show.
pub(crate) fn snapshot(h: &audio_control::Handle, with_apps: bool) -> AudioStatus {
    AudioStatus {
        running: true,
        outputs: h.outputs(),
        inputs: h.inputs(),
        apps: if with_apps { h.apps() } else { Vec::new() },
        per_app_supported: audio_control::per_app_volume_supported(),
        capture_permission: h.capture_permission(),
        error: LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()).clone(),
    }
}

/// The last status written (the tray menu reads it instead of asking
/// CoreAudio on the main thread), with whether per-app volume is on.
static LATEST: Mutex<Option<(AudioStatus, bool)>> = Mutex::new(None);

/// What the tray shows: the latest devices and apps, `None` while off.
pub(crate) fn latest() -> Option<(AudioStatus, bool)> {
    LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Refresh the status soon (after the tray changed something).
static REFRESH: AtomicBool = AtomicBool::new(false);

pub(crate) fn refresh_soon() {
    REFRESH.store(true, Ordering::Relaxed);
}

fn write_status(path: &Path) {
    let st = {
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(h) = guard.as_ref().and_then(|r| r.audio.as_ref()) else {
            *LATEST.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return;
        };
        let per_app = h.config().per_app_volume;
        let st = snapshot(h, per_app);
        *LATEST.lock().unwrap_or_else(|p| p.into_inner()) = Some((st.clone(), per_app));
        st
    };
    if let Err(e) = bridge::write(path, AUDIO_STATUS_FILE, &st) {
        log::debug!("sound: status file: {e:#}");
    }
}

/// Tell the settings window the module is off.
pub fn write_stopped() {
    *LATEST.lock().unwrap_or_else(|p| p.into_inner()) = None;
    if let Some(p) = config_path() {
        let st = AudioStatus { capture_permission: CapturePermission::NotApplicable, ..Default::default() };
        let _ = bridge::write(&p, AUDIO_STATUS_FILE, &st);
    }
}
