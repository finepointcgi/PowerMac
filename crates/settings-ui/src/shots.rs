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
    cfg.notch_shelf.weather.location = "Portland, Oregon".into();
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
