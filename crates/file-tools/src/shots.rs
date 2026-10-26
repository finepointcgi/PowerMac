//! Offscreen screenshots of the PowerRename and New+ windows (egui_kittest
//! and wgpu), plus a few interaction checks through the window state. Run
//! `PM_UI_SHOTS=<dir> cargo test -p file-tools shots` to write PNGs into a
//! folder; skipped by default and when no GPU adapter is available.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::Harness;

use crate::config::Config;
use crate::newplus_ui::NewApp;
use crate::rename::{ApplyTo, TextCase};
use crate::rename_ui::RenameApp;
use crate::resize::{OutputFormat, ResizeFit, ResizeUnit};
use crate::resize_ui::ResizeApp;

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

fn sample(root: &Path) -> Vec<PathBuf> {
    let w = root.join("Holiday 2024");
    std::fs::create_dir_all(w.join("Raw files")).unwrap();
    for n in [
        "IMG_0001.JPG",
        "IMG_0002.JPG",
        "IMG_0003.JPG",
        "IMG_0004.HEIC",
        "IMG_0005.JPG",
        "notes from the trip.txt",
        "itinerary FINAL (2).pdf",
        "Raw files/IMG_0001.dng",
        "Raw files/IMG_0002.dng",
    ] {
        std::fs::write(w.join(n), n).unwrap();
    }
    std::fs::write(w.join("Beach.JPG"), "x").unwrap();
    let mut v: Vec<PathBuf> = std::fs::read_dir(&w)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    v
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
    let tmp = tempfile::tempdir().unwrap();
    let paths = sample(tmp.path());
    let cfg = tmp.path().join("file-tools.json");
    let mut c = Config::default();
    c.rename.search_mru = vec!["IMG_".into(), "(\\d+)".into()];
    c.save_to(&cfg).unwrap();
    let journal = tmp.path().join("undo.json");

    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let app = RenameApp::new(paths.clone(), Some(cfg.clone()), Some(journal.clone()));
        let mut h = Harness::builder()
            .with_size(win(1100.0, 720.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut RenameApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("rename-empty-{tn}"));
        h.state_mut().set_options(|o| {
            o.search = "IMG_".into();
            o.replace = "Holiday ".into();
        });
        save(&mut h, &dir, &format!("rename-plain-{tn}"));
        h.state_mut().set_options(|o| {
            o.use_regex = true;
            o.search = r"^IMG_(\d+)".into();
            o.replace = "$YYYY-$MM-$DD Beach ${start=1,padding=3}".into();
            o.apply_to = ApplyTo::NameOnly;
            o.text_case = TextCase::None;
        });
        h.state_mut().set_help(true);
        save(&mut h, &dir, &format!("rename-regex-help-{tn}"));
        h.set_size(egui::vec2(1100.0 + 2.0 * MARGIN, 1400.0 + 2.0 * MARGIN));
        save(&mut h, &dir, &format!("rename-regex-help-{tn}-full"));
        h.set_size(egui::vec2(1100.0 + 2.0 * MARGIN, 720.0 + 2.0 * MARGIN));
        h.state_mut().set_help(false);
        // Conflicts: everything to the same name.
        h.state_mut().set_options(|o| {
            o.use_regex = true;
            o.search = r"^IMG_\d+".into();
            o.replace = "Beach".into();
            o.apply_to = ApplyTo::NameOnly;
        });
        h.state_mut().deselect(1);
        h.state_mut().set_only_changed(true);
        save(&mut h, &dir, &format!("rename-conflicts-{tn}"));
        h.state_mut().set_only_changed(false);
        h.state_mut().set_options(|o| {
            o.use_regex = false;
            o.search = String::new();
            o.text_case = TextCase::Title;
            o.apply_to = ApplyTo::NameAndExtension;
        });
        h.state_mut().set_progress(4, 11);
        save(&mut h, &dir, &format!("rename-progress-{tn}"));
        let mut app2 = RenameApp::new(paths.clone(), Some(cfg.clone()), None);
        app2.set_options(|o| o.search = "(".into());
        app2.set_options(|o| o.use_regex = true);
        app2.set_outcome_errors();
        let mut h = Harness::builder()
            .with_size(win(900.0, 560.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut RenameApp| app.draw(ui), app2);
        save(&mut h, &dir, &format!("rename-error-small-{tn}"));
    }

    // New+.
    let tdir = tmp.path().join("Templates");
    crate::newplus::ensure_templates(&tdir).unwrap();
    std::fs::write(tdir.join("Budget.csv"), "a,b\n").unwrap();
    let target = tmp.path().join("Holiday 2024");
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let mut app = NewApp::with_templates_dir(target.clone(), tdir.clone(), Config::default());
        app.test_mode();
        let mut h = Harness::builder()
            .with_size(win(480.0, 560.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut NewApp| app.draw(ui), app);
        h.state_mut().select(2);
        save(&mut h, &dir, &format!("newplus-{tn}"));
        h.state_mut().set_error("Permission denied (os error 13)");
        save(&mut h, &dir, &format!("newplus-error-{tn}"));
        let empty = tmp.path().join("EmptyTemplates");
        std::fs::create_dir_all(&empty).unwrap();
        let mut app = NewApp::with_templates_dir(target.clone(), empty, Config::default());
        app.test_mode();
        let mut h = Harness::builder()
            .with_size(win(480.0, 420.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut NewApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("newplus-empty-{tn}"));
    }

    // Image Resizer.
    let pics = pictures(tmp.path());
    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let mut app = ResizeApp::new(pics.clone(), Some(cfg.clone()));
        app.test_mode();
        let mut h = Harness::builder()
            .with_size(win(960.0, 760.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut ResizeApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("resize-presets-{tn}"));
        h.state_mut().select(4);
        h.state_mut().set_custom(|c| {
            c.width = 1200.0;
            c.height = 0.0;
            c.fit = ResizeFit::Fit;
        });
        h.state_mut().set_opts(|o| {
            o.format = OutputFormat::Jpeg;
            o.jpeg_quality = 82;
            o.remove_gps = true;
            o.file_name_format = "%1 (%5 × %6)".into();
        });
        save(&mut h, &dir, &format!("resize-custom-{tn}"));
        h.state_mut().set_custom(|c| {
            c.width = 0.0;
            c.height = 0.0;
            c.unit = ResizeUnit::Centimeter;
        });
        save(&mut h, &dir, &format!("resize-custom-invalid-{tn}"));
        let n = h.state().file_count();
        let ok = |i: usize| -> Option<crate::resize_ui::FileResult> {
            let f = &pics[i];
            let stem = f.file_stem().unwrap().to_string_lossy().to_string();
            Some(Ok((
                f.with_file_name(format!("{stem} (Small).jpg")),
                (854, 480),
            )))
        };
        let mut running: Vec<_> = (0..n).map(|_| None).collect();
        for (i, r) in running.iter_mut().enumerate().take(3) {
            *r = ok(i);
        }
        h.state_mut().set_running(3, running);
        save(&mut h, &dir, &format!("resize-progress-{tn}"));
        let mut done: Vec<_> = (0..n).map(ok).collect();
        done[4] = Some(Err("not an image (or an unsupported format)".into()));
        h.state_mut().set_finished(done);
        save(&mut h, &dir, &format!("resize-results-{tn}"));
        // Small window.
        let mut app = ResizeApp::new(pics.clone(), Some(cfg.clone()));
        app.test_mode();
        let mut h = Harness::builder()
            .with_size(win(820.0, 560.0))
            .with_theme(theme)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(|ui, app: &mut ResizeApp| app.draw(ui), app);
        save(&mut h, &dir, &format!("resize-small-{tn}"));
    }
}

/// Real pictures for the Image Resizer (generated with ImageIO on macOS).
fn pictures(root: &Path) -> Vec<PathBuf> {
    let d = root.join("Pictures");
    std::fs::create_dir_all(&d).unwrap();
    let mut v = Vec::new();
    for (i, (w, h)) in [
        (4032, 3024),
        (3024, 4032),
        (1920, 1080),
        (800, 600),
        (640, 480),
        (2048, 1536),
    ]
    .into_iter()
    .enumerate()
    {
        let p = d.join(format!("IMG_{:04}.jpg", 4100 + i));
        #[cfg(target_os = "macos")]
        crate::resize::mac::write_test_image(&p, w / 8, h / 8, OutputFormat::Jpeg, 1, i == 0, 72.0)
            .unwrap();
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (w, h);
            std::fs::write(&p, "x").unwrap();
        }
        v.push(p);
    }
    v
}

/// Resize through the window state (no GPU needed).
#[cfg(target_os = "macos")]
#[test]
fn resize_window_runs_and_remembers() {
    let tmp = tempfile::tempdir().unwrap();
    let pics = pictures(tmp.path());
    let cfg = tmp.path().join("ft.json");
    let ctx = egui::Context::default();
    // A folder expands to its pictures.
    let mut app = ResizeApp::new(vec![tmp.path().join("Pictures")], Some(cfg.clone()));
    app.test_mode();
    assert_eq!(app.file_count(), pics.len());
    app.select(4);
    app.set_custom(|c| {
        c.width = 50.0;
        c.height = 0.0;
        c.unit = ResizeUnit::Percent;
    });
    app.set_opts(|o| o.format = OutputFormat::Png);
    let r = app.run_now(&ctx);
    assert_eq!(r.len(), pics.len());
    for (res, f) in r.iter().zip(&pics) {
        let (out, (w, h)) = res.clone().unwrap().unwrap();
        let src = crate::resize::image_info(f).unwrap();
        let half = |v: u32| (f64::from(v) / 2.0).round_ties_even() as u32;
        assert_eq!((w, h), (half(src.width), half(src.height)));
        assert_eq!(out.extension().unwrap(), "png");
        assert!(out
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("(Custom)"));
    }
    // The choice is remembered.
    let c = Config::load_from(&cfg);
    assert_eq!(c.resize.selected, 4);
    assert_eq!(c.resize.custom.unit, ResizeUnit::Percent);
    assert_eq!(c.resize.options.format, OutputFormat::Png);
    let app2 = ResizeApp::new(pics.clone(), Some(cfg.clone()));
    assert_eq!(app2.file_count(), pics.len());
}

/// Apply and undo through the window state (no GPU needed).
#[test]
fn window_apply_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = sample(tmp.path());
    let journal = tmp.path().join("undo.json");
    let cfg = tmp.path().join("ft.json");
    let ctx = egui::Context::default();
    let mut app = RenameApp::new(paths.clone(), Some(cfg.clone()), Some(journal.clone()));
    app.set_options(|o| {
        o.search = "IMG_".into();
        o.replace = "Trip ".into();
    });
    assert_eq!(app.plan_for_test().rename_count, 7);
    app.apply_now(&ctx);
    let w = tmp.path().join("Holiday 2024");
    assert!(w.join("Trip 0001.JPG").exists());
    assert!(w.join("Raw files/Trip 0002.dng").exists());
    // Options and MRU were saved.
    let c = Config::load_from(&cfg);
    assert_eq!(c.rename.search_mru, vec!["IMG_"]);
    assert_eq!(c.rename.options.replace, "Trip ");
    // The list follows the renames: nothing left to rename.
    assert_eq!(app.plan_for_test().rename_count, 0);
    let rep = crate::rename::undo_last(&journal).unwrap();
    assert_eq!(rep.renamed, 7);
    assert!(w.join("IMG_0001.JPG").exists());
}

#[test]
fn window_creates_from_template() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("dest");
    std::fs::create_dir(&target).unwrap();
    let ctx = egui::Context::default();
    let mut app =
        NewApp::with_templates_dir(target.clone(), tmp.path().join("T"), Config::default());
    app.test_mode();
    app.select(2); // Markdown.md
    assert_eq!(app.name(), "Markdown.md");
    assert_eq!(app.create_now(&ctx), Some(target.join("Markdown.md")));
    assert_eq!(app.create_now(&ctx), Some(target.join("Markdown 2.md")));
}
