//! Render the Dock preview panel offscreen to PNGs (no screen capture):
//!
//! ```sh
//! cargo run -p dock-previews --example render_preview -- /tmp/out-dir
//! ```
//!
//! Produces `bottom.png` (thumbnails, a hovered tile with its buttons, a
//! minimized window), `selected.png` (scroll selection), `icons.png` (the
//! no-Screen-Recording fallback), `single.png`, `side.png` (Dock on the
//! left: a column) and `many.png`.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use std::collections::HashMap;

    use dock_previews::geom::{Orientation, RectF};
    use dock_previews::layout::{Hit, Layout};
    use dock_previews::preview::{fake_window_image, render_png, Model, TileView};
    use dock_previews::PreviewSize;

    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir)?;

    type Win = (&'static str, f64, Option<&'static str>, (f64, f64, f64), bool);
    let wins: [Win; 6] = [
        ("Meeting notes.rtf", 1.30, None, (0.25, 0.55, 0.95), false),
        ("Quarterly plan — draft 3.txt", 1.55, None, (0.30, 0.80, 0.45), true),
        ("Groceries.txt", 0.95, Some("Minimized"), (0.95, 0.75, 0.20), false),
        ("Untitled 4", 1.45, Some("Other Space"), (0.95, 0.30, 0.30), false),
        ("Reading list.rtf", 1.60, None, (0.55, 0.45, 0.95), true),
        ("A very long document title that will certainly need truncating.txt", 1.20, None, (0.2, 0.7, 0.7), false),
    ];
    let make = |n: usize, size: PreviewSize, orientation: Orientation, icon_mode: bool, hover: Hit, selected: Option<usize>| {
        let items: Vec<TileView> = (0..n)
            .map(|i| {
                let w = &wins[i % wins.len()];
                TileView { wid: i as u32 + 1, title: w.0.into(), aspect: w.1, badge: w.2, dimmed: w.2 == Some("Minimized") }
            })
            .collect();
        let aspects: Vec<f64> = items.iter().map(|i| i.aspect).collect();
        let layout = Layout::compute(&aspects, size, orientation, 1480.0, 860.0);
        let mut images = HashMap::new();
        if !icon_mode {
            for (i, it) in items.iter().enumerate() {
                // The "Other Space" window has no thumbnail yet: placeholder.
                if i % wins.len() == 3 {
                    continue;
                }
                let w = &wins[i % wins.len()];
                let px = 560usize;
                if let Some(img) = fake_window_image(px, (px as f64 / it.aspect) as usize, w.3, w.4) {
                    images.insert(it.wid, img);
                }
            }
        }
        let frame = RectF::new(0.0, 0.0, layout.w, layout.h);
        (
            Model {
                pid: -1,
                app_name: "TextEdit".into(),
                app_path: "/System/Applications/TextEdit.app".into(),
                items,
                layout,
                frame,
                hover,
                selected,
                icon_mode,
            },
            images,
        )
    };

    let shots = [
        ("bottom", make(4, PreviewSize::Medium, Orientation::Bottom, false, Hit::Close(1), None)),
        ("selected", make(4, PreviewSize::Medium, Orientation::Bottom, false, Hit::None, Some(0))),
        ("icons", make(4, PreviewSize::Medium, Orientation::Bottom, true, Hit::Tile(0), None)),
        ("single", make(1, PreviewSize::Large, Orientation::Bottom, false, Hit::Minimize(0), None)),
        ("side", make(3, PreviewSize::Small, Orientation::Left, false, Hit::Tile(2), None)),
        ("many", make(11, PreviewSize::Small, Orientation::Bottom, false, Hit::Tile(5), None)),
    ];
    for (name, (model, images)) in shots {
        let path = format!("{dir}/{name}.png");
        render_png(&model, &images, &path, 2.0)?;
        println!("wrote {path} ({}x{} pt)", model.layout.w, model.layout.h);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
