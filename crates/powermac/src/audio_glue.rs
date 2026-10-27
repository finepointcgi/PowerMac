//! Sound glue: the settings window (another process) sees the devices,
//! the apps playing audio and the capture permission through
//! `audio-status.json` (written every 2 s while the module runs) and changes
//! the default device, device levels and per-app levels through
//! `audio-request.json` (polled every 150 ms). Per-app levels changed
//! anywhere (tray presets, settings sliders) are saved into the config's
//! `appVolumes` a moment after the last change, and routes / the mixer
//! changed through the handle (`RoutesChanged` / `MixerChanged`) into
//! `appRoutes` / `mixer` the same way.
//!
//! Meters: while the settings window shows the mixer it renews
//! `audio-watch.json` every second; while that is fresh a second thread
//! writes `audio-meters.json` (`Handle::meters`) every 50 ms. Nothing is
//! written at 20 Hz otherwise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use audio_control::{CapturePermission, DeviceKind, Event};
use pm_config::bridge::{
    self, AudioRequest, AudioStatus, AudioWatch, RequestReader, AUDIO_METERS_FILE, AUDIO_REQUEST_FILE, AUDIO_STATUS_FILE, AUDIO_WATCH_FILE,
    AUDIO_WATCH_FRESH_MS,
};
use pm_config::AppConfig;

use crate::RUNNING;

/// A per-app level changed and isn't saved yet.
static LEVELS_DIRTY: AtomicBool = AtomicBool::new(false);
/// Routes changed through the handle and aren't saved yet.
static ROUTES_DIRTY: AtomicBool = AtomicBool::new(false);
/// The mixer changed through the handle and isn't saved yet.
static MIXER_DIRTY: AtomicBool = AtomicBool::new(false);
/// The settings window shows Sound / the mixer (`audio-watch.json` is fresh).
static WATCHED: AtomicBool = AtomicBool::new(false);
/// ... and wants the meters.
static METERS_WANTED: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Hook up a freshly started module's events.
pub fn attach(h: &audio_control::Handle) {
    h.set_event_callback(|ev| match ev {
        Event::AppLevelChanged { .. } => LEVELS_DIRTY.store(true, Ordering::Relaxed),
        Event::RoutesChanged => ROUTES_DIRTY.store(true, Ordering::Relaxed),
        Event::MixerChanged => MIXER_DIRTY.store(true, Ordering::Relaxed),
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
        let p2 = path.clone();
        let _ = std::thread::Builder::new().name("audio-status".into()).spawn(move || run(&path));
        let _ = std::thread::Builder::new().name("audio-meters".into()).spawn(move || run_meters(&p2));
    });
}

/// Write the meters at ~20 Hz while the settings window asks for them.
fn run_meters(path: &Path) {
    let mut checked = Instant::now() - Duration::from_secs(1);
    loop {
        if checked.elapsed() >= Duration::from_millis(500) {
            checked = Instant::now();
            let w = bridge::read_fresh::<AudioWatch>(path, AUDIO_WATCH_FILE, AUDIO_WATCH_FRESH_MS);
            WATCHED.store(w.is_some(), Ordering::Relaxed);
            METERS_WANTED.store(w.is_some_and(|w| w.meters), Ordering::Relaxed);
        }
        if !METERS_WANTED.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let t = Instant::now();
        // Never wait for the config watcher here (it may hold RUNNING for a while).
        let meters = match RUNNING.try_lock() {
            Ok(g) => g.as_ref().and_then(|r| r.audio.as_ref()).map(|h| h.meters()),
            Err(_) => None,
        };
        if let Some(m) = meters {
            if let Err(e) = bridge::write(path, AUDIO_METERS_FILE, &m) {
                log::debug!("sound: meters file: {e:#}");
            }
        }
        std::thread::sleep(audio_control::METER_INTERVAL.saturating_sub(t.elapsed()));
    }
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, AUDIO_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    let mut dirty_since: Option<Instant> = None;
    let mut mixer_dirty_since: Option<Instant> = None;
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
        let routes = ROUTES_DIRTY.swap(false, Ordering::Relaxed);
        let mixer = MIXER_DIRTY.swap(false, Ordering::Relaxed);
        if routes || mixer {
            mixer_dirty_since = Some(Instant::now());
            PENDING_SAVE.fetch_or(u8::from(routes) | (u8::from(mixer) << 1), Ordering::Relaxed);
            // The status shows the new strips soon.
            written = written.min(Instant::now() - Duration::from_millis(1_500));
        }
        if mixer_dirty_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(600)) {
            mixer_dirty_since = None;
            save_mixer(PENDING_SAVE.swap(0, Ordering::Relaxed));
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
        AudioRequest::SetMixer { mixer } => h.set_mixer(mixer),
        AudioRequest::SetRoute { bundle_id, route } => h.set_route(&bundle_id, route),
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

/// Bit 0: routes, bit 1: the mixer (what [`save_mixer`] writes).
static PENDING_SAVE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Save the running module's routes and / or mixer (`what`: bit 0 routes,
/// bit 1 mixer) into `audio.appRoutes` / `audio.mixer`. Only the part that
/// changed through the handle is written, so a newer edit the settings
/// window saved to the other part is never overwritten.
fn save_mixer(what: u8) {
    let (routes, mixer) = {
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref().and_then(|r| r.audio.as_ref()) {
            Some(h) => (h.routes(), h.mixer()),
            None => return,
        }
    };
    let cur = crate::current_config().audio;
    let routes = (what & 1 != 0 && cur.app_routes != routes).then_some(routes);
    let mixer = (what & 2 != 0 && cur.mixer != mixer).then_some(mixer);
    if routes.is_none() && mixer.is_none() {
        return;
    }
    let r = crate::edit_config(|c| {
        if let Some(r) = routes {
            c.audio.app_routes = r;
        }
        if let Some(m) = mixer {
            c.audio.mixer = m;
        }
    });
    if let Err(e) = r {
        log::warn!("sound: saving routes / mixer: {e:#}");
    }
}

/// Whether the apps playing audio should be listed (per-app volume, a
/// route or the mixer is on, or the settings window shows Sound).
fn wants_apps(c: &audio_control::Config) -> bool {
    c.per_app_volume || !c.app_routes.is_empty() || c.mixer.enabled || WATCHED.load(Ordering::Relaxed)
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
        mixer: Some(h.mixer_state()),
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
        let c = h.config();
        let per_app = c.per_app_volume;
        let st = snapshot(h, wants_apps(&c));
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

// ---------------------------------------------------------------------------
// The Notch Shelf's Sound widget

/// Audio control for the Notch Shelf's Sound widget.
///
/// The shelf calls this on its provider thread. Reading never needs
/// `RUNNING`: devices, the default output and its level come from a one-shot
/// CoreAudio snapshot (cached for a moment, so one widget refresh asks
/// once). Changes go through the running Sound module when there is one (so
/// its auto-switch rules know the switch was manual); `RUNNING` is only
/// tried, and when it's busy (the config watcher may hold it while it waits
/// for the main thread) the change is applied from a short-lived thread
/// instead of waiting here. Without the module, the default device is set
/// directly and the volume / mute through `osascript`.
pub(crate) struct NotchSound {
    cache: Mutex<Option<(Instant, Vec<audio_control::AudioDevice>)>>,
}

/// How long a device snapshot is reused.
const SOUND_CACHE: Duration = Duration::from_millis(400);

/// What the Sound widget asked for.
#[derive(Clone, Debug)]
enum SoundChange {
    Device(String),
    Volume(f32),
    Mute(bool),
}

pub(crate) fn notch_sound() -> std::sync::Arc<dyn notch_shelf::SoundProvider> {
    std::sync::Arc::new(NotchSound { cache: Mutex::new(None) })
}

impl NotchSound {
    fn devices(&self) -> Vec<audio_control::AudioDevice> {
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, list)) = c.as_ref() {
            if at.elapsed() < SOUND_CACHE {
                return list.clone();
            }
        }
        let list = audio_control::list_devices(DeviceKind::Output).unwrap_or_else(|e| {
            log::debug!("sound (notch): {e:#}");
            Vec::new()
        });
        *c = Some((Instant::now(), list.clone()));
        list
    }

    fn default_device(&self) -> Option<audio_control::AudioDevice> {
        self.devices().into_iter().find(|d| d.is_default)
    }

    fn invalidate(&self) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn change(&self, ch: SoundChange) -> Result<(), String> {
        let target = match &ch {
            SoundChange::Device(_) => None,
            _ => Some(self.default_device().ok_or("No output device")?.uid),
        };
        self.invalidate();
        // Fast path: RUNNING is free.
        if let Ok(guard) = RUNNING.try_lock() {
            return match guard.as_ref().and_then(|r| r.audio.as_ref()) {
                Some(h) => apply_with_handle(h, &ch, target.as_deref()).map_err(|e| format!("{e:#}")),
                None => {
                    drop(guard);
                    apply_without_module(&ch)
                }
            };
        }
        // RUNNING is busy: never wait for it on this thread.
        std::thread::spawn(move || {
            let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            let r = match guard.as_ref().and_then(|r| r.audio.as_ref()) {
                Some(h) => apply_with_handle(h, &ch, target.as_deref()).map_err(|e| format!("{e:#}")),
                None => {
                    drop(guard);
                    apply_without_module(&ch)
                }
            };
            if let Err(e) = r {
                log::info!("sound (notch): {e}");
            }
        });
        Ok(())
    }
}

fn apply_with_handle(h: &audio_control::Handle, ch: &SoundChange, default_uid: Option<&str>) -> anyhow::Result<()> {
    let r = match ch {
        SoundChange::Device(uid) => h.set_default_output(uid),
        SoundChange::Volume(v) => h.set_device_volume(default_uid.unwrap_or_default(), *v),
        SoundChange::Mute(m) => h.set_device_mute(default_uid.unwrap_or_default(), *m),
    };
    refresh_soon();
    r
}

/// The Sound module is off: set the default directly; volume and mute of
/// the default output through AppleScript's `set volume` (no prompt).
fn apply_without_module(ch: &SoundChange) -> Result<(), String> {
    match ch {
        SoundChange::Device(uid) => audio_control::set_default_device(DeviceKind::Output, uid).map_err(|e| format!("{e:#}")),
        SoundChange::Volume(v) => osascript_volume(&format!("set volume output volume {}", (v.clamp(0.0, 1.0) * 100.0).round() as u32)),
        SoundChange::Mute(m) => osascript_volume(&format!("set volume output muted {m}")),
    }
}

#[cfg(target_os = "macos")]
fn osascript_volume(script: &str) -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/osascript").args(["-e", script]).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(not(target_os = "macos"))]
fn osascript_volume(_script: &str) -> Result<(), String> {
    Err("Turn on Sound in PowerMac to change the volume here".into())
}

impl notch_shelf::SoundProvider for NotchSound {
    fn outputs(&self) -> Vec<notch_shelf::AudioOutput> {
        self.devices()
            .into_iter()
            .map(|d| {
                let mut o = notch_shelf::AudioOutput::new(&d.uid, &d.name);
                if d.transport.is_bluetooth() && notch_shelf::widgets::sound::guess_symbol(&d.name) == "hifispeaker.fill" {
                    o.symbol = "headphones".into();
                }
                o
            })
            .collect()
    }

    fn default_output(&self) -> Option<String> {
        self.default_device().map(|d| d.uid)
    }

    fn set_default_output(&self, id: &str) -> Result<(), String> {
        self.change(SoundChange::Device(id.to_string()))
    }

    fn volume(&self) -> Option<f32> {
        self.default_device().and_then(|d| d.volume)
    }

    fn set_volume(&self, v: f32) -> Result<(), String> {
        self.change(SoundChange::Volume(v.clamp(0.0, 1.0)))
    }

    fn muted(&self) -> Option<bool> {
        self.default_device().and_then(|d| d.muted)
    }

    fn set_muted(&self, muted: bool) -> Result<(), String> {
        self.change(SoundChange::Mute(muted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notch_shelf::SoundProvider;

    #[test]
    fn notch_sound_reads_without_running_and_caches() {
        let s = NotchSound { cache: Mutex::new(None) };
        // Whatever the machine has (CI may have no devices): consistent.
        let outs = s.outputs();
        if let Some(def) = s.default_output() {
            assert!(outs.iter().any(|o| o.id == def));
        }
        let t = s.cache.lock().unwrap().as_ref().map(|(t, _)| *t);
        let _ = s.volume();
        let t2 = s.cache.lock().unwrap().as_ref().map(|(t, _)| *t);
        assert_eq!(t, t2, "reused within the cache window");
        s.invalidate();
        assert!(s.cache.lock().unwrap().is_none());
    }
}
