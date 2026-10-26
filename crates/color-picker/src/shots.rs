//! Offscreen screenshots of the editor (egui_kittest + wgpu). Run with
//! `PM_UI_SHOTS=<dir> cargo test -p color-picker shots` to write PNGs into
//! `<dir>`; skipped by default and when no GPU adapter is available.

use std::path::PathBuf;

use eframe::egui;
use egui_kittest::Harness;

use crate::editor::EditorApp;
use crate::{history, Config, CustomFormat, Rgb};

const MARGIN: f32 = 8.0;

fn win(w: f32, h: f32) -> [f32; 2] {
    [w + 2.0 * MARGIN, h + 2.0 * MARGIN]
}

fn save(h: &mut Harness<'_, impl Sized>, dir: &Option<PathBuf>, name: &str) {
    h.run_steps(4);
    match h.render() {
        Ok(img) => {
            if let Some(d) = dir {
                std::fs::create_dir_all(d).ok();
                let (w, hh) = img.dimensions();
                let img = image::imageops::crop_imm(
                    &img,
                    MARGIN as u32,
                    MARGIN as u32,
                    w - 2 * MARGIN as u32,
                    hh - 2 * MARGIN as u32,
                )
                .to_image();
                img.save(d.join(format!("{name}.png"))).expect("save png");
            }
        }
        Err(e) => eprintln!("render {name} skipped: {e}"),
    }
}

fn has_gpu() -> bool {
    let mut h = Harness::builder()
        .with_size([64.0, 64.0])
        .wgpu()
        .build_ui(|ui| {
            ui.label("x");
        });
    h.run();
    h.render().is_ok()
}

#[test]
fn shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none()
    {
        return;
    }
    if !has_gpu() {
        return;
    }
    let dir = std::env::var_os("PM_UI_SHOTS").map(PathBuf::from);
    let data = std::env::temp_dir().join(format!("cp-shots-{}", std::process::id()));
    let empty = data.join("empty");
    let _ = std::fs::remove_dir_all(&data);
    let colors = [
        "#FFE4B5", "#0A84FF", "#FF375F", "#30D158", "#1C1C1E", "#BF5AF2", "#FFD60A", "#64D2FF",
        "#AC8E68", "#F2F2F7",
    ];
    let hist: Vec<Rgb> = colors.iter().map(|h| Rgb::from_hex(h).unwrap()).collect();
    history::save(&data, &hist).unwrap();
    let cfg = Config {
        visible_formats: [
            "HEX", "RGB", "HSL", "CMYK", "CIELAB", "Oklch", "SwiftUI", "NSColor", "CSS var",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        custom_formats: vec![CustomFormat {
            name: "CSS var".into(),
            format: "--accent: #%Rex%Grx%Blx;".into(),
        }],
        show_color_name: true,
        ..Default::default()
    };

    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        for (sel, slug) in [(1usize, "blue"), (0, "moccasin"), (4, "dark")] {
            let mut app = EditorApp::new(data.clone(), cfg.clone());
            app.select(sel);
            let mut h = Harness::builder()
                .with_size(win(820.0, 580.0))
                .with_theme(theme)
                .with_accessibility_check(false)
                .wgpu()
                .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app);
            save(&mut h, &dir, &format!("editor-{slug}-{tn}"));
            if sel == 1 {
                let app = EditorApp::new(data.clone(), cfg.clone());
                let mut tall = Harness::builder()
                    .with_size(win(820.0, 1000.0))
                    .with_theme(theme)
                    .with_accessibility_check(false)
                    .wgpu()
                    .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app);
                tall.state_mut().select(1);
                save(&mut tall, &dir, &format!("editor-full-{tn}"));
                h.state_mut().set_draft(Some(Rgb::new(0x5E, 0x5C, 0xE6)));
                h.state_mut().show_toast_for_test("Copied 5e5ce6");
                save(&mut h, &dir, &format!("editor-adjusted-{tn}"));
            }
        }
        let app = EditorApp::new(empty.clone(), Config::default());
        let mut h = Harness::builder()
            .with_size(win(820.0, 580.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("editor-empty-{tn}"));
        // Minimum window size.
        let mut app = EditorApp::new(data.clone(), cfg.clone());
        app.select(2);
        let mut h = Harness::builder()
            .with_size(win(640.0, 440.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("editor-small-{tn}"));
    }
    // Looking doesn't write anything.
    assert_eq!(history::load(&data), hist);
    let _ = std::fs::remove_dir_all(&data);
}
