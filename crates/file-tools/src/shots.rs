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
