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
    let fresh_status = || display_status.clone().stamp().save_for(&path).unwrap();
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
