//! Render the switcher overlay offscreen to PNGs (no screen capture needed):
//!
//! ```sh
//! cargo run -p window-switcher --example render_preview -- /tmp/out-dir
//! ```
//!
//! Produces `thumbnails.png` (synthetic window thumbnails), `icons.png`
//! (the no-Screen-Recording fallback), `many.png` and `empty.png`, plus
//! whole-screen scenes with the selected-window preview: `scene-real.png`
//! (visible window at its real position, background dimmed),
//! `scene-minimized.png` / `scene-other-space.png` (centered with a badge),
//! `scene-outline.png` (no Screen Recording: outline + icon + title),
//! `scene-loading.png` (capture pending) and `scene-no-dim.png`.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use std::collections::HashMap;
    use window_switcher::layout::{self, LayoutParams};
    use window_switcher::model::RectF;
    use window_switcher::preview::{fake_window_image, render_png, ItemView, Model};

    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir)?;
    let screen = RectF::new(0.0, 0.0, 1512.0, 982.0);

    type App = (&'static str, &'static str, f64, Option<&'static str>, (f64, f64, f64), bool);
    let apps: [App; 7] = [
        ("/System/Applications/TextEdit.app", "Meeting notes.rtf", 1.30, None, (0.25, 0.55, 0.95), false),
        ("/Applications/Safari.app", "Apple Developer Documentation — ScreenCaptureKit", 1.60, None, (0.20, 0.60, 0.90), false),
        ("/System/Applications/Utilities/Terminal.app", "~/PowerMac — cargo test — 120×40", 1.45, None, (0.30, 0.80, 0.45), true),
        ("/System/Library/CoreServices/Finder.app", "Downloads", 1.50, Some("Minimized"), (0.55, 0.55, 0.60), false),
        ("/System/Applications/Notes.app", "Groceries", 1.25, Some("Other Desktop"), (0.95, 0.75, 0.20), false),
        ("/System/Applications/Calendar.app", "Calendar", 1.55, Some("Hidden"), (0.95, 0.30, 0.30), false),
        ("/System/Applications/Mail.app", "Inbox — 3 unread", 1.70, None, (0.25, 0.50, 0.95), false),
    ];
    let make = |n: usize, icon_mode: bool, with_images: bool, selected: usize| {
        let items: Vec<ItemView> = (0..n)
            .map(|i| {
                let a = &apps[i % apps.len()];
                ItemView {
                    wid: i as u32 + 1,
                    pid: -1,
                    title: a.1.into(),
                    app_path: a.0.into(),
                    aspect: a.2,
                    badge: a.3,
                    dimmed: matches!(a.3, Some("Minimized") | Some("Hidden")),
                }
            })
            .collect();
        let aspects: Vec<f64> = items.iter().map(|i| if icon_mode { 1.0 } else { i.aspect }).collect();
        let p = if icon_mode { LayoutParams::ICONS } else { LayoutParams::THUMBNAILS };
        let layout = layout::compute(&aspects, screen, &p);
        let mut images = HashMap::new();
        if with_images {
            for (i, it) in items.iter().enumerate() {
                // Leave one tile without a thumbnail to show the placeholder.
                if i == 4 {
                    continue;
                }
                let a = &apps[i % apps.len()];
                let w = 640usize;
                let h = (w as f64 / it.aspect) as usize;
                if let Some(img) = fake_window_image(w, h, a.4, a.5) {
                    images.insert(it.wid, img);
                }
            }
        }
        (Model { items, layout, selected, icon_mode }, images)
    };

    for (name, n, icon_mode, imgs, sel) in
        [("thumbnails", 7, false, true, 1), ("icons", 7, true, false, 1), ("many", 22, false, true, 9), ("empty", 0, false, false, 0)]
    {
        let (model, images) = make(n, icon_mode, imgs, sel);
        let path = format!("{dir}/{name}.png");
        render_png(&model, &images, &path, 2.0)?;
        println!("wrote {path}");
    }

    // ---- Whole-screen scenes with the preview of the selected window.
    use window_switcher::model::WindowInfo;
    use window_switcher::placement::place;
    use window_switcher::preview::{render_scene_png, DesktopWindow, PreviewView};
    let desk = [
        // (frame, tint, dark)
        (RectF::new(60.0, 70.0, 860.0, 600.0), (0.25, 0.55, 0.95), false),
        (RectF::new(520.0, 180.0, 900.0, 640.0), (0.20, 0.60, 0.90), false),
        (RectF::new(240.0, 330.0, 760.0, 520.0), (0.30, 0.80, 0.45), true),
    ];
    let desktop: Vec<DesktopWindow> = desk
        .iter()
        .filter_map(|(f, t, d)| fake_window_image((f.w * 2.0) as usize, (f.h * 2.0) as usize, *t, *d).map(|image| DesktopWindow { frame: *f, image }))
        .collect();
    type Scene = (&'static str, usize, Option<RectF>, bool, bool, bool, Option<f64>);
    let scenes: [Scene; 6] = [
        // name, selected tile, real frame (None = use WindowInfo flags), minimized, other space, outline, dim
        ("scene-real", 1, Some(desk[1].0), false, false, false, Some(0.25)),
        ("scene-minimized", 3, None, true, false, false, Some(0.25)),
        ("scene-other-space", 4, None, false, true, false, Some(0.25)),
        ("scene-outline", 1, Some(desk[1].0), false, false, true, Some(0.25)),
        ("scene-loading", 0, Some(desk[0].0), false, false, false, Some(0.25)),
        ("scene-no-dim", 1, Some(desk[1].0), false, false, false, None),
    ];
    for (name, sel, frame, minimized, other_space, outline, dim) in scenes {
        let (mut model, images) = make(7, outline, !outline, sel);
        model.selected = sel;
        let a = &apps[sel % apps.len()];
        let info = WindowInfo {
            id: sel as u64 + 1,
            pid: -1,
            title: a.1.into(),
            app_path: a.0.into(),
            frame: frame.unwrap_or(RectF::new(300.0, 120.0, 1000.0, 700.0)),
            minimized,
            other_space,
            ..Default::default()
        };
        let placement = place(&info, screen, &[screen], false);
        let view = PreviewView {
            wid: info.id as u32,
            pid: -1,
            title: info.title.clone(),
            app_path: info.app_path.clone(),
            placement,
            dim,
            outline,
            avoid: Some(model.layout.panel),
        };
        let img = if outline || name == "scene-loading" {
            None
        } else {
            let f = info.frame;
            fake_window_image((f.w * 2.0) as usize, (f.h * 2.0) as usize, a.4, a.5)
        };
        let path = format!("{dir}/{name}.png");
        render_scene_png(&model, &images, Some((&view, img.as_ref())), &desktop, screen, &path, 2.0)?;
        println!("wrote {path}");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
