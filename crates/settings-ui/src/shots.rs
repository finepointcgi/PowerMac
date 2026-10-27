//! Offscreen screenshots of every settings page and editor screen, rendered
//! with egui_kittest and wgpu. Run with
//! `PM_UI_SHOTS=<dir> cargo test -p settings-ui shots` to write PNGs into
//! `<dir>` (`PM_UI_SHOTS_ALWAYS=1` renders without saving). It is skipped
//! by default and when no GPU adapter is available.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::Harness;
use fancyzones::{AppliedLayout, LayoutType};

use crate::app::SettingsApp;
use crate::editor::EditorApp;
use crate::model::*;

fn out_dir() -> Option<PathBuf> {
    std::env::var_os("PM_UI_SHOTS").map(PathBuf::from)
}

/// kittest's margin around the app (see `save`).
const MARGIN: f32 = 8.0;

/// Harness size for a window of `w` x `h`.
fn win(w: f32, h: f32) -> [f32; 2] {
    [w + 2.0 * MARGIN, h + 2.0 * MARGIN]
}

fn save(h: &mut Harness<'_, impl Sized>, dir: &Option<PathBuf>, name: &str) {
    h.run_steps(4);
    match h.render() {
        Ok(img) => {
            if let Some(d) = dir {
                std::fs::create_dir_all(d).ok();
                // kittest puts an 8 pt margin around the app; the real window doesn't.
                let (w, h) = img.dimensions();
                let img = image::imageops::crop_imm(
                    &img,
                    MARGIN as u32,
                    MARGIN as u32,
                    w - 2 * MARGIN as u32,
                    h - 2 * MARGIN as u32,
                )
                .to_image();
                img.save(d.join(format!("{name}.png"))).expect("save png");
            }
        }
        Err(e) => eprintln!("render {name} skipped: {e}"),
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pm-ui-shots-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sample_fz(path: &Path) {
    let mut c = fancyzones::Config::default();
    c.applied_layouts
        .insert("gone".into(), AppliedLayout::template(LayoutType::Grid, 4));
    c.settings.default_layout = AppliedLayout::template(LayoutType::PriorityGrid, 3);
    c.settings.window_switching = true;
    c.settings.excluded_apps = vec!["com.apple.Safari".into(), "notepad.exe".into()];
    c.import_fancyzones_custom_layouts(
        r#"{"custom-layouts": [
          {"uuid": "{AAAAAAAA-0000-0000-0000-000000000001}", "name": "Coding", "type": "grid",
           "info": {"rows": 2, "columns": 2, "rows-percentage": [5000, 5000], "columns-percentage": [6000, 4000],
                    "cell-child-map": [[0, 1], [0, 2]], "show-spacing": true, "spacing": 8, "sensitivity-radius": 20}},
          {"uuid": "{AAAAAAAA-0000-0000-0000-000000000002}", "name": "Floating", "type": "canvas",
           "info": {"ref-width": 1920, "ref-height": 1080, "zones": [{"X": 0, "Y": 0, "width": 960, "height": 1080},
                    {"X": 700, "Y": 150, "width": 900, "height": 600}], "sensitivity-radius": 20}}]}"#,
    )
    .unwrap();
    c.layout_hotkeys.push(fancyzones::LayoutHotkey {
        key: 1,
        layout_uuid: "{AAAAAAAA-0000-0000-0000-000000000001}".into(),
    });
    c.save_to(path).unwrap();
}

fn monitors(w: i32, h: i32) -> Vec<fancyzones::Monitor> {
    vec![
        fancyzones::Monitor {
            id: "mon-a".into(),
            name: "Built-in Retina Display".into(),
            frame: fancyzones::Rect::new(0, 0, w, h),
            work_area: fancyzones::Rect::new(0, 33, w, h),
            scale: 2.0,
            primary: true,
        },
        fancyzones::Monitor {
            id: "mon-b".into(),
            name: "LG UltraFine".into(),
            frame: fancyzones::Rect::new(w, 0, w + 2560, 1440),
            work_area: fancyzones::Rect::new(w, 0, w + 2560, 1440),
            scale: 1.0,
            primary: false,
        },
    ]
}

/// A Home layout with three pages and most widget kinds (stable ids).
fn sample_widgets(n: &mut notch_shelf::Config) {
    use notch_shelf::{WidgetInstance, WidgetKind as K, WidgetPage, WidgetSize as S};
    use serde_json::json;
    n.enabled = true;
    n.pages = vec![
        WidgetPage { name: "Home".into(), symbol: "house.fill".into() },
        WidgetPage { name: "Work".into(), symbol: "briefcase.fill".into() },
        WidgetPage { name: "Dev".into(), symbol: String::new() },
    ];
    let mk = |id: &str, kind: K, size: S, page: usize, settings: serde_json::Value| {
        let mut w = WidgetInstance::new(kind).on_page(page).sized(size);
        w.id = id.into();
        if let (Some(o), serde_json::Value::Object(extra)) = (w.settings.as_object_mut(), settings) {
            o.extend(extra);
        }
        w
    };
    n.widgets = vec![
        mk("np", K::NowPlaying, S::Medium, 0, json!({})),
        mk("clocks", K::WorldClock, S::Medium, 0, json!({"clocks": [
            {"zone": "America/Los_Angeles", "label": "Cupertino"},
            {"zone": "Europe/Berlin", "label": "Berlin"},
            {"zone": "Asia/Tokyo", "label": ""}], "style": "analog"})),
        mk("weather", K::Weather, S::Small, 0, json!({"location": "Portland, Oregon"})),
        mk("battery", K::Battery, S::Small, 0, json!({})),
        mk("launcher", K::Launcher, S::Medium, 0, json!({"title": "Favorites", "items": [
            {"kind": "app", "target": "Safari", "label": "", "symbol": ""},
            {"kind": "folder", "target": "~/Documents", "label": "Docs", "symbol": ""},
            {"kind": "url", "target": "https://github.com", "label": "GitHub", "symbol": "link"},
            {"kind": "command", "target": "open -a Terminal ~/Code", "label": "Code", "symbol": "terminal"}]})),
        mk("cal", K::Calendar, S::Medium, 1, json!({"days": 3})),
        mk("rem", K::Reminders, S::Medium, 1, json!({"lists": ["Work"]})),
        mk("shortcuts", K::Shortcuts, S::Medium, 1, json!({"shortcuts": ["Start Focus Session", "Log Water"], "title": "Routines"})),
        mk("timer", K::Timer, S::Medium, 1, json!({"presets": [5, 25, 50]})),
        mk("disk", K::Script, S::Medium, 2, json!({"title": "Disk", "command": "df -h / | awk 'NR==2 {print $5}'", "symbol": "internaldrive", "interval": 300})),
        mk("btc", K::Web, S::Medium, 2, json!({"title": "Bitcoin", "url": "https://api.coinbase.com/v2/prices/BTC-USD/spot",
            "template": "{{data.amount | round:0}} {{data.currency}}", "subtitle": "Coinbase spot price", "symbol": "bitcoinsign.circle", "token": "sk-live-1234", "interval": 600})),
        mk("xbar", K::Xbar, S::Large, 2, json!({"title": "Plugins"})),
        mk("stats", K::SystemStats, S::Medium, 2, json!({})),
    ];
    n.widgets.last_mut().unwrap().enabled = false;
}

fn has_gpu() -> bool {
    let mut h = Harness::builder()
        .with_size([64.0, 64.0])
        .wgpu()
        .build_ui(|ui| {
            ui.label("x");
        });
    h.run();
    match h.render() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("no offscreen renderer: {e}");
            false
        }
    }
}

#[test]
fn shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none()
    {
        return; // slow (GPU); opt-in
    }
    if !has_gpu() {
        return;
    }
    let dir = out_dir();
    let d = scratch("settings");
    let path = d.join("config.json");
    let mut cfg = pm_config::AppConfig::with_new_key();
    cfg.mwb.enabled = true;
    cfg.fancyzones.enabled = true;
    cfg.keyboard_manager = keyboard_manager::Config::from_json(
        r#"{"enabled": true, "remapKeys":[{"from":"CapsLock","to":"Esc"}],
            "remapShortcuts":[{"from":"Ctrl+Alt+Y","to":{"run":{"path":"/bin/echo"}}}]}"#,
    )
    .unwrap();
    cfg.mwb
        .settings
        .peers
        .insert("studio-pc".into(), "192.168.1.20".into());
    cfg.mwb.settings.machine_name = "MacBook-Pro".into();
    cfg.mwb.settings.matrix = vec![
        "MacBook-Pro".into(),
        "studio-pc".into(),
        "living-room".into(),
    ];
    cfg.clipboard_history.enabled = true;
    cfg.keyboard_manager.windows_style.enabled = true;
    cfg.mouse_tuning.mouse.disable_acceleration = true;
    cfg.mouse_tuning.mouse.speed = Some(0.6);
    cfg.mouse_tuning.buttons.mappings.push(mouse_tuning::ButtonMapping {
        button: mouse_tuning::button::MIDDLE,
        action: mouse_tuning::ButtonAction::MissionControl,
    });
    cfg.mouse_tuning.buttons.mappings.push(mouse_tuning::ButtonMapping {
        button: mouse_tuning::button::FORWARD,
        action: mouse_tuning::ButtonAction::Keystroke { keys: "Cmd+Shift+T".into() },
    });
    cfg.scroll_reverser.smooth_scrolling = true;
    cfg.color_picker.settings.custom_formats.push(color_picker::CustomFormat {
        name: "CSS var".into(),
        format: "--brand: #%Rex%Grx%Blx;".into(),
    });
    cfg.color_picker.settings.visible_formats.push("CSS var".into());
    cfg.text_extractor.settings.languages = vec!["en-US".into(), "de-DE".into()];
    cfg.display_control.hotkeys.brightness_up = display_control::Hotkey::new(false, true, true, false, 0x26);
    cfg.display_control.hotkeys.brightness_down = display_control::Hotkey::new(false, true, true, false, 0x28);
    // Newer modules
    cfg.keyboard_manager.hyper_key.mode = keyboard_manager::HyperMode::Hyper;
    cfg.display_scaling.enabled = true;
    cfg.display_scaling.displays.insert(
        "LG HDR 4K#8R2N".into(),
        display_scaling::DisplayConfig {
            hidpi_enabled: true,
            target_logical_resolution: Some(display_scaling::Resolution { width: 1920, height: 1080 }),
            custom_resolutions: vec![display_scaling::Resolution { width: 2200, height: 1238 }],
            preferred_mode: None,
        },
    );
    cfg.audio.rules.outputs = vec![
        audio_control::DeviceMatch::uid("studio-uid", "Studio Display Speakers"),
        audio_control::DeviceMatch::name("MacBook Pro Speakers"),
    ];
    cfg.audio.rules.inputs = vec![audio_control::DeviceMatch::name("Shure MV7")];
    cfg.audio.block_auto_switch_to = vec![audio_control::DeviceMatch::name("AirPods")];
    cfg.audio.keep_builtin_mic_with_bluetooth = true;
    cfg.audio.per_app_volume = true;
    cfg.audio.app_volumes.insert("com.spotify.client".into(), audio_control::AppLevel { volume: 0.6, muted: false });
    cfg.audio.app_volumes.insert("us.zoom.xos".into(), audio_control::AppLevel { volume: 1.0, muted: true });
    cfg.text_expander.enabled = true;
    cfg.text_expander.snippets.push(text_expander::Snippet {
        word_boundary: true,
        case_sensitive: false,
        ..text_expander::Snippet::new("btw", "by the way")
    });
    cfg.text_expander.snippets.push(text_expander::Snippet {
        apps: vec!["com.apple.mail".into()],
        label: "Signature".into(),
        ..text_expander::Snippet::new(";sig", "Best regards,\nAda Lovelace")
    });
    sample_widgets(&mut cfg.notch_shelf);
    cfg.menu_bar.enabled = true;
    cfg.save(&path).unwrap();
    sample_fz(&fancyzones_path(&path));
    // What a running app reports about the displays (re-stamped before
    // each render so it stays fresh).
    let display_status = pm_config::DisplayStatus {
        running: true,
        keys_active: true,
        displays: vec![
            display_control::DisplayInfo {
                id: "Built-in Retina Display#0".into(),
                name: "Built-in Retina Display".into(),
                is_builtin: true,
                enabled: true,
                bounds: display_control::Rect { x: 0, y: 0, width: 1512, height: 982 },
                supports_ddc: false,
                brightness_method: display_control::BrightnessMethod::Native,
                brightness: Some(72),
                contrast: None,
                volume: None,
                muted: None,
                input_source: None,
                max_brightness: None,
                max_contrast: None,
                max_volume: None,
            },
            display_control::DisplayInfo {
                id: "LG HDR 4K#8R2N".into(),
                name: "LG HDR 4K".into(),
                is_builtin: false,
                enabled: true,
                bounds: display_control::Rect { x: 1512, y: 0, width: 2560, height: 1440 },
                supports_ddc: true,
                brightness_method: display_control::BrightnessMethod::Ddc,
                brightness: Some(45),
                contrast: Some(70),
                volume: Some(30),
                muted: Some(false),
                input_source: Some(0x0F),
                max_brightness: Some(100),
                max_contrast: Some(100),
                max_volume: Some(100),
            },
        ],
        ..Default::default()
    }
    ;
    let mode = |w: u32, h: u32, hidpi: bool| display_scaling::Mode {
        width: w,
        height: h,
        pixel_width: if hidpi { w * 2 } else { w },
        pixel_height: if hidpi { h * 2 } else { h },
        refresh: 60.0,
        hidpi,
    };
    let scaling = pm_config::bridge::ScalingStatus {
        running: true,
        hidpi_supported: true,
        displays: vec![display_scaling::DisplayModes {
            id: "LG HDR 4K#8R2N".into(),
            name: "LG HDR 4K".into(),
            is_builtin: false,
            vendor: 0x1E6D,
            model: 0x5B10,
            current: Some(mode(1920, 1080, true)),
            modes: vec![
                mode(2560, 1440, true),
                mode(2200, 1238, true),
                mode(1920, 1080, true),
                mode(1280, 720, true),
                mode(2560, 1440, false),
                mode(1920, 1080, false),
            ],
            physical_current: Some(mode(2560, 1440, false)),
            native: Some(display_scaling::Resolution { width: 2560, height: 1440 }),
            hidpi_capable: true,
            hidpi_active: true,
            native_hidpi: false,
            suggested_hidpi: vec![
                display_scaling::Resolution { width: 2560, height: 1440 },
                display_scaling::Resolution { width: 2048, height: 1152 },
                display_scaling::Resolution { width: 1920, height: 1080 },
                display_scaling::Resolution { width: 1600, height: 900 },
            ],
            dpi: None,
            scale_percent: Some(200),
        }],
        pending: vec![display_scaling::PendingChange {
            token: display_scaling::Token(1),
            display_id: "LG HDR 4K#8R2N".into(),
            previous: mode(2560, 1440, false),
            applied: mode(1920, 1080, true),
            seconds_left: 12,
        }],
        error: None,
    };
    let dev = |uid: &str, name: &str, t: audio_control::Transport, def: bool, vol: Option<f32>| audio_control::AudioDevice {
        uid: uid.into(),
        name: name.into(),
        transport: t,
        is_default: def,
        is_default_system: def,
        channels: 2,
        volume: vol,
        muted: Some(false),
    };
    use audio_control::Transport as T;
    let audio = pm_config::bridge::AudioStatus {
        running: true,
        outputs: vec![
            dev("studio-uid", "Studio Display Speakers", T::Usb, true, Some(0.55)),
            dev("builtin-out", "MacBook Pro Speakers", T::BuiltIn, false, Some(0.4)),
            dev("airpods", "Ada's AirPods Pro", T::Bluetooth, false, Some(0.5)),
        ],
        inputs: vec![
            dev("mv7", "Shure MV7", T::Usb, false, Some(0.8)),
            dev("builtin-in", "MacBook Pro Microphone", T::BuiltIn, true, Some(0.7)),
        ],
        apps: vec![
            audio_control::AudioApp { pid: 10, pids: vec![10], bundle_id: "com.spotify.client".into(), name: "Spotify".into(), volume: 0.6, muted: false, playing: true },
            audio_control::AudioApp { pid: 11, pids: vec![11], bundle_id: "com.google.Chrome".into(), name: "Google Chrome".into(), volume: 1.0, muted: false, playing: true },
        ],
        per_app_supported: true,
        capture_permission: audio_control::CapturePermission::Granted,
        error: None,
        mixer: None,
    };
    let item = |id: &str, owner: &str, title: Option<&str>, sec: menubar_manager::Section, system: bool| menubar_manager::MenuBarItem {
        id: id.into(),
        owner: owner.into(),
        bundle_id: Some(format!("com.example.{}", owner.to_lowercase().replace(' ', ""))),
        pid: 1,
        title: title.map(Into::into),
        frame: Default::default(),
        section: sec,
        window_id: None,
        own: false,
        system,
    };
    use menubar_manager::Section as Sec;
    let menubar = pm_config::bridge::MenuBarStatus {
        running: true,
        accessibility: true,
        screen_recording: false,
        listed: true,
        items: vec![
            item("dropbox", "Dropbox", None, Sec::AlwaysHidden, false),
            item("docker", "Docker Desktop", None, Sec::Hidden, false),
            item("1password", "1Password", None, Sec::Hidden, false),
            item("slack", "Slack", None, Sec::Visible, false),
            item("wifi", "Control Center", Some("Wi‑Fi"), Sec::Visible, true),
            item("battery", "Control Center", Some("Battery"), Sec::Visible, true),
        ],
        hidden_by_notch: vec![item("slack", "Slack", None, Sec::Visible, false)],
        fitted_mode: true,
        layout_issue: None,
        error: None,
    };
    let fresh_status = || {
        display_status.clone().stamp().save_for(&path).unwrap();
        pm_config::bridge::write(&path, pm_config::bridge::SCALING_STATUS_FILE, &scaling).unwrap();
        pm_config::bridge::write(&path, pm_config::bridge::AUDIO_STATUS_FILE, &audio).unwrap();
        pm_config::bridge::write(&path, pm_config::bridge::MENUBAR_STATUS_FILE, &menubar).unwrap();
    };
    fresh_status();
    // What a running app reports: one machine found automatically and
    // connected, one found but reconnecting, one with another key.
    pm_config::MwbStatus {
        running: true,
        machine_name: "MacBook-Pro".into(),
        auto_discover: true,
        machines: vec![
            pm_config::MwbMachine {
                name: "living-room".into(),
                address: "192.168.1.42".into(),
                connected: false,
                discovered: true,
            },
            pm_config::MwbMachine {
                name: "studio-pc".into(),
                address: "192.168.1.20".into(),
                connected: true,
                discovered: true,
            },
        ],
        other_key: vec!["192.168.1.57".into()],
        ..Default::default()
    }
    .stamp()
    .save_for(&path)
    .unwrap();

    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        for (size, suffix) in [(win(1100.0, 780.0), ""), (win(1100.0, 3400.0), "-full")] {
            let mut app = SettingsApp::new(path.clone());
            app.set_devices_for_test(vec![
                mouse_tuning::DeviceInfo {
                    id: 1,
                    name: "MX Master 3S".into(),
                    vendor_id: 0x046D,
                    product_id: 0xB034,
                    kind: mouse_tuning::DeviceKind::Mouse,
                    transport: "Bluetooth Low Energy".into(),
                },
                mouse_tuning::DeviceInfo {
                    id: 2,
                    name: "Apple Internal Keyboard / Trackpad".into(),
                    vendor_id: 0,
                    product_id: 0,
                    kind: mouse_tuning::DeviceKind::Trackpad,
                    transport: "FIFO".into(),
                },
            ]);
            let mut h = Harness::builder()
                .with_size(size)
                .with_theme(theme)
                .with_accessibility_check(false)
                .wgpu()
                .build_ui_state(|ui, app: &mut SettingsApp| app.draw(ui), app);
            // Like the real window: characters the bundled fonts lack (the
            // default ¯\_(ツ)_/¯ snippet) are drawn as boxes, not a panic.
            h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
            // Shots don't depend on whether AltTab runs on this machine.
            h.state_mut().set_alt_tab_for_test(false, vec![]);
            for p in Page::ALL {
                if !suffix.is_empty() && matches!(p, Page::Awake) {
                    continue;
                }
                fresh_status();
                h.state_mut().set_page(p);
                h.state_mut().reload_status_for_test();
                let slug = format!("{p:?}").to_lowercase();
                save(&mut h, &dir, &format!("settings-{slug}-{tn}{suffix}"));
            }
            // Window Switcher with Cmd+Tab replaced, while AltTab (set to
            // Cmd+Tab) is running.
            let before = h.state_mut().window_switcher_for_test().clone();
            {
                let ws = h.state_mut().window_switcher_for_test();
                ws.replace_cmd_tab = true;
                ws.replace_cmd_backtick = true;
            }
            h.state_mut().set_alt_tab_for_test(true, vec![window_switcher::Hotkey::system_switcher()]);
            h.state_mut().set_page(Page::WindowSwitcher);
            save(&mut h, &dir, &format!("settings-windowswitcher-cmdtab-{tn}{suffix}"));
            h.state_mut().set_alt_tab_for_test(false, vec![]);
            *h.state_mut().window_switcher_for_test() = before;
            if suffix.is_empty() {
                h.state_mut().set_search_for_test("color");
                save(&mut h, &dir, &format!("settings-search-{tn}"));
                h.state_mut().set_search_for_test("");
            }
        }
    }

    // Layout editor at several window sizes.
    let ed = scratch("editor");
    let fz = ed.join("fancyzones.json");
    sample_fz(&fz);
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        for size in [win(1000.0, 630.0), win(1100.0, 780.0), win(1400.0, 900.0)] {
            let app =
                EditorApp::with_monitors(fz.clone(), Some("mon-a".into()), monitors(1512, 982));
            let mut h = Harness::builder()
                .with_size(size)
                .with_theme(theme)
                .with_accessibility_check(false)
                .wgpu()
                .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app);
            let sz = format!(
                "{}x{}",
                (size[0] - 2.0 * MARGIN) as i32,
                (size[1] - 2.0 * MARGIN) as i32
            );
            save(&mut h, &dir, &format!("editor-gallery-{sz}-{tn}"));
            if size[0] == 1100.0 + 2.0 * MARGIN {
                h.state_mut().test_open_template_dialog(4);
                save(&mut h, &dir, &format!("editor-template-dialog-{tn}"));
                h.state_mut()
                    .test_open_custom_dialog("{AAAAAAAA-0000-0000-0000-000000000001}");
                save(&mut h, &dir, &format!("editor-custom-dialog-{tn}"));
                h.state_mut().test_open_new_dialog();
                save(&mut h, &dir, &format!("editor-new-dialog-{tn}"));
                h.state_mut().test_close_dialog();
                h.state_mut()
                    .test_open_editor("{AAAAAAAA-0000-0000-0000-000000000001}");
                save(&mut h, &dir, &format!("editor-grid-{tn}"));
                h.state_mut().session = None;
                h.state_mut()
                    .test_open_editor("{AAAAAAAA-0000-0000-0000-000000000002}");
                save(&mut h, &dir, &format!("editor-canvas-{tn}"));
                h.state_mut().session = None;
            }
        }
    }
    // Nothing was saved by looking.
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&ed);
}

/// The Notch Widgets page: pages manager, gallery and the editors of the
/// "build your own" widgets, light and dark.
#[test]
fn widget_shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none() {
        return;
    }
    if !has_gpu() {
        return;
    }
    use notch_shelf::widgets::content::Item;
    use notch_shelf::WidgetContent;
    let dir = out_dir();
    let d = scratch("widgets");
    let path = d.join("config.json");
    let plugins = d.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    for f in ["cpu.5s.sh", "github-prs.10m.py", "weather.1h.sh"] {
        std::fs::write(plugins.join(f), "#!/bin/sh\necho hi\n").unwrap();
    }
    let mut cfg = pm_config::AppConfig::with_new_key();
    sample_widgets(&mut cfg.notch_shelf);
    if let Some(w) = cfg.notch_shelf.widget_mut("xbar") {
        w.settings["path"] = serde_json::json!(plugins.to_string_lossy());
    }
    cfg.save(&path).unwrap();
    let status = pm_config::bridge::NotchStatus {
        running: true,
        reminders: "granted".into(),
        reminder_lists: vec!["Work".into(), "Groceries".into(), "Someday".into()],
        ..Default::default()
    };
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let mut app = SettingsApp::new(path.clone());
        app.set_page(Page::NotchWidgets);
        app.nw_shortcuts_for_test(vec!["Log Water".into(), "Start Focus Session".into(), "Morning Routine".into(), "Resize Images".into()]);
        app.nw_apps_for_test(vec!["Calendar".into(), "Mail".into(), "Safari".into(), "Terminal".into(), "Xcode".into()]);
        app.nw_test_result_for_test(
            "disk",
            Ok(WidgetContent { title: "Disk".into(), value: Some("73".into()), number: Some(73.0), unit: "%".into(), progress: Some(0.73), subtitle: "142 GB free".into(), ..Default::default() }),
        );
        app.nw_test_result_for_test("btc", Err("HTTP 401: Unauthorized (check the API token)".into()));
        app.nw_test_result_for_test(
            "xbar",
            Ok(WidgetContent {
                title: "CPU 12%".into(),
                items: ["cpu.5s.sh — CPU 12%", "github-prs.10m.py — 3 open PRs", "weather.1h.sh — 21° Sunny"].iter().map(|t| Item::text(t)).collect(),
                ..Default::default()
            }),
        );
        let mut h = Harness::builder()
            .with_size(win(1100.0, 780.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut SettingsApp| app.draw(ui), app);
        h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
        let fresh = || pm_config::bridge::write(&path, pm_config::bridge::NOTCH_STATUS_FILE, &status).unwrap();
        fresh();
        h.state_mut().reload_status_for_test();
        // The top of the page: pages manager and the widgets of "Home".
        save(&mut h, &dir, &format!("notch-widgets-pages-{tn}"));
        // The whole page, tall.
        h.set_size(egui::vec2(1100.0 + 2.0 * MARGIN, 2300.0 + 2.0 * MARGIN));
        save(&mut h, &dir, &format!("notch-widgets-full-{tn}"));
        // The gallery.
        h.state_mut().nw_gallery_for_test(true);
        h.set_size(egui::vec2(1100.0 + 2.0 * MARGIN, 3000.0 + 2.0 * MARGIN));
        save(&mut h, &dir, &format!("notch-widgets-gallery-{tn}"));
        h.state_mut().nw_gallery_for_test(false);
        // Editors.
        for (page, id, slug) in [
            (2, "disk", "script"),
            (2, "btc", "web"),
            (2, "xbar", "xbar"),
            (0, "launcher", "launcher"),
            (0, "clocks", "worldclock"),
            (1, "shortcuts", "shortcuts"),
            (1, "rem", "reminders"),
        ] {
            fresh();
            h.state_mut().reload_status_for_test();
            h.state_mut().nw_page_for_test(page);
            h.state_mut().nw_select_for_test(Some(id));
            h.set_size(egui::vec2(1100.0 + 2.0 * MARGIN, 3400.0 + 2.0 * MARGIN));
            save(&mut h, &dir, &format!("notch-widgets-editor-{slug}-{tn}"));
        }
        h.state_mut().nw_select_for_test(None);
        h.state_mut().nw_io_result_for_test(true, "Imported: now 14 widgets on 4 pages", vec!["A widget of an unknown kind was skipped".into()]);
        h.state_mut().nw_page_for_test(0);
        save(&mut h, &dir, &format!("notch-widgets-share-{tn}"));
    }
    // Looking didn't change the layout's widgets.
    let back = pm_config::AppConfig::load_or_default(&path).unwrap();
    assert_eq!(back.notch_shelf.widgets.len(), 13);
    let _ = std::fs::remove_dir_all(&d);
}

/// The setup, Settings → General (Permissions & Health, Updates), the
/// Workspaces page and Mouse Without Borders' pairing card, light and dark.
#[test]
fn setup_shots() {
    use crate::permissions::{Grant, Perm, Probe};
    use pm_config::bridge::{AppPermissions, AppStatus, HookHealth, ModuleState, ModuleStatus, SlowHandler};
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none() {
        return;
    }
    if !has_gpu() {
        return;
    }
    let dir = out_dir();
    let d = scratch("setup");
    let path = d.join("config.json");
    let mut cfg = pm_config::AppConfig::with_new_key();
    cfg.mwb.settings.machine_name = "MacBook-Pro".into();
    cfg.mwb.settings.matrix = vec!["MacBook-Pro".into(), "studio-pc".into()];
    cfg.clipboard_history.enabled = true;
    cfg.window_switcher.enabled = true;
    cfg.audio.enabled = true;
    cfg.audio.per_app_volume = true;
    cfg.workspaces.hotkey = workspaces::Hotkey::new(false, true, true, false, 0x57);
    cfg.save(&path).unwrap();

    // Two saved workspaces.
    let mon = |id: &str, name: &str, x: i32, w: i32, h: i32, primary: bool| workspaces::Monitor {
        id: id.into(),
        name: name.into(),
        number: if primary { 0 } else { 1 },
        frame: workspaces::Rect::new(x, 0, w, h),
        work_area: workspaces::Rect::new(x, 33, w, h - 33),
        scale: 2.0,
        primary,
    };
    let app = |name: &str, bundle: &str, x: i32, y: i32, w: i32, h: i32, m: &str| workspaces::App {
        id: format!("{bundle}-{x}"),
        name: name.into(),
        bundle_id: bundle.into(),
        path: format!("/Applications/{name}.app"),
        frame: workspaces::Rect::new(x, y, w, h),
        monitor_id: m.into(),
        ..Default::default()
    };
    let now = workspaces::model::now_unix();
    let ws = vec![
        workspaces::Workspace {
            id: "{11111111-0000-0000-0000-000000000001}".into(),
            name: "Coding".into(),
            creation_time: now - 86_400 * 3,
            last_launched_time: Some(now - 7200),
            monitors: vec![mon("m1", "Built-in Retina Display", 0, 1512, 982, true), mon("m2", "LG UltraFine", 1512, 2560, 1440, false)],
            apps: vec![
                app("Xcode", "com.apple.dt.Xcode", 1512, 0, 1700, 1440, "m2"),
                app("Terminal", "com.apple.Terminal", 3212, 0, 860, 1440, "m2"),
                app("Safari", "com.apple.Safari", 0, 33, 1512, 949, "m1"),
            ],
            ..Default::default()
        },
        workspaces::Workspace {
            id: "{11111111-0000-0000-0000-000000000002}".into(),
            name: "Writing".into(),
            creation_time: now - 86_400,
            monitors: vec![mon("m1", "Built-in Retina Display", 0, 1512, 982, true)],
            apps: vec![app("Pages", "com.apple.iWork.Pages", 0, 33, 1000, 949, "m1"), app("Notes", "com.apple.Notes", 1000, 33, 512, 949, "m1")],
            ..Default::default()
        },
    ];
    workspaces::store::save(&d, &ws).unwrap();

    // What a running app reports.
    let mstat = |page: &str, name: &str, state: ModuleState, detail: &str| ModuleStatus { page: page.into(), name: name.into(), state, detail: detail.into() };
    let app_status = AppStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        pid: 4242,
        started_at: 0,
        executable: "/Applications/PowerMac.app/Contents/MacOS/powermac".into(),
        permissions: AppPermissions { accessibility: true, input_monitoring: true, screen_recording: false },
        hook: HookHealth {
            state: "active".into(),
            healthy: true,
            reenabled: 1,
            subscribers: vec!["keyboard-manager".into(), "mwb".into(), "window-switcher".into(), "fancyzones".into(), "mouse-utils".into()],
            slow_handlers: vec![SlowHandler { name: "fancyzones".into(), slow: 3, calls: 18_204, max_ms: 14 }],
            ..Default::default()
        },
        modules: vec![
            mstat("fancyzones", "FancyZones", ModuleState::Running, ""),
            mstat("window-switcher", "Window Switcher", ModuleState::Running, "Option+Tab"),
            mstat("workspaces", "Workspaces", ModuleState::Running, "2 saved"),
            mstat("notch-shelf", "Notch Shelf", ModuleState::Running, ""),
            mstat("scrolling", "Scrolling", ModuleState::Running, ""),
            mstat("clipboard-history", "Clipboard History", ModuleState::Running, ""),
            mstat("sound", "Sound", ModuleState::WaitingForPermission, "needs System Audio Recording"),
            mstat("text-extractor", "Text Extractor", ModuleState::WaitingForPermission, "needs Screen & System Audio Recording"),
            mstat("mouse-without-borders", "Mouse Without Borders", ModuleState::Running, "Connected: STUDIO-PC"),
            mstat("color-picker", "Color Picker", ModuleState::Failed, ""),
            mstat("awake", "Awake", ModuleState::Off, ""),
        ],
    };
    let release = pm_update::Release {
        tag: "v1.2.0".into(),
        version: "1.2.0".into(),
        name: "PowerMac 1.2.0".into(),
        notes: "### Added\n- **Workspaces**: capture and restore a desktop of apps.\n- A first-run setup with live permission status.\n\n### Fixed\n- Permissions after rebuilding the app.".into(),
        url: "https://github.com/finepointcgi/PowerMac/releases/tag/v1.2.0".into(),
        prerelease: false,
        published_at: "2026-11-20T10:12:00Z".into(),
        download_url: Some("https://github.com/finepointcgi/PowerMac/releases/download/v1.2.0/PowerMac-1.2.0-macos-arm64.dmg".into()),
        download_name: Some("PowerMac-1.2.0-macos-arm64.dmg".into()),
        download_size: Some(9_437_184),
    };
    let update = pm_update::UpdateState {
        checked_at: pm_update::now_secs() - 1800,
        current: env!("CARGO_PKG_VERSION").into(),
        outcome: Some(pm_update::CheckOutcome::Available { release }),
        error: None,
    };
    let diag = serde_json::json!({
        "at": 0,
        "machineName": "MacBook-Pro",
        "incoming": [
            {"from": "10.0.0.35:50123", "at": 1, "result": "rejected: different security key"},
            {"from": "10.0.0.35:50131", "at": 2, "result": "trusted as 'STUDIO-PC'"}
        ],
        "learned": [{"name": "STUDIO-PC", "addr": "10.0.0.35"}],
        "peers": [],
        "llmnr": {"running": true, "name": "MACBOOK-PRO", "port": 5355, "queries": 14, "answered": 3,
                  "lastAnswer": {"from": "10.0.0.35:61001", "name": "MACBOOK-PRO", "answer": "10.0.0.112", "at": 3}}
    });
    let mut diag = diag;
    let nowms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    diag["incoming"][0]["at"] = (nowms - 600_000).into();
    diag["incoming"][1]["at"] = (nowms - 240_000).into();
    std::fs::write(d.join("mwb-diagnostics.json"), serde_json::to_string(&diag).unwrap()).unwrap();
    let doctor = "== This Mac\nMouse Without Borders enabled: true\nmachine name: MacBook-Pro   (type MACBOOK-PRO in PowerToys; case doesn't matter)\nIPv4: 10.0.0.112/24  <- LAN address Windows should use\nTCP 15101 (MWB messages): listening - powermac (pid 4242)\n\n== Security key\nvalid: true (16 characters after removing whitespace; PowerToys needs >= 16)\nbit-31 quirk: not affected (magic 0x00a1b2c3)\n\n== Name lookups from Windows (LLMNR, UDP 5355)\nquery MACBOOK-PRO at 127.0.0.1:5355: answered 10.0.0.112\n\n== Summary\nno problems found on this Mac".to_string();
    pm_config::bridge::write(&path, pm_config::bridge::UPDATE_STATUS_FILE, &update).unwrap();
    let fresh = |status: &AppStatus| {
        pm_config::bridge::write(&path, pm_config::bridge::APP_STATUS_FILE, status).unwrap();
        pm_config::MwbStatus {
            running: true,
            machine_name: "MacBook-Pro".into(),
            machines: vec![pm_config::MwbMachine { name: "STUDIO-PC".into(), address: "10.0.0.35".into(), connected: true, discovered: true }],
            ..Default::default()
        }
        .stamp()
        .save_for(&path)
        .unwrap();
    };

    // Live probe: Accessibility and Input Monitoring granted; Screen
    // Recording granted here but not in the running app (restart needed);
    // audio capture missing.
    let probe = Probe {
        accessibility: Grant::Granted,
        input_monitoring: Grant::Granted,
        screen_recording: Grant::Granted,
        audio_capture: Grant::Denied,
    };
    let first_run = Probe { accessibility: Grant::Denied, input_monitoring: Grant::Denied, screen_recording: Grant::Denied, audio_capture: Grant::NotDetermined };
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        for (size, suffix) in [(win(1000.0, 720.0), ""), (win(1000.0, 1500.0), "-full"), (win(1000.0, 2500.0), "-tall")] {
            let mut app = SettingsApp::new(path.clone());
            app.set_pairing_for_test(
                vec![("10.0.0.112".parse().unwrap(), true, false), ("100.64.12.7".parse().unwrap(), false, true)],
                Some(doctor.clone()),
            );
            let mut h = Harness::builder()
                .with_size(size)
                .with_theme(theme)
                .with_accessibility_check(false)
                .wgpu()
                .build_ui_state(|ui, app: &mut SettingsApp| app.draw(ui), app);
            h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
            h.state_mut().set_alt_tab_for_test(false, vec![]);
            // The setup, as on a first launch (nothing granted, app not running yet).
            h.state_mut().set_app_status_for_test(None);
            h.state_mut().set_probe_for_test(first_run.clone(), &[]);
            h.state_mut().open_named("welcome");
            let tall = suffix == "-tall";
            for (step, name) in [(0, "welcome"), (1, "features"), (2, "permissions"), (3, "done")] {
                if tall {
                    break;
                }
                h.state_mut().set_setup_step_for_test(step);
                save(&mut h, &dir, &format!("setup-{name}-{tn}{suffix}"));
            }
            // Permissions after granting: one needs a restart, one has an outdated entry.
            fresh(&app_status);
            h.state_mut().set_app_status_for_test(Some(app_status.clone()));
            h.state_mut().set_probe_for_test(probe.clone(), &[Perm::AudioCapture]);
            h.state_mut().set_setup_step_for_test(2);
            if !tall {
                save(&mut h, &dir, &format!("setup-permissions-granted-{tn}{suffix}"));
            }
            // Settings pages.
            h.state_mut().set_update_state_for_test(update.clone());
            h.state_mut().set_probe_for_test(probe.clone(), &[]);
            for (page, name) in [(Page::General, "general"), (Page::Workspaces, "workspaces"), (Page::Mwb, "mwb")] {
                if tall && page == Page::Workspaces {
                    continue;
                }
                fresh(&app_status);
                h.state_mut().set_page(page);
                h.state_mut().reload_status_for_test();
                h.state_mut().set_app_status_for_test(Some(app_status.clone()));
                h.state_mut().set_update_state_for_test(update.clone());
                save(&mut h, &dir, &format!("final-{name}-{tn}{suffix}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Routing, mixer, battery, network and automation samples (shared by the
/// page shots).
fn int5_config(cfg: &mut pm_config::AppConfig) {
    use audio_control::{AppRoute, DeviceMatch, EqConfig, Effects, LimiterConfig, MixOutput, MixSource, SourceKind};
    use keyboard_manager::automation::{Action, NotifyWhen, ShellAction};
    cfg.audio.enabled = true;
    cfg.audio.app_routes.insert(
        "com.spotify.client".into(),
        AppRoute { name: "Spotify".into(), ..AppRoute::to(DeviceMatch::uid("airpods", "Ada's AirPods Pro")) },
    );
    cfg.audio.app_routes.insert(
        "com.google.Chrome".into(),
        AppRoute {
            name: "Google Chrome".into(),
            outputs: vec![DeviceMatch::uid("PowerMacAudio:mix1", "PowerMac Mix 1"), DeviceMatch::uid("studio-uid", "Studio Display Speakers")],
            keep_default: true,
            gain_db: -3.0,
            enabled: true,
        },
    );
    let mut eq = EqConfig::flat(5);
    eq.bands[0].gain_db = -4.0;
    eq.bands[2].gain_db = 3.5;
    eq.bands[2].q = 1.4;
    eq.bands[4].gain_db = 2.0;
    cfg.audio.mixer = audio_control::Mixer {
        enabled: true,
        sources: vec![
            MixSource { id: "music".into(), name: "Spotify".into(), kind: SourceKind::App { bundle_id: "com.spotify.client".into() }, gain_db: -6.0, ..Default::default() },
            MixSource {
                id: "mic".into(),
                name: "Shure MV7".into(),
                kind: SourceKind::Input { device: DeviceMatch::uid("mv7", "Shure MV7") },
                mono: true,
                solo: false,
                gain_db: 2.5,
                effects: Effects { gain_db: 4.0, eq, limiter: LimiterConfig { enabled: true, ceiling_db: -1.5, ..Default::default() } },
                ..Default::default()
            },
            MixSource { id: "game".into(), name: "PowerMac Audio".into(), kind: SourceKind::Loopback { device: DeviceMatch::uid("PowerMacAudio:main", "PowerMac Audio") }, muted: true, ..Default::default() },
        ],
        outputs: vec![
            MixOutput { id: "spk".into(), name: "Studio Display".into(), device: DeviceMatch::uid("studio-uid", "Studio Display Speakers"), ..Default::default() },
            MixOutput { id: "stream".into(), name: "Stream mix".into(), device: DeviceMatch::uid("PowerMacAudio:mix1", "PowerMac Mix 1"), gain_db: -2.0, ..Default::default() },
        ],
        sends: Vec::new(),
    };
    let m = &mut cfg.audio.mixer;
    m.set_send("music", "spk", 0.0, false);
    m.set_send("music", "stream", -9.0, false);
    m.set_send("mic", "stream", 0.0, false);
    m.set_send("game", "spk", 0.0, true);
    m.set_send("game", "stream", -3.0, false);
    cfg.battery_limit.enabled = true;
    cfg.battery_limit.limit_percent = 80;
    cfg.keyboard_manager.enabled = true;
    cfg.keyboard_manager.hotkeys = vec![
        keyboard_manager::Hotkey {
            from: "Ctrl+Alt+B".parse().unwrap(),
            action: Action::Shell(ShellAction {
                name: "Build app".into(),
                command: "make -C ~/src/app".into(),
                wait: true,
                timeout_ms: Some(120_000),
                notify: NotifyWhen::OnFailure,
                env: [("PROFILE".to_string(), "release".to_string())].into_iter().collect(),
                ..Default::default()
            }),
            app: None,
            enabled: true,
        },
        keyboard_manager::Hotkey { from: "F13".parse().unwrap(), action: Action::OpenUri { uri: "https://github.com/notifications".into() }, app: None, enabled: true },
        keyboard_manager::Hotkey {
            from: "Cmd+Shift+L".parse().unwrap(),
            action: Action::Text { text: "ls -la\n".into() },
            app: Some("com.apple.Terminal".into()),
            enabled: false,
        },
    ];
    cfg.mouse_tuning.enabled = true;
    cfg.mouse_tuning.buttons.mappings = vec![
        mouse_tuning::ButtonMapping { button: mouse_tuning::button::MIDDLE, action: mouse_tuning::ButtonAction::MissionControl },
        mouse_tuning::ButtonMapping {
            button: mouse_tuning::button::BACK,
            action: mouse_tuning::ButtonAction::Action(Action::Shell(ShellAction { command: "open -a Notes".into(), ..Default::default() })),
        },
    ];
    cfg.gestures.bindings = vec![
        gestures::GestureBinding::new("3-finger-tap".parse().unwrap(), Action::Shortcut { keys: "Cmd+W".into() }),
        gestures::GestureBinding::new(
            "4-finger-swipe-down".parse().unwrap(),
            Action::Shell(ShellAction { name: "Sleep display".into(), command: "pmset displaysleepnow".into(), ..Default::default() }),
        ),
        gestures::GestureBinding { app: Some("com.google.Chrome".into()), ..gestures::GestureBinding::new("3-finger-click".parse().unwrap(), Action::Shortcut { keys: "Cmd+Shift+T".into() }) },
    ];
}

fn int5_audio_status() -> pm_config::bridge::AudioStatus {
    use audio_control::{MixerState, RouteStatus, RouteTarget, StripKind, StripState, StripStatus, Transport as T};
    let dev = |uid: &str, name: &str, t: T, def: bool| audio_control::AudioDevice {
        uid: uid.into(),
        name: name.into(),
        transport: t,
        is_default: def,
        is_default_system: def,
        channels: 2,
        volume: Some(0.5),
        muted: Some(false),
    };
    let strip = |id: &str, name: &str, kind: StripKind, state: StripState| StripStatus { id: id.into(), name: name.into(), kind, state, device_uid: None, device_name: None };
    pm_config::bridge::AudioStatus {
        running: true,
        outputs: vec![
            dev("studio-uid", "Studio Display Speakers", T::Usb, true),
            dev("builtin-out", "MacBook Pro Speakers", T::BuiltIn, false),
            dev("airpods", "Ada's AirPods Pro", T::Bluetooth, false),
            dev("PowerMacAudio:main", "PowerMac Audio", T::Virtual, false),
            dev("PowerMacAudio:mix1", "PowerMac Mix 1", T::Virtual, false),
        ],
        inputs: vec![
            dev("mv7", "Shure MV7", T::Usb, false),
            dev("builtin-in", "MacBook Pro Microphone", T::BuiltIn, true),
            dev("PowerMacAudio:main", "PowerMac Audio", T::Virtual, false),
        ],
        apps: vec![
            audio_control::AudioApp { pid: 10, pids: vec![10], bundle_id: "com.spotify.client".into(), name: "Spotify".into(), volume: 1.0, muted: false, playing: true },
            audio_control::AudioApp { pid: 11, pids: vec![11], bundle_id: "com.google.Chrome".into(), name: "Google Chrome".into(), volume: 1.0, muted: false, playing: true },
            audio_control::AudioApp { pid: 12, pids: vec![12], bundle_id: "us.zoom.xos".into(), name: "zoom.us".into(), volume: 1.0, muted: false, playing: false },
        ],
        per_app_supported: true,
        capture_permission: audio_control::CapturePermission::Granted,
        error: None,
        mixer: Some(MixerState {
            supported: true,
            app_sources_supported: true,
            running: true,
            sample_rate: Some(48_000.0),
            clock_device: Some("studio-uid".into()),
            capture_permission: audio_control::CapturePermission::Granted,
            sources: vec![
                strip("music", "Spotify", StripKind::App, StripState::Active),
                strip("mic", "Shure MV7", StripKind::Input, StripState::Active),
                strip("game", "PowerMac Audio", StripKind::Loopback, StripState::Active),
            ],
            outputs: vec![
                strip("spk", "Studio Display", StripKind::Output, StripState::Active),
                strip(
                    "stream",
                    "Stream mix",
                    StripKind::Output,
                    StripState::Unavailable { reason: "PowerMac Mix 1 runs at 44100 Hz and the mixer at 48000 Hz (set them to the same rate in Audio MIDI Setup)".into() },
                ),
            ],
            routes: vec![
                RouteStatus {
                    bundle_id: "com.spotify.client".into(),
                    name: "Spotify".into(),
                    enabled: true,
                    active: true,
                    keep_default: false,
                    targets: vec![RouteTarget { uid: Some("airpods".into()), name: "Ada's AirPods Pro".into(), available: true, reason: None }],
                    state: StripState::Active,
                    message: None,
                },
                RouteStatus {
                    bundle_id: "com.google.Chrome".into(),
                    name: "Google Chrome".into(),
                    enabled: true,
                    active: false,
                    keep_default: true,
                    targets: vec![
                        RouteTarget { uid: Some("PowerMacAudio:mix1".into()), name: "PowerMac Mix 1".into(), available: false, reason: Some("runs at another sample rate".into()) },
                        RouteTarget { uid: Some("studio-uid".into()), name: "Studio Display Speakers".into(), available: true, reason: None },
                    ],
                    state: StripState::Waiting { reason: "the target device runs at another sample rate".into() },
                    message: None,
                },
            ],
            errors: Vec::new(),
            callbacks: 48_211,
        }),
    }
}

fn int5_meters() -> audio_control::Meters {
    let m = |id: &str, peak: [f32; 2], rms: [f32; 2], lim: f32, clips: u32| audio_control::MeterReading { id: id.into(), peak, rms, limiter_reduction_db: lim, clips };
    audio_control::Meters {
        sources: vec![m("music", [0.62, 0.58], [0.31, 0.29], 0.0, 0), m("mic", [0.89, 0.89], [0.42, 0.42], 2.4, 0), m("game", [0.2, 0.18], [0.08, 0.07], 0.0, 0)],
        outputs: vec![m("spk", [0.7, 0.66], [0.36, 0.34], 0.0, 0), m("stream", [1.0, 0.97], [0.55, 0.52], 0.0, 3)],
    }
}

fn int5_battery(strategy: battery_limit::Strategy, native: Option<u8>) -> battery_limit::BatteryStatus {
    battery_limit::BatteryStatus {
        battery: battery_limit::BatteryReading {
            has_battery: true,
            percent: Some(78),
            charging: false,
            plugged: true,
            adapter_watts: Some(96),
            cycles: Some(312),
            health_percent: Some(91.4),
            design_capacity_mah: Some(8579),
            max_capacity_mah: Some(7841),
            temperature_c: Some(31.2),
            time_remaining_min: None,
            amperage_ma: Some(0),
        },
        inhibited: strategy != battery_limit::Strategy::LimitViaDischarge,
        discharging: strategy == battery_limit::Strategy::LimitViaDischarge,
        helper_installed: true,
        helper_running: true,
        scheme: Some(if strategy == battery_limit::Strategy::LimitViaDischarge { "CHIE".into() } else { "CHTE+CHIE".into() }),
        charge_control: Some(strategy != battery_limit::Strategy::LimitViaDischarge),
        strategy: Some(strategy),
        strategy_label: strategy.label().into(),
        strategy_note: strategy.note().into(),
        native_limit_percent: native,
        reason: "at limit (80 %)".into(),
        supported: true,
        ..Default::default()
    }
}

fn int5_network() -> (net_monitor::Snapshot, net_monitor::RuleSet) {
    use net_monitor::{Protocol, Socket, TcpState};
    use std::net::SocketAddr;
    let p = |pid: u32, name: &str, path: &str| net_monitor::ProcessInfo { pid, name: name.into(), path: Some(path.into()) };
    let a = |s: &str| -> SocketAddr { s.parse().unwrap() };
    let tcp = |pid: u32, l: &str, r: &str, st: TcpState| Socket { pid, protocol: Protocol::Tcp, local: a(l), remote: Some(a(r)), state: Some(st) };
    let snap = net_monitor::Snapshot::new(
        vec![
            p(812, "Google Chrome Helper", "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"),
            p(901, "Spotify", "/Applications/Spotify.app/Contents/MacOS/Spotify"),
            p(950, "curl", "/usr/bin/curl"),
            p(990, "Dropbox", "/Applications/Dropbox.app/Contents/MacOS/Dropbox"),
        ],
        vec![
            tcp(812, "192.168.1.5:50001", "142.250.72.14:443", TcpState::Established),
            tcp(812, "192.168.1.5:50002", "151.101.1.69:443", TcpState::Established),
            Socket { pid: 812, protocol: Protocol::Udp, local: a("192.168.1.5:61022"), remote: Some(a("142.250.72.14:443")), state: None },
            tcp(901, "192.168.1.5:50110", "35.186.224.25:4070", TcpState::Established),
            tcp(901, "192.168.1.5:50111", "192.168.1.20:57621", TcpState::Established),
            tcp(950, "192.168.1.5:60131", "93.184.215.14:80", TcpState::SynSent),
            tcp(990, "127.0.0.1:17600", "127.0.0.1:50999", TcpState::Established),
            tcp(990, "192.168.1.5:50200", "162.125.19.131:443", TcpState::Established),
            Socket { pid: 990, protocol: Protocol::Tcp, local: a("0.0.0.0:17500"), remote: None, state: Some(TcpState::Listen) },
        ],
        342,
    );
    let mut rules = net_monitor::RuleSet::default();
    rules.add(net_monitor::Rule { created: 1, ..net_monitor::Rule::for_app(net_monitor::AppMatch::Name("curl".into()), net_monitor::Action::Deny) });
    rules.add(net_monitor::Rule {
        action: net_monitor::Action::Allow,
        app: net_monitor::AppMatch::Bundle("/Applications/Spotify.app".into()),
        remote: net_monitor::RemoteMatch::LocalNetwork,
        direction: net_monitor::rules::DirectionMatch::Both,
        note: "Spotify Connect".into(),
        created: 1,
        ..Default::default()
    });
    rules.add(net_monitor::Rule {
        action: net_monitor::Action::Ask,
        app: net_monitor::AppMatch::Bundle("/Applications/Dropbox.app".into()),
        remote: net_monitor::RemoteMatch::Network { addr: "162.125.0.0".parse().unwrap(), prefix: 16 },
        ports: net_monitor::rules::Ports::parse("443").unwrap(),
        protocol: Some(net_monitor::Protocol::Tcp),
        source: Some("Dropbox telemetry".into()),
        created: 1,
        ..Default::default()
    });
    (snap, rules)
}

/// The Sound page's App routing card, the Mixer, Battery, Automation and
/// Network pages, light and dark.
#[test]
fn int5_shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none() {
        return;
    }
    if !has_gpu() {
        return;
    }
    let dir = out_dir();
    let d = scratch("int5");
    let path = d.join("config.json");
    let mut cfg = pm_config::AppConfig::with_new_key();
    int5_config(&mut cfg);
    cfg.save(&path).unwrap();
    let audio = int5_audio_status();
    let gst = pm_config::bridge::GesturesStatus { state: "running".into(), detail: "1 trackpad".into(), devices: 1, ..Default::default() };
    let app_status = pm_config::bridge::AppStatus { version: "1.0.0".into(), ..Default::default() };
    let fresh = || {
        pm_config::bridge::write(&path, pm_config::bridge::AUDIO_STATUS_FILE, &audio).unwrap();
        pm_config::bridge::write(&path, pm_config::bridge::APP_STATUS_FILE, &app_status).unwrap();
        pm_config::bridge::write(&path, pm_config::bridge::GESTURES_STATUS_FILE, &gst).unwrap();
    };
    let (snap, rules) = int5_network();
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        for (size, suffix) in [(win(1100.0, 780.0), ""), (win(1100.0, 2600.0), "-full")] {
            fresh();
            let app = SettingsApp::new(path.clone());
            let mut h = Harness::builder()
                .with_size(size)
                .with_theme(theme)
                .with_accessibility_check(false)
                .wgpu()
                .build_ui_state(|ui, app: &mut SettingsApp| app.draw(ui), app);
            h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
            h.state_mut().set_alt_tab_for_test(false, vec![]);
            h.state_mut().set_meters_for_test(int5_meters());
            h.state_mut().set_battery_for_test(Some(int5_battery(battery_limit::Strategy::Inhibit, None)));
            h.state_mut().set_gestures_for_test(Some(gst.clone()), &[]);
            let shot = |h: &mut Harness<'_, SettingsApp>, page: Page, name: &str| {
                fresh();
                h.state_mut().set_page(page);
                h.state_mut().reload_status_for_test();
                save(h, &dir, &format!("int5-{name}-{tn}{suffix}"));
            };
            shot(&mut h, Page::Sound, "sound");
            shot(&mut h, Page::Mixer, "mixer");
            h.state_mut().select_strip_for_test(Some("src:mic"));
            shot(&mut h, Page::Mixer, "mixer-fx");
            h.state_mut().select_strip_for_test(None);
            shot(&mut h, Page::Battery, "battery");
            h.state_mut().set_battery_for_test(Some(int5_battery(battery_limit::Strategy::LimitViaDischarge, Some(80))));
            shot(&mut h, Page::Battery, "battery-discharge");
            shot(&mut h, Page::Automation, "automation");
            h.state_mut().set_gestures_for_test(Some(gst.clone()), &["hk:0", "g:1", &format!("btn:{}", mouse_tuning::button::BACK)]);
            h.state_mut().set_test_result_for_test("hk:0", true, "exit 0: make: Nothing to be done for 'all'.", "make: Nothing to be done for 'all'.\n");
            shot(&mut h, Page::Automation, "automation-edit");
            for (view, name) in [(0u8, "network"), (1, "network-rules"), (2, "network-audit"), (3, "network-rule-edit")] {
                h.state_mut().set_network_for_test(snap.clone(), rules.clone(), view);
                shot(&mut h, Page::Network, name);
            }
        }
    }
    // Looking didn't change the config.
    let back = pm_config::AppConfig::load_or_default(&path).unwrap();
    assert_eq!(back.audio, cfg.audio);
    assert_eq!(back.keyboard_manager.hotkeys, cfg.keyboard_manager.hotkeys);
    assert_eq!(back.gestures, cfg.gestures);
    let _ = std::fs::remove_dir_all(&d);
}

/// The Drag Shelf page, light and dark, with the default and the custom
/// sensitivity (`drag-shelf-*.png`).
#[test]
fn drag_shelf_shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none() {
        return;
    }
    if !has_gpu() {
        return;
    }
    let dir = out_dir();
    let d = scratch("drag-shelf");
    let path = d.join("config.json");
    for (variant, custom) in [("", false), ("-custom", true)] {
        let mut cfg = pm_config::AppConfig::with_new_key();
        cfg.drag_shelf.enabled = true;
        cfg.drag_shelf.hotkey = drag_shelf::Hotkey::new(false, true, true, false, 0x44);
        if custom {
            cfg.drag_shelf.sensitivity = drag_shelf::Sensitivity::Custom;
            cfg.drag_shelf.custom_shake = drag_shelf::ShakeThresholds { reversals: 4, window_ms: 800, min_segment_px: 30, min_distance_px: 260 };
        }
        cfg.save(&path).unwrap();
        for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
            for (size, suffix) in [(win(1100.0, 780.0), ""), (win(1100.0, 1300.0), "-full")] {
                let mut app = SettingsApp::new(path.clone());
                app.set_page(Page::DragShelf);
                let mut h = Harness::builder()
                    .with_size(size)
                    .with_theme(theme)
                    .with_accessibility_check(false)
                    .wgpu()
                    .build_ui_state(|ui, app: &mut SettingsApp| app.draw(ui), app);
                h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
                h.state_mut().set_alt_tab_for_test(false, vec![]);
                save(&mut h, &dir, &format!("drag-shelf{variant}-{tn}{suffix}"));
            }
        }
        // Looking didn't change the config.
        let back = pm_config::AppConfig::load_or_default(&path).unwrap();
        assert_eq!(back.drag_shelf, cfg.drag_shelf);
    }
    let _ = std::fs::remove_dir_all(&d);
}
