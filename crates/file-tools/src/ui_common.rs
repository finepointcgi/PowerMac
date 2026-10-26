//! Bits shared by the PowerRename and New+ windows: extra Phosphor icons,
//! file-type icons and tints, the app icon and bringing the window forward.

use eframe::egui::{self, Color32};

/// Phosphor (regular) code points not in `pm_ui::theme::icon`, checked
/// against the bundled font.
#[allow(dead_code)]
pub mod icon {
    pub const FILE: &str = "\u{E230}";
    pub const FILE_TEXT: &str = "\u{E23A}";
    pub const FILE_PLUS: &str = "\u{E236}";
    pub const FILE_CODE: &str = "\u{E914}";
    pub const FILE_HTML: &str = "\u{EB6C}";
    pub const FOLDER: &str = "\u{E24A}";
    pub const FOLDER_PLUS: &str = "\u{E258}";
    pub const FOLDERS: &str = "\u{E260}";
    pub const FILES: &str = "\u{E710}";
    pub const CODE: &str = "\u{E1BC}";
    pub const TERMINAL: &str = "\u{EAE8}";
    pub const MARKDOWN: &str = "\u{E508}";
    pub const TEXT_AA: &str = "\u{E6EE}";
    pub const HASH: &str = "\u{E2A2}";
    pub const SHUFFLE: &str = "\u{E422}";
    pub const CALENDAR: &str = "\u{E108}";
    pub const FUNNEL: &str = "\u{E266}";
    pub const ARROW_RIGHT: &str = "\u{E06C}";
    pub const GLOBE: &str = "\u{E288}";
    pub const LIST_NUMBERS: &str = "\u{E2F4}";
    pub const ARTICLE: &str = "\u{E0A8}";
    pub const CHECK_SQUARE: &str = "\u{E186}";
    pub const SQUARE: &str = "\u{E45E}";
    pub const CLOCK_CCW: &str = "\u{E1A0}";
    pub const TREE: &str = "\u{E67C}";
    pub const BRACKETS: &str = "\u{E860}";
    pub const NOTEPAD: &str = "\u{E63E}";
    pub const X_CIRCLE: &str = "\u{E4F8}";
    pub const IMAGE: &str = "\u{E2CA}";
    pub const IMAGES: &str = "\u{E836}";
    pub const DEVICE_MOBILE: &str = "\u{E1E0}";
    pub const TELEVISION_SIMPLE: &str = "\u{EAE6}";
    pub const RULER: &str = "\u{E6B8}";
    pub const ARROWS_OUT: &str = "\u{E0A2}";
    pub const ARROWS_IN_SIMPLE: &str = "\u{E09E}";
    pub const CROP: &str = "\u{E1D4}";
    pub const MAP_PIN: &str = "\u{E316}";
    pub const EJECT: &str = "\u{E212}";
    pub const HARD_DRIVE: &str = "\u{E29E}";
    pub const SPINNER_GAP: &str = "\u{E66C}";
    pub const FRAME_CORNERS: &str = "\u{E626}";
    pub const PERCENT: &str = "\u{E3B6}";
}

/// Tile colors (macOS system colors).
#[allow(dead_code)]
pub mod tint {
    use super::Color32;
    pub const BLUE: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);
    pub const INDIGO: Color32 = Color32::from_rgb(0x5E, 0x5C, 0xE6);
    pub const PURPLE: Color32 = Color32::from_rgb(0xAF, 0x52, 0xDE);
    pub const ORANGE: Color32 = Color32::from_rgb(0xFF, 0x95, 0x00);
    pub const GREEN: Color32 = Color32::from_rgb(0x34, 0xC7, 0x59);
    pub const TEAL: Color32 = Color32::from_rgb(0x30, 0xB0, 0xC7);
    pub const RED: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x30);
    pub const GRAY: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x93);
    pub const PINK: Color32 = Color32::from_rgb(0xFF, 0x2D, 0x55);
    pub const SKY: Color32 = Color32::from_rgb(0x64, 0xA8, 0xF0);
}

/// Icon and tile color for a file with extension `ext` (lowercase, no dot),
/// or a folder.
pub fn file_icon(ext: &str, is_dir: bool) -> (&'static str, Color32) {
    if is_dir {
        return (icon::FOLDER, tint::SKY);
    }
    match ext {
        "txt" | "text" | "log" => (icon::FILE_TEXT, tint::GRAY),
        "md" | "markdown" => (icon::MARKDOWN, tint::INDIGO),
        "rtf" | "rtfd" | "doc" | "docx" | "pages" | "odt" => (icon::ARTICLE, tint::BLUE),
        "sh" | "zsh" | "bash" | "command" | "ps1" | "bat" | "cmd" => (icon::TERMINAL, tint::GREEN),
        "html" | "htm" | "xhtml" => (icon::FILE_HTML, tint::ORANGE),
        "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "swift" | "go" | "java" | "css"
        | "json" | "toml" | "yaml" | "yml" | "xml" => (icon::FILE_CODE, tint::PURPLE),
        "csv" | "xls" | "xlsx" | "numbers" => (icon::LIST_NUMBERS, tint::GREEN),
        "jpg" | "jpeg" | "png" | "heic" | "heif" | "gif" | "webp" | "tif" | "tiff" | "bmp"
        | "dng" | "avif" => (icon::IMAGE, tint::TEAL),
        _ => (icon::FILE, tint::GRAY),
    }
}

/// Make the process a regular foreground app so its window comes to the
/// front (it's started from Finder or the tray).
pub fn become_foreground_app() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

/// A 64x64 window icon: a rounded tile in `color` with a white glyph-like
/// mark (two lines for rename, a plus for New+).
pub fn app_icon(color: [u8; 3], plus: bool) -> egui::IconData {
    app_icon_with(color, if plus { Mark::Plus } else { Mark::Lines })
}

/// The white mark on an [`app_icon_with`] tile.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Lines,
    Plus,
    /// Two nested frames (Image Resizer).
    Frames,
}

/// [`app_icon`] with any [`Mark`].
pub fn app_icon_with(color: [u8; 3], mark: Mark) -> egui::IconData {
    const N: usize = 64;
    let mut px = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            // Rounded square 4..60 with radius 12.
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (cx, cy) = (fx.clamp(16.0, 48.0), fy.clamp(16.0, 48.0));
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            if d > 12.0 {
                continue;
            }
            let frame = |x0: f32, y0: f32, x1: f32, y1: f32| {
                (x0..x1).contains(&fx)
                    && (y0..y1).contains(&fy)
                    && !((x0 + 4.0..x1 - 4.0).contains(&fx) && (y0 + 4.0..y1 - 4.0).contains(&fy))
            };
            let white = match mark {
                Mark::Plus => {
                    ((29.0..35.0).contains(&fx) && (18.0..46.0).contains(&fy))
                        || ((18.0..46.0).contains(&fx) && (29.0..35.0).contains(&fy))
                }
                Mark::Lines => {
                    ((16.0..48.0).contains(&fx) && (20.0..26.0).contains(&fy))
                        || ((16.0..38.0).contains(&fx) && (38.0..44.0).contains(&fy))
                        || ((42.0..46.0).contains(&fx) && (34.0..48.0).contains(&fy))
                }
                Mark::Frames => frame(14.0, 14.0, 50.0, 50.0) || frame(14.0, 30.0, 34.0, 50.0),
            };
            let i = (y * N + x) * 4;
            let c = if white { [255, 255, 255] } else { color };
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    egui::IconData {
        rgba: px,
        width: N as u32,
        height: N as u32,
    }
}

/// `~/…` for paths under the home folder.
pub fn pretty_path(p: &std::path::Path) -> String {
    if let Some(h) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&h) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    p.display().to_string()
}
