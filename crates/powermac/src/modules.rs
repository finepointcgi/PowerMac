//! Starting, stopping and live-reconfiguring the modules from `AppConfig`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use pm_config::AppConfig;
use crate::{clipboard, mwb_glue, Running, RUNNING};

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
        // Peers, layout, discovery and behavior changes (including the app's own
        // writes of discovered machines) go to the running engine; only socket,
        // key and identity changes restart it.
        let live = new.mwb.enabled
            && running.mwb.as_ref().is_some_and(|m| m.update_config(&new.mwb.settings));
        if live {
            log::info!("mouse without borders: settings applied");
        } else {
            if let Some(m) = running.mwb.take() {
                m.stop();
                log::info!("mouse without borders: stopped");
            }
            if new.mwb.enabled {
                running.mwb = start_mwb(&new.mwb.settings);
            }
            if running.mwb.is_none() {
                mwb_glue::write_stopped();
            }
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
        // Paste as plain text and the Caps Lock key work without the
        // remapping switch (`enabled`); the engine checks each part.
        let wanted = k.enabled || k.paste_plain_text.enabled || k.hyper_key.is_on();
        match (wanted, running.keyboard.as_ref()) {
            (true, Some(h)) => {
                if let Err(e) = h.update(k.clone()) {
                    log::error!("keyboard manager: {e:#}");
                }
            }
            (true, None) => match keyboard_manager::KeyboardManager::start(k.clone()) {
                Ok(h) => {
                    log::info!("keyboard manager: running{}{}",
                        if k.enabled { "" } else { " (remaps off)" },
                        if k.hyper_key.is_on() { ", Caps Lock remapped" } else { "" });
                    for w in k.validate() {
                        log::warn!("keyboard manager: {w}");
                    }
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
                    let opts = fancyzones::StartOptions {
                        config_path: path.clone(),
                        on_open_editor: Some(editor_launcher(path.clone())),
                    };
                    fancyzones::FancyZones::start_with(c, opts)
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

    if changed(old.map(|o| &o.mouse_tuning), &new.mouse_tuning) {
        apply_mouse_tuning(running, &new.mouse_tuning);
    }
    if changed(old.map(|o| &o.display_control), &new.display_control) {
        apply_display_control(running, &new.display_control);
    }
    if changed(old.map(|o| &o.clipboard_history), &new.clipboard_history) {
        apply_clipboard_history(running, &new.clipboard_history);
    }
    if changed(old.map(|o| &o.notch_shelf), &new.notch_shelf) {
        apply_notch_shelf(running, &new.notch_shelf);
    }
    if changed(old.map(|o| &o.color_picker), &new.color_picker) {
        apply_color_picker(running, &new.color_picker);
    }
    if changed(old.map(|o| &o.text_extractor), &new.text_extractor) {
        apply_text_extractor(running, &new.text_extractor);
    }
    if changed(old.map(|o| &o.window_switcher), &new.window_switcher) {
        apply_window_switcher(running, &new.window_switcher);
    }
    if changed(old.map(|o| &o.display_scaling), &new.display_scaling) {
        apply_display_scaling(running, &new.display_scaling, old.is_none());
    }
    if changed(old.map(|o| &o.audio), &new.audio) {
        apply_audio(running, &new.audio);
    }
    if changed(old.map(|o| &o.dock_previews), &new.dock_previews) {
        apply_dock_previews(running, &new.dock_previews);
    }
    if changed(old.map(|o| &o.text_expander), &new.text_expander) {
        apply_text_expander(running, &new.text_expander);
    }
    if changed(old.map(|o| &o.menu_bar), &new.menu_bar) {
        apply_menu_bar(running, &new.menu_bar);
    }
}

fn apply_menu_bar(running: &mut Running, cfg: &menubar_manager::Config) {
    match (cfg.enabled, running.menubar.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("menu bar: settings applied");
        }
        (true, None) => match menubar_manager::MenuBarManager::start(cfg.clone()) {
            Ok(h) => {
                log::info!("menu bar: running");
                if !menubar_manager::permissions().accessibility {
                    log::info!("menu bar: without Accessibility, items can't be moved from Settings (Cmd-drag them instead)");
                }
                running.menubar = Some(std::sync::Arc::new(h));
                crate::menubar_glue::start_once();
            }
            Err(e) => log::error!("menu bar: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.menubar.take() {
                h.stop();
            }
            crate::menubar_glue::write_stopped();
            log::info!("menu bar: stopped");
        }
        (false, None) => {}
    }
}

fn apply_display_scaling(running: &mut Running, cfg: &display_scaling::Config, startup: bool) {
    let state = data_dir().join(display_scaling::STATE_FILE);
    match (cfg.enabled, running.scaling.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("display scaling: settings applied");
        }
        (true, None) => match display_scaling::DisplayScaling::start_with_state_file(cfg.clone(), Some(state)) {
            Ok(h) => {
                let hidpi = cfg.displays.values().filter(|d| d.hidpi_enabled).count();
                log::info!("display scaling: running ({hidpi} display{} with HiDPI)", if hidpi == 1 { "" } else { "s" });
                running.scaling = Some(h);
                crate::scaling_glue::start_once();
            }
            Err(e) => log::error!("display scaling: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.scaling.take() {
                h.stop(); // removes virtual displays, restores modes
            }
            crate::scaling_glue::write_stopped();
            log::info!("display scaling: stopped");
        }
        (false, None) => {
            // A previous run crashed with HiDPI on or a mode unconfirmed:
            // starting undoes that (the state file is gone after a clean stop).
            if startup && state.exists() {
                log::info!("display scaling: undoing what a previous run left behind");
                match display_scaling::DisplayScaling::start_with_state_file(display_scaling::Config::default(), Some(state)) {
                    Ok(h) => h.stop(),
                    Err(e) => log::warn!("display scaling: {e:#}"),
                }
            }
        }
    }
}

fn apply_audio(running: &mut Running, cfg: &audio_control::Config) {
    match (cfg.enabled, running.audio.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("sound: settings applied");
        }
        (true, None) => match audio_control::AudioControl::start(cfg.clone()) {
            Ok(h) => {
                let rules = cfg.rules.outputs.len() + cfg.rules.inputs.len() + cfg.block_auto_switch_to.len();
                log::info!("sound: running ({rules} device rule{}{})", if rules == 1 { "" } else { "s" },
                    if cfg.per_app_volume { ", per-app volume" } else { "" });
                if cfg.per_app_volume && !audio_control::per_app_volume_supported() {
                    log::warn!("sound: per-app volume needs macOS 14.4 or later");
                }
                crate::audio_glue::attach(&h);
                running.audio = Some(h);
                crate::audio_glue::start_once();
            }
            Err(e) => log::error!("sound: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.audio.take() {
                h.stop(); // removes the per-app taps
            }
            crate::audio_glue::write_stopped();
            log::info!("sound: stopped");
        }
        (false, None) => {}
    }
}

fn apply_dock_previews(running: &mut Running, cfg: &dock_previews::Config) {
    match (cfg.enabled && dock_previews::SUPPORTED, running.dock.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("dock previews: settings applied");
        }
        (true, None) => match dock_previews::DockPreviews::start(cfg.clone()) {
            Ok(h) => {
                let p = dock_previews::permissions();
                log::info!("dock previews: running");
                if !p.accessibility {
                    log::warn!("dock previews: need Accessibility permission; they start working as soon as it's granted");
                }
                if !p.screen_recording {
                    log::info!("dock previews: no Screen Recording permission, showing app icons and titles instead of thumbnails");
                }
                running.dock = Some(h);
            }
            Err(e) => log::error!("dock previews: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.dock.take() {
                h.stop();
            }
            log::info!("dock previews: stopped");
        }
        (false, None) => {}
    }
}

fn apply_text_expander(running: &mut Running, cfg: &text_expander::Config) {
    match (cfg.enabled, running.expander.as_ref()) {
        (true, Some(h)) => match h.update(cfg.clone()) {
            Ok(()) => log::info!("text expander: settings applied"),
            Err(e) => log::error!("text expander: {e:#}"),
        },
        (true, None) => match text_expander::TextExpander::start(cfg.clone()) {
            Ok(h) => {
                let n = cfg.snippets.iter().filter(|s| s.enabled).count();
                log::info!("text expander: running ({n} snippet{})", if n == 1 { "" } else { "s" });
                running.expander = Some(h);
            }
            Err(e) => log::error!("text expander: {e:#} (it needs Accessibility permission)"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.expander.take() {
                h.stop();
            }
            log::info!("text expander: stopped");
        }
        (false, None) => {}
    }
}

/// The config file in use.
fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// The PowerMac data folder (the one holding `config.json`).
pub(crate) fn data_dir() -> PathBuf {
    config_path()
        .map(|p| pm_config::data_dir_for(&p))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn apply_mouse_tuning(running: &mut Running, cfg: &mouse_tuning::Config) {
    match (cfg.enabled, running.mouse_tuning.as_mut()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("mouse tuning: settings applied");
            warn_mouse_buttons(cfg);
        }
        (true, None) => match mouse_tuning::MouseTuning::start(cfg.clone()) {
            Ok(h) => {
                log::info!("mouse tuning: running (pointer settings{})",
                    if cfg.needs_button_hook() { ", button mappings" } else { "" });
                warn_mouse_buttons(cfg);
                running.mouse_tuning = Some(h);
            }
            Err(e) => log::error!("mouse tuning: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(mut h) = running.mouse_tuning.take() {
                h.stop(); // restores the system's pointer settings
            }
            log::info!("mouse tuning: stopped");
        }
        (false, None) => {}
    }
}

fn warn_mouse_buttons(cfg: &mouse_tuning::Config) {
    if cfg.needs_button_hook() && mouse_tuning::needs_permission() {
        log::warn!("mouse tuning: button mappings need Accessibility permission; grant it in \
                    System Settings → Privacy & Security (pointer settings work without it)");
    }
}

fn apply_display_control(running: &mut Running, cfg: &display_control::Config) {
    match (cfg.enabled, running.display.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("display control: settings applied");
        }
        (true, None) => match display_control::DisplayControl::start(cfg.clone()) {
            Ok(h) => {
                let n = h.displays().len();
                log::info!("display control: running ({n} display{})", if n == 1 { "" } else { "s" });
                let wants_keys = cfg.take_over_brightness_keys || cfg.take_over_volume_keys;
                if wants_keys && !h.keys_active() {
                    log::warn!("display control: brightness/volume keys need Accessibility permission; \
                                the sliders in Settings and the tray still work");
                }
                running.display = Some(h);
                crate::display_glue::start_once();
            }
            Err(e) => log::error!("display control: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.display.take() {
                h.stop(); // restores gamma tables
            }
            crate::display_glue::write_stopped();
            log::info!("display control: stopped");
        }
        (false, None) => {}
    }
}

fn apply_clipboard_history(running: &mut Running, cfg: &clipboard_history::Config) {
    match (cfg.enabled, running.clipboard.as_mut()) {
        (true, Some(h)) => match h.update(cfg.clone()) {
            Ok(()) => log::info!("clipboard history: settings applied{}", if cfg.paused { " (paused)" } else { "" }),
            Err(e) => log::error!("clipboard history: {e:#}"),
        },
        (true, None) => {
            let dir = match config_path() {
                Some(p) => clipboard_history::data_dir_for_config(&p),
                None => data_dir().join("clipboard"),
            };
            match clipboard_history::ClipboardHistory::start(cfg.clone(), dir) {
                Ok(h) => {
                    h.on_open_picker(|| crate::children::open(&crate::children::CLIPBOARD, &["clipboard"]));
                    if cfg.hotkey().is_some() && !h.hotkey_active() {
                        log::warn!("clipboard history: hotkey {} isn't active (it needs Accessibility permission); \
                                    the picker still opens from the tray", cfg.hotkey);
                    }
                    log::info!("clipboard history: running{}", if cfg.paused { " (paused)" } else { "" });
                    running.clipboard = Some(h);
                }
                Err(e) => log::error!("clipboard history: {e:#}"),
            }
        }
        (false, Some(_)) => {
            if let Some(h) = running.clipboard.take() {
                h.stop();
            }
            log::info!("clipboard history: stopped");
        }
        (false, None) => {}
    }
}

fn apply_notch_shelf(running: &mut Running, cfg: &notch_shelf::Config) {
    match (cfg.enabled, running.shelf.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("notch shelf: settings applied");
        }
        (true, None) => match notch_shelf::NotchShelf::start_with(cfg.clone(), data_dir(), crate::quick::notch_controls()) {
            Ok(h) => {
                log::info!("notch shelf: running ({})", if notch_shelf::has_notch() { "notch" } else { "no notch on this Mac" });
                running.shelf = Some(h);
            }
            Err(e) => log::error!("notch shelf: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.shelf.take() {
                h.stop();
            }
            log::info!("notch shelf: stopped");
        }
        (false, None) => {}
    }
}

fn apply_color_picker(running: &mut Running, cfg: &pm_config::Module<color_picker::Config>) {
    match (cfg.enabled, running.color.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.settings.clone());
            log::info!("color picker: settings applied");
        }
        (true, None) => {
            let started = color_picker::ColorPicker::start(cfg.settings.clone(), data_dir(), || {
                crate::children::open(&crate::children::COLOR_EDITOR, &["color-picker", "--editor"])
            });
            match started {
                Ok(h) => running.color = Some(h),
                Err(e) => log::error!("color picker: {e:#}"),
            }
        }
        (false, Some(_)) => {
            running.color = None; // unregisters the hotkey
            log::info!("color picker: stopped");
        }
        (false, None) => {}
    }
}

fn apply_text_extractor(running: &mut Running, cfg: &pm_config::Module<text_extractor::Config>) {
    match (cfg.enabled, running.ocr.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.settings.clone());
            log::info!("text extractor: settings applied");
        }
        (true, None) => match text_extractor::TextExtractor::start(cfg.settings.clone()) {
            Ok(h) => {
                if text_extractor::screen_recording_permission() != text_extractor::Permission::Granted {
                    log::warn!("text extractor: capturing the screen needs Screen Recording permission; \
                                grant it in System Settings → Privacy & Security → Screen Recording");
                }
                running.ocr = Some(h);
            }
            Err(e) => log::error!("text extractor: {e:#}"),
        },
        (false, Some(_)) => {
            running.ocr = None; // unregisters the hotkey
            log::info!("text extractor: stopped");
        }
        (false, None) => {}
    }
}

fn apply_window_switcher(running: &mut Running, cfg: &window_switcher::Config) {
    match (cfg.enabled, running.switcher.as_ref()) {
        (true, Some(h)) => {
            h.update(cfg.clone());
            log::info!("window switcher: settings applied");
        }
        (true, None) => match window_switcher::WindowSwitcher::start(cfg.clone()) {
            Ok(h) => {
                let p = window_switcher::permissions();
                log::info!("window switcher: running ({}{})", cfg.trigger,
                    if cfg.replace_cmd_tab { ", replaces the system switcher" } else { "" });
                if !p.accessibility {
                    log::warn!("window switcher: needs Accessibility permission for its shortcut; \
                                it starts working as soon as it's granted");
                }
                if cfg.thumbnails && !p.screen_recording {
                    log::info!("window switcher: no Screen Recording permission, showing app icons instead of thumbnails");
                }
                running.switcher = Some(h);
            }
            Err(e) => log::error!("window switcher: {e:#}"),
        },
        (false, Some(_)) => {
            if let Some(h) = running.switcher.take() {
                h.stop();
            }
            log::info!("window switcher: stopped");
        }
        (false, None) => {}
    }
}

/// FancyZones editor hotkey: run `<this exe> --config <config> editor
/// --monitor <id>` (the `editor` subcommand edits `fancyzones.json`).
fn editor_launcher(fz_path: Option<PathBuf>) -> fancyzones::EditorCallback {
    Box::new(move |monitor: Option<String>| {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                log::error!("fancyzones editor: {e}");
                return;
            }
        };
        let config = crate::CONFIG_PATH
            .get()
            .cloned()
            .or_else(pm_config::AppConfig::default_path)
            .or_else(|| fz_path.as_ref().map(|p| p.with_file_name("config.json")));
        let mut cmd = std::process::Command::new(exe);
        if let Some(c) = &config {
            cmd.arg("--config").arg(c);
        }
        cmd.arg("editor");
        if let Some(m) = &monitor {
            cmd.arg("--monitor").arg(m);
        }
        match cmd.spawn() {
            Ok(child) => {
                log::info!("fancyzones: editor started (pid {})", child.id());
                // reap it when it exits
                std::thread::spawn(move || {
                    let mut child = child;
                    let _ = child.wait();
                });
            }
            Err(e) => log::error!("fancyzones: starting the editor failed: {e}"),
        }
    })
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
    pub layout_hotkeys: Option<Vec<fancyzones::LayoutHotkey>>,
    pub layout_templates: Option<Vec<fancyzones::AppliedLayout>>,
}

impl FzChanges {
    pub fn is_empty(&self) -> bool {
        self.custom.is_empty()
            && self.layouts.is_empty()
            && self.settings.is_none()
            && self.layout_hotkeys.is_none()
            && self.layout_templates.is_none()
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
    let layout_hotkeys = (cur.layout_hotkeys != new.layout_hotkeys).then(|| new.layout_hotkeys.clone());
    let layout_templates = (cur.layout_templates != new.layout_templates).then(|| new.layout_templates.clone());
    FzChanges { custom, layouts, settings, layout_hotkeys, layout_templates }
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
    if let Some(h) = ch.layout_hotkeys {
        z.set_layout_hotkeys(h);
    }
    if let Some(t) = ch.layout_templates {
        z.set_layout_templates(t);
    }
}

/// Handle a flash request file written by the settings window: briefly show
/// the zones with the current colors (requests older than 5 s are ignored).
fn flash_zones(path: &Path) {
    let req: fancyzones::FlashRequest =
        match std::fs::read_to_string(path).map(|s| serde_json::from_str(&s)) {
            Ok(Ok(r)) => r,
            _ => return,
        };
    if !req.is_fresh(5_000) {
        return;
    }
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(z) = guard.as_ref().and_then(|r| r.zones.as_ref()) {
        log::info!("fancyzones: showing the zones (settings preview)");
        z.flash_zones(req.monitor.as_deref());
    }
}

/// Handle a "Search now" request file written by the settings window
/// (requests older than 10 s are ignored).
fn request_discovery(path: &Path) {
    let req: pm_config::DiscoverRequest = match std::fs::read_to_string(path).map(|s| serde_json::from_str(&s)) {
        Ok(Ok(r)) => r,
        _ => return,
    };
    if !req.is_fresh(10_000) {
        return;
    }
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = guard.as_ref().and_then(|r| r.mwb.as_ref()) {
        log::info!("mouse without borders: searching the network (settings)");
        m.discover_now();
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
            mwb_glue::start(&h);
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
    // "Show on screen" requests from the settings window.
    let flash_path = fz_path
        .as_ref()
        .map(|p| p.with_file_name(fancyzones::FLASH_REQUEST_FILE));
    let mut flash_last: Option<SystemTime> = flash_path.as_deref().and_then(mtime);
    // "Search now" requests from the settings window.
    let discover_path = path.with_file_name(pm_config::MWB_DISCOVER_REQUEST_FILE);
    let mut discover_last: Option<SystemTime> = mtime(&discover_path);
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
            if let Some(fp) = &flash_path {
                let now = mtime(fp);
                if now != flash_last {
                    flash_last = now;
                    if now.is_some() {
                        flash_zones(fp);
                    }
                }
            }
            let now = mtime(&discover_path);
            if now != discover_last {
                discover_last = now;
                if now.is_some() {
                    request_discovery(&discover_path);
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

        let mut new = cur.clone();
        new.layout_hotkeys.push(fancyzones::LayoutHotkey { key: 1, layout_uuid: "{X}".into() });
        let ch = fz_changes(&cur, &new);
        assert_eq!(ch.layout_hotkeys.unwrap().len(), 1);
        assert!(ch.layout_templates.is_none() && ch.settings.is_none());
    }
}
