//! Starting, stopping and live-reconfiguring the modules from `AppConfig`.

use std::path::{Path, PathBuf};
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
                let path = fancyzones_path();
                let started = load_fancyzones(path.as_deref()).and_then(|c| {
                    fancyzones::FancyZones::start_with(c, fancyzones::StartOptions { config_path: path.clone() })
                });
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

/// `fancyzones.json` next to the config file in use (the default location
/// when running with the default config).
pub fn fancyzones_path() -> Option<PathBuf> {
    crate::CONFIG_PATH
        .get()
        .map(|p| p.with_file_name("fancyzones.json"))
        .or_else(fancyzones::Config::default_path)
}

fn load_fancyzones(path: Option<&Path>) -> anyhow::Result<fancyzones::Config> {
    match path {
        Some(p) if p.exists() => fancyzones::Config::load_from(p),
        _ => Ok(fancyzones::Config::default()),
    }
}

/// What has to be pushed into a running FancyZones to match `new`.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FzChanges {
    pub custom: Vec<fancyzones::CustomLayout>,
    /// Monitor id -> layout (a removed entry maps to the new default layout).
    pub layouts: Vec<(String, fancyzones::AppliedLayout)>,
    pub settings: Option<fancyzones::Settings>,
}

impl FzChanges {
    pub fn is_empty(&self) -> bool {
        self.custom.is_empty() && self.layouts.is_empty() && self.settings.is_none()
    }
}

pub(crate) fn fz_changes(cur: &fancyzones::Config, new: &fancyzones::Config) -> FzChanges {
    let custom = new
        .custom_layouts
        .iter()
        .filter(|l| !cur.custom_layouts.contains(l))
        .cloned()
        .collect();
    let mut layouts: Vec<_> = new
        .applied_layouts
        .iter()
        .filter(|(id, l)| cur.applied_layouts.get(*id) != Some(*l))
        .map(|(id, l)| (id.clone(), l.clone()))
        .collect();
    // An entry removed in the file means "use the default layout" for that monitor.
    for id in cur.applied_layouts.keys().filter(|id| !new.applied_layouts.contains_key(*id)) {
        let d = new.settings.default_layout.clone();
        if cur.applied_layouts.get(id) != Some(&d) {
            layouts.push((id.clone(), d));
        }
    }
    let settings = (cur.settings != new.settings).then(|| new.settings.clone());
    FzChanges { custom, layouts, settings }
}

/// Push changes made to `fancyzones.json` (e.g. by the settings window) into
/// the running FancyZones. Its own writes produce no changes, so they are no-ops.
fn reload_fancyzones(running: &Running, path: &Path) {
    let Some(z) = &running.zones else { return };
    let new = match fancyzones::Config::load_from(path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("fancyzones.json not reloaded: {e:#}");
            return;
        }
    };
    let ch = fz_changes(&z.config(), &new);
    if ch.is_empty() {
        return;
    }
    log::info!("fancyzones.json changed, applying");
    for l in ch.custom {
        z.upsert_custom_layout(l);
    }
    for (id, l) in ch.layouts {
        if let Err(e) = z.apply_layout(&id, l) {
            log::warn!("fancyzones: {e:#}");
        }
    }
    if let Some(s) = ch.settings {
        z.update_settings(|cur| *cur = s);
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
/// settings window, or the tray writing it), and push `fancyzones.json`
/// changes into the running FancyZones.
pub fn watch(path: &Path, mut current: AppConfig) {
    let path = path.to_path_buf();
    let fz_path = fancyzones_path();
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut last: Option<SystemTime> = mtime(&path);
    let mut fz_last: Option<SystemTime> = fz_path.as_deref().and_then(mtime);
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Some(fz) = &fz_path {
                let now = mtime(fz);
                if now != fz_last {
                    fz_last = now;
                    if now.is_some() {
                        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(running) = guard.as_ref() {
                            reload_fancyzones(running, fz);
                        }
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use fancyzones::{AppliedLayout, Config, LayoutType};

    #[test]
    fn fancyzones_changes() {
        let mut cur = Config::default();
        cur.applied_layouts.insert("a".into(), AppliedLayout::template(LayoutType::Columns, 2));
        cur.applied_layouts.insert("b".into(), AppliedLayout::template(LayoutType::Rows, 2));
        assert!(fz_changes(&cur, &cur).is_empty());

        let mut new = cur.clone();
        new.applied_layouts.insert("a".into(), AppliedLayout::template(LayoutType::Grid, 4));
        new.applied_layouts.remove("b");
        new.app_zone_history.insert(
            "x".into(),
            fancyzones::config::AppZoneHistoryEntry {
                monitor: "a".into(),
                layout_uuid: "u".into(),
                layout_kind: LayoutType::Grid,
                zones: vec![0],
            },
        );
        let ch = fz_changes(&cur, &new);
        assert_eq!(ch.layouts.len(), 2);
        assert_eq!(ch.layouts[0], ("a".to_string(), AppliedLayout::template(LayoutType::Grid, 4)));
        assert_eq!(ch.layouts[1], ("b".to_string(), new.settings.default_layout.clone()));
        assert!(ch.settings.is_none() && ch.custom.is_empty());

        let mut new = cur.clone();
        new.settings.shift_drag = false;
        let ch = fz_changes(&cur, &new);
        assert!(ch.layouts.is_empty());
        assert!(!ch.settings.unwrap().shift_drag);
    }
}
