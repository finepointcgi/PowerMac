//! Starting, stopping and live-reconfiguring the modules from `AppConfig`.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use pm_config::AppConfig;
use crate::{clipboard, Running, RUNNING};

fn changed<T: serde::Serialize>(old: Option<&T>, new: &T) -> bool {
    match old {
        None => true,
        Some(o) => serde_json::to_value(o).ok() != serde_json::to_value(new).ok(),
    }
}

/// Bring `running` in line with `new`. `old` is the config currently applied
/// (`None` on startup); modules whose settings didn't change are left alone.
pub fn apply(running: &mut Running, old: Option<&AppConfig>, new: &AppConfig) {
    // Awake always exists (passive unless enabled) so the tray can switch it on.
    if running.awake.is_none() {
        match awake::Awake::new() {
            Ok(a) => running.awake = Some(a),
            Err(e) => log::error!("awake: {e}"),
        }
    }
    if changed(old.map(|o| &o.awake), &new.awake) {
        if let Some(a) = &running.awake {
            if new.awake.enabled {
                match a.set(new.awake.settings) {
                    Ok(()) => log::info!("awake: {:?}", new.awake.settings.mode),
                    Err(e) => log::error!("awake: {e}"),
                }
            } else if old.is_some() {
                a.stop();
            }
        }
    }

    clipboard::SHARE.store(new.mwb.enabled && new.mwb.settings.share_clipboard, Ordering::Relaxed);
    if changed(old.map(|o| &o.mwb), &new.mwb) {
        if let Some(m) = running.mwb.take() {
            m.stop();
            log::info!("mouse without borders: stopped");
        }
        if new.mwb.enabled {
            running.mwb = start_mwb(&new.mwb.settings);
        }
    }

    if changed(old.map(|o| &o.scroll_reverser), &new.scroll_reverser) {
        apply_scroll(running, &new.scroll_reverser);
    }

    if changed(old.map(|o| &o.mouse_utils), &new.mouse_utils) {
        let m = &new.mouse_utils;
        let any = m.find_my_mouse.enabled || m.highlighter.enabled || m.crosshairs.enabled;
        match (any, running.mouse.as_mut()) {
            (true, Some(h)) => h.update(m.clone()),
            (true, None) => match mouse_utils::MouseUtils::start(m.clone()) {
                Ok(h) => {
                    log::info!("mouse utilities: running ({:?} input)", h.input_mode());
                    running.mouse = Some(h);
                }
                Err(e) => log::error!("mouse utilities: {e:#}"),
            },
            (false, Some(_)) => {
                if let Some(mut h) = running.mouse.take() {
                    h.stop();
                }
            }
            (false, None) => {}
        }
    }

    if changed(old.map(|o| &o.keyboard_manager), &new.keyboard_manager) {
        let k = &new.keyboard_manager;
        match (k.enabled, running.keyboard.as_ref()) {
            (true, Some(h)) => {
                if let Err(e) = h.update(k.clone()) {
                    log::error!("keyboard manager: {e:#}");
                }
            }
            (true, None) => match keyboard_manager::KeyboardManager::start(k.clone()) {
                Ok(h) => {
                    log::info!("keyboard manager: running");
                    running.keyboard = Some(h);
                }
                Err(e) => log::error!("keyboard manager: {e:#}"),
            },
            (false, Some(_)) => {
                if let Some(h) = running.keyboard.take() {
                    h.stop();
                }
                log::info!("keyboard manager: stopped");
            }
            (false, None) => {}
        }
    }

    if changed(old.map(|o| &o.fancyzones), &new.fancyzones) {
        match (new.fancyzones.enabled, running.zones.is_some()) {
            (true, false) => {
                let started = fancyzones::Config::load_or_default().and_then(fancyzones::FancyZones::start);
                match started {
                    Ok(h) => {
                        log::info!("fancyzones: running");
                        running.zones = Some(h);
                    }
                    Err(e) => log::error!("fancyzones: {e:#}"),
                }
            }
            (false, true) => {
                if let Some(mut z) = running.zones.take() {
                    z.stop();
                }
                log::info!("fancyzones: stopped");
            }
            _ => {}
        }
    }
}

fn apply_scroll(running: &mut Running, cfg: &scroll_reverser::Config) {
    match (cfg.enabled, running.scroll.as_mut()) {
        (true, Some(h)) => h.update(*cfg),
        (true, None) => match scroll_reverser::ScrollReverser::start(*cfg) {
            Ok(h) => {
                log::info!("scroll reverser: running");
                running.scroll = Some(h);
            }
            Err(e) => log::error!("scroll reverser: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(mut h) = running.scroll.take() {
                h.stop();
            }
            log::info!("scroll reverser: stopped");
        }
        (false, None) => {}
    }
}

fn start_mwb(settings: &mwb::Config) -> Option<mwb::EngineHandle> {
    let backend = match pm_input::backend() {
        Ok(b) => b,
        Err(e) => {
            log::error!("mouse without borders: {e:#}");
            return None;
        }
    };
    if !backend.has_permissions(true) {
        log::error!("mouse without borders needs Accessibility + Input Monitoring permission; \
                     grant it in System Settings → Privacy & Security and restart");
        return None;
    }
    if !mwb::crypto::is_key_valid(&settings.key) {
        log::error!("mouse without borders: key missing or shorter than 16 chars (run `powermac init`)");
        return None;
    }
    match mwb::Engine::start(settings.clone(), backend) {
        Ok(h) => {
            log::info!("mouse without borders: {} listening on {}", h.machine_name(), h.local_addr());
            clipboard::forward_events(h.events());
            Some(h)
        }
        Err(e) => {
            log::error!("mouse without borders: {e:#}");
            None
        }
    }
}

/// Re-apply the config file whenever it changes on disk (hand edits, the
/// settings window, or the tray writing it).
pub fn watch(path: &Path, mut current: AppConfig) {
    let path = path.to_path_buf();
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut last: Option<SystemTime> = mtime(&path);
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = mtime(&path);
            if now == last {
                continue;
            }
            last = now;
            let new = match AppConfig::load_or_default(&path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("config not reloaded: {e:#}");
                    continue;
                }
            };
            log::info!("config changed, applying");
            let mut guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(running) = guard.as_mut() else { return };
            apply(running, Some(&current), &new);
            current = new;
        })
        .expect("spawn config-watch");
}
