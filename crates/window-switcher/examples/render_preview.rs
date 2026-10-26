//! Render the switcher overlay offscreen to PNGs (no screen capture needed):
//!
//! ```sh
//! cargo run -p window-switcher --example render_preview -- /tmp/out-dir
//! ```
//!
//! Produces `thumbnails.png` (synthetic window thumbnails), `icons.png`
//! (the no-Screen-Recording fallback), `many.png` and `empty.png`.

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
        ("/System/Applications/Notes.app", "Groceries", 1.25, Some("Other Space"), (0.95, 0.75, 0.20), false),
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
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
