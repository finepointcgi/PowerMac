//! Offscreen screenshots of the editor (egui_kittest + wgpu). Run with
//! `PM_UI_SHOTS=<dir> cargo test -p workspaces shots` to write PNGs into
//! `<dir>`; skipped by default and when no GPU adapter is available.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::Harness;

use crate::editor::{EditorApp, LaunchUi};
use crate::geometry::{Monitor, Rect};
use crate::launch::AppStatus;
use crate::model::*;
use crate::Config;

const MARGIN: f32 = 8.0;
const NOW: i64 = 1_760_000_000;

fn win(w: f32, h: f32) -> [f32; 2] {
    [w + 2.0 * MARGIN, h + 2.0 * MARGIN]
}

fn save(h: &mut Harness<'_, impl Sized>, dir: &Option<PathBuf>, name: &str) {
    h.run_steps(6);
    match h.render() {
        Ok(img) => {
            if let Some(d) = dir {
                std::fs::create_dir_all(d).ok();
                let (w, hh) = img.dimensions();
                let img = image::imageops::crop_imm(&img, MARGIN as u32, MARGIN as u32, w - 2 * MARGIN as u32, hh - 2 * MARGIN as u32).to_image();
                img.save(d.join(format!("{name}.png"))).expect("save png");
            }
        }
        Err(e) => eprintln!("render {name} skipped: {e}"),
    }
}

fn has_gpu() -> bool {
    let mut h = Harness::builder().with_size([64.0, 64.0]).wgpu().build_ui(|ui| {
        ui.label("x");
    });
    h.run();
    h.render().is_ok()
}

fn monitors() -> Vec<Monitor> {
    vec![
        Monitor { id: "0610-A061-FD626D62".into(), name: "Built-in Retina Display".into(), number: 0, frame: Rect::new(0, 0, 1512, 982), work_area: Rect::new(0, 33, 1512, 885), scale: 2.0, primary: true },
        Monitor { id: "1E6D-5B11-0003C2A1".into(), name: "LG UltraFine".into(), number: 1, frame: Rect::new(1512, -300, 2560, 1440), work_area: Rect::new(1512, -275, 2560, 1415), scale: 1.0, primary: false },
    ]
}

fn app(name: &str, bundle: &str, path: &str, frame: Rect, monitor: &str, state: WindowState, title: &str) -> App {
    App {
        id: format!("{bundle}-{}-{}", frame.x, frame.y),
        name: name.into(),
        bundle_id: bundle.into(),
        path: path.into(),
        title: title.into(),
        frame,
        monitor_id: monitor.into(),
        state,
        ..Default::default()
    }
}

fn sample(dir: &Path) -> Vec<Workspace> {
    let [a, b] = [monitors()[0].id.clone(), monitors()[1].id.clone()];
    let mut coding = Workspace {
        id: "{C0D1}".into(),
        name: "Coding".into(),
        creation_time: NOW - 86_400 * 12,
        last_launched_time: Some(NOW - 3600 * 3),
        move_existing_windows: true,
        monitors: monitors(),
        apps: vec![
            app("Visual Studio Code", "com.microsoft.VSCode", "/Applications/Visual Studio Code.app", Rect::new(1512, -275, 1700, 1415), &b, WindowState::Normal, "main.rs — PowerMac"),
            app("Terminal", "com.apple.Terminal", "/System/Applications/Utilities/Terminal.app", Rect::new(3212, -275, 860, 900), &b, WindowState::Normal, "~/PowerMac — zsh"),
            app("Safari", "com.apple.Safari", "/Applications/Safari.app", Rect::new(3212, 625, 860, 515), &b, WindowState::Normal, "docs.rs"),
            app("Notes", "com.apple.Notes", "/System/Applications/Notes.app", Rect::new(0, 33, 1512, 885), &a, WindowState::Zoomed, "Ideas"),
            app("Music", "com.apple.Music", "/System/Applications/Music.app", Rect::new(200, 200, 900, 600), &a, WindowState::Minimized, "Music"),
        ],
        ..Default::default()
    };
    coding.apps[1].document = Some("/Users/me/PowerMac".into());
    let writing = Workspace {
        id: "{W817}".into(),
        name: "Writing".into(),
        creation_time: NOW - 86_400 * 30,
        last_launched_time: Some(NOW - 86_400 * 2),
        monitors: vec![monitors()[0].clone()],
        apps: vec![
            {
                let mut t = app("TextEdit", "com.apple.TextEdit", "/System/Applications/TextEdit.app", Rect::new(0, 33, 900, 885), &a, WindowState::Normal, "chapter-3.rtf");
                t.document = Some("/Users/me/Documents/Book/chapter-3.rtf".into());
                t
            },
            app("Preview", "com.apple.Preview", "/System/Applications/Preview.app", Rect::new(900, 33, 612, 885), &a, WindowState::Normal, "outline.pdf"),
        ],
        ..Default::default()
    };
    let meeting = Workspace {
        id: "{MEE7}".into(),
        name: "Monday stand-up".into(),
        creation_time: NOW - 86_400 * 5,
        last_launched_time: None,
        other_windows: OtherWindows::Minimize,
        monitors: monitors(),
        apps: vec![
            app("Calendar", "com.apple.iCal", "/System/Applications/Calendar.app", Rect::new(1512, -275, 1280, 1415), &b, WindowState::Normal, "Calendar"),
            app("Messages", "com.apple.MobileSMS", "/System/Applications/Messages.app", Rect::new(2792, -275, 1280, 700), &b, WindowState::Normal, "Messages"),
            app("Mail", "com.apple.mail", "/System/Applications/Mail.app", Rect::new(2792, 425, 1280, 715), &b, WindowState::Normal, "Inbox"),
            app("Reminders", "com.apple.reminders", "/System/Applications/Reminders.app", Rect::new(100, 120, 700, 600), &a, WindowState::Normal, "Today"),
        ],
        ..Default::default()
    };
    let design = Workspace {
        id: "{DE51}".into(),
        name: "Photo editing".into(),
        creation_time: NOW - 86_400 * 60,
        last_launched_time: Some(NOW - 86_400 * 40),
        monitors: vec![monitors()[1].clone()],
        apps: vec![
            app("Photos", "com.apple.Photos", "/System/Applications/Photos.app", Rect::new(1512, -275, 2560, 1415), &b, WindowState::FullScreen, "Library"),
            app("Finder", "com.apple.finder", "/System/Library/CoreServices/Finder.app", Rect::new(1700, 0, 900, 600), &b, WindowState::Normal, "Exports"),
        ],
        ..Default::default()
    };
    let list = vec![coding, writing, meeting, design];
    crate::store::save(dir, &list).unwrap();
    list
}

fn harness<'a>(app: EditorApp, w: f32, h: f32, theme: egui::Theme) -> Harness<'a, EditorApp> {
    Harness::builder()
        .with_size(win(w, h))
        .with_theme(theme)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app)
}

fn editor(dir: &Path) -> EditorApp {
    let mut e = EditorApp::new(dir.to_path_buf(), Config::default());
    e.test_setup(monitors(), true, NOW, true);
    e
}

#[test]
fn shots() {
    if std::env::var_os("PM_UI_SHOTS").is_none() && std::env::var_os("PM_UI_SHOTS_ALWAYS").is_none() {
        return;
    }
    if !has_gpu() {
        return;
    }
    let out = std::env::var_os("PM_UI_SHOTS").map(PathBuf::from);
    let root = std::env::temp_dir().join(format!("ws-shots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    let empty = root.join("empty");
    let list = sample(&data);
    let before = std::fs::read_to_string(crate::store::path_in(&data)).unwrap();

    for (theme, tn) in [(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")] {
        let mut h = harness(editor(&data), 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("list-{tn}"));

        let mut h = harness(editor(&empty), 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("empty-{tn}"));

        let mut e = editor(&data);
        e.test_edit("{C0D1}", Some(0));
        let mut h = harness(e, 1000.0, 1180.0, theme);
        save(&mut h, &out, &format!("edit-{tn}"));

        let mut e = editor(&data);
        e.test_edit("{C0D1}", Some(4));
        let mut h = harness(e, 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("edit-window-{tn}"));

        let mut e = editor(&data);
        let mut captured = list[2].clone();
        captured.id = "{NEW}".into();
        captured.name = "Workspace 5".into();
        captured.apps.extend(list[1].apps.iter().cloned());
        e.test_new_draft(captured, &[4, 5]);
        e.test_status("Captured 6 windows. Turn off what you don't need, then save.", false);
        let mut h = harness(e, 1000.0, 760.0, theme);
        save(&mut h, &out, &format!("capture-{tn}"));

        let mut e = editor(&data);
        e.test_launch_progress(LaunchUi {
            name: "Coding".into(),
            rows: vec![
                ("Visual Studio Code".into(), AppStatus::Placed, Some("moved its open window".into())),
                ("Terminal".into(), AppStatus::Placed, None),
                ("Safari".into(), AppStatus::Waiting, None),
                ("Notes".into(), AppStatus::Launching, None),
                ("Music".into(), AppStatus::Failed, Some("no window appeared within 15 s".into())),
            ],
            apps: list[0].apps.clone(),
            ..Default::default()
        });
        let mut h = harness(e, 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("launching-{tn}"));

        let mut e = editor(&data);
        e.test_confirm_delete("{W817}");
        let mut h = harness(e, 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("delete-{tn}"));

        let mut e = editor(&data);
        e.test_import_dialog("");
        let mut h = harness(e, 1000.0, 700.0, theme);
        save(&mut h, &out, &format!("import-{tn}"));

        let mut e = editor(&data);
        e.test_edit("{W817}", Some(0));
        let mut h = harness(e, 720.0, 900.0, theme);
        save(&mut h, &out, &format!("edit-narrow-{tn}"));

        let mut e = editor(&data);
        e.test_setup(monitors(), false, NOW, true);
        e.test_search("co");
        let mut h = harness(e, 760.0, 600.0, theme);
        save(&mut h, &out, &format!("list-narrow-noperm-{tn}"));
    }
    // Looking doesn't write anything.
    assert_eq!(std::fs::read_to_string(crate::store::path_in(&data)).unwrap(), before);
    assert!(!crate::store::path_in(&empty).exists());
    let _ = std::fs::remove_dir_all(&root);
}
