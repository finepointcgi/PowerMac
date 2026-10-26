//! Offscreen screenshots of the picker (like settings-ui's `shots.rs`). Run
//! with `PM_UI_SHOTS=<dir> cargo test -p clipboard-history shots` to write
//! PNGs; skipped by default and without a GPU adapter.

use std::path::PathBuf;

use eframe::egui;
use egui_kittest::Harness;

use crate::config::Config;
use crate::content::{Capture, Content, SourceApp};
use crate::picker::{Backend, PickerApp, HEIGHT, WIDTH};
use crate::store::Store;

const MARGIN: f32 = 8.0;
const NOW: i64 = 1_760_000_000_000;
const MIN: i64 = 60_000;

fn out_dir() -> Option<PathBuf> {
    std::env::var_os("PM_UI_SHOTS").map(PathBuf::from)
}

fn save(h: &mut Harness<'_, PickerApp>, dir: &Option<PathBuf>, name: &str) {
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

/// A photo-like test picture: sky gradient, sun and hills.
fn picture(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let hill = 0.62 + 0.08 * (fx * 7.0).sin() + 0.05 * (fx * 13.0 + 1.0).cos();
        let hill2 = 0.75 + 0.06 * (fx * 5.0 + 2.0).sin();
        let (sx, sy) = (fx - 0.72, fy - 0.3);
        let sun = (sx * sx * 3.0 + sy * sy).sqrt() < 0.09;
        let c = if fy > hill2 {
            [46.0, 110.0 - fy * 30.0, 60.0]
        } else if fy > hill {
            [70.0, 140.0 - fy * 40.0, 80.0]
        } else if sun {
            [255.0, 214.0, 120.0]
        } else {
            [80.0 + fy * 170.0, 140.0 + fy * 90.0, 230.0 - fy * 30.0]
        };
        image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn src(name: &str, id: &str) -> Option<SourceApp> {
    Some(SourceApp {
        name: Some(name.into()),
        id: Some(id.into()),
    })
}

pub(crate) fn sample_store() -> Store {
    let cfg = Config::default();
    let mut s = Store::in_memory();
    let mut add = |c: Content, source: Option<SourceApp>, t: i64| {
        s.add(
            Capture {
                content: c,
                types: vec![],
                source,
            },
            &cfg,
            t,
        )
        .unwrap()
        .id()
    };
    let addr = add(
        Content::text("Ada Lovelace\n12 Analytical Engine Way\nLondon NW1 6XE"),
        src("Contacts", "com.apple.AddressBook"),
        NOW - 9 * 86_400_000,
    );
    let sig = add(
        Content::text("Kind regards,\nAda"),
        src("Mail", "com.apple.mail"),
        NOW - 3 * 86_400_000,
    );
    add(
        Content::text("cargo test -p clipboard-history -- --nocapture"),
        src("Terminal", "com.apple.Terminal"),
        NOW - 26 * 3_600_000,
    );
    add(
        Content {
            files: vec![
                "/Users/ada/Documents/Q3 Report.pdf".into(),
                "/Users/ada/Documents/Budget 2026.xlsx".into(),
            ],
            text: Some("Q3 Report.pdf\nBudget 2026.xlsx".into()),
            ..Default::default()
        },
        src("Finder", "com.apple.finder"),
        NOW - 5 * 3_600_000,
    );
    add(
        Content {
            text: Some("The Analytical Engine weaves algebraic patterns just as the Jacquard loom weaves flowers and leaves. Its operating mechanism can act upon other things besides number.".into()),
            html: Some("<p>The <b>Analytical Engine</b> weaves…</p>".into()),
            ..Default::default()
        },
        src("Safari", "com.apple.Safari"),
        NOW - 47 * MIN,
    );
    add(
        Content {
            png: Some(picture(1280, 720)),
            ..Default::default()
        },
        src("Preview", "com.apple.Preview"),
        NOW - 12 * MIN,
    );
    add(
        Content::text("https://github.com/finepointcgi/PowerMac/pull/42"),
        src("Google Chrome", "com.google.Chrome"),
        NOW - 3 * MIN,
    );
    add(
        Content::text("fn main() {\n    println!(\"hello\");\n}"),
        src("Visual Studio Code", "com.microsoft.VSCode"),
        NOW - 20_000,
    );
    s.set_pinned(addr, true);
    s.set_pinned(sig, true);
    s
}

fn harness(app: PickerApp, theme: egui::Theme) -> Harness<'static, PickerApp> {
    Harness::builder()
        .with_size([WIDTH + 2.0 * MARGIN, HEIGHT + 2.0 * MARGIN])
        .with_theme(theme)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui_state(|ui, app: &mut PickerApp| app.draw(ui), app)
}

fn app(store: Store, paused: bool) -> PickerApp {
    let mut a = PickerApp::new(Backend::Local {
        store,
        cfg: Config {
            paused,
            ..Default::default()
        },
    });
    a.now = Some(NOW);
    a
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
        return;
    }
    if !has_gpu() {
        return;
    }
    let dir = out_dir();
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let mut h = harness(app(sample_store(), false), theme);
        save(&mut h, &dir, &format!("picker-{tn}"));
        h.state_mut().select(3);
        save(&mut h, &dir, &format!("picker-selected-{tn}"));
        h.state_mut().set_query("report");
        save(&mut h, &dir, &format!("picker-search-{tn}"));
        h.state_mut().set_query("zebra");
        save(&mut h, &dir, &format!("picker-nomatch-{tn}"));
        let mut h = harness(app(Store::in_memory(), true), theme);
        save(&mut h, &dir, &format!("picker-empty-paused-{tn}"));
    }
}
