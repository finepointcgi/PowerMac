//! What is on the clipboard: the formats PowerMac keeps, the app it came
//! from, and the privacy markers that make it skip an item.

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Clipboard contents in the formats the history keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Content {
    /// Plain text (always kept when there is any text).
    pub text: Option<String>,
    /// HTML (macOS `public.html`; Windows `HTML Format`, CF_HTML with its
    /// header, kept verbatim).
    pub html: Option<String>,
    /// RTF bytes.
    pub rtf: Option<Vec<u8>>,
    /// An image, as PNG.
    pub png: Option<Vec<u8>>,
    /// Copied files / folders (absolute paths).
    pub files: Vec<String>,
}

/// The type of a history item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Text,
    /// Text with HTML / RTF formatting (and a plain-text fallback).
    RichText,
    Image,
    Files,
}

impl Content {
    /// Plain text only.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: Some(s.into()),
            ..Default::default()
        }
    }

    /// The text, if it has any non-whitespace.
    pub fn plain(&self) -> Option<&str> {
        self.text.as_deref().filter(|t| !t.trim().is_empty())
    }

    /// What kind of item this becomes (`None`: nothing worth keeping).
    /// Files win over text (Finder also puts the names on the clipboard),
    /// text over images (Office / browsers add a picture of the selection).
    pub fn kind(&self) -> Option<Kind> {
        if !self.files.is_empty() {
            Some(Kind::Files)
        } else if self.plain().is_some() {
            Some(if self.html.is_some() || self.rtf.is_some() {
                Kind::RichText
            } else {
                Kind::Text
            })
        } else if self.png.is_some() {
            Some(Kind::Image)
        } else {
            None
        }
    }

    /// Approximate size in bytes.
    pub fn size(&self) -> u64 {
        (self.text.as_ref().map_or(0, |t| t.len())
            + self.html.as_ref().map_or(0, |t| t.len())
            + self.rtf.as_ref().map_or(0, |t| t.len())
            + self.png.as_ref().map_or(0, |t| t.len())
            + self.files.iter().map(|f| f.len()).sum::<usize>()) as u64
    }

    /// Drop the formats the config doesn't keep; returns false if nothing
    /// is left.
    pub fn restrict(&mut self, cfg: &Config) -> bool {
        if !cfg.capture_files {
            self.files.clear();
        }
        if !cfg.capture_rich_text {
            self.html = None;
            self.rtf = None;
        }
        if !cfg.capture_images {
            self.png = None;
        }
        match self.kind() {
            Some(Kind::Files) => {
                self.html = None;
                self.rtf = None;
                self.png = None;
            }
            Some(Kind::Text | Kind::RichText) => {
                // The picture Office/browsers add of a text selection.
                self.png = None;
            }
            Some(Kind::Image) => {
                self.text = None;
                self.html = None;
                self.rtf = None;
            }
            None => return false,
        }
        true
    }

    /// The identity used for de-duplication: the same text copied from two
    /// apps (with different HTML around it) is one item.
    pub fn dedupe_hash(&self) -> u64 {
        let mut h = Fnv::new();
        match self.kind() {
            Some(Kind::Files) => {
                h.write(b"files\0");
                for f in &self.files {
                    h.write(f.as_bytes());
                    h.write(b"\0");
                }
            }
            Some(Kind::Text | Kind::RichText) => {
                h.write(b"text\0");
                h.write(self.text.as_deref().unwrap_or("").as_bytes());
            }
            Some(Kind::Image) => {
                h.write(b"png\0");
                h.write(self.png.as_deref().unwrap_or(&[]));
            }
            None => {}
        }
        h.finish()
    }
}

/// FNV-1a 64 (stable across Rust versions, unlike `DefaultHasher`, so
/// hashes can be persisted).
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub(crate) fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

/// The app that owned the clipboard / was in front when it changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceApp {
    /// Display name ("Safari").
    pub name: Option<String>,
    /// macOS bundle id / Windows exe name.
    pub id: Option<String>,
}

impl SourceApp {
    fn ids(&self) -> impl Iterator<Item = &str> {
        self.name.iter().chain(self.id.iter()).map(|s| s.as_str())
    }
}

/// One clipboard change as read from the OS.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    pub content: Content,
    /// Every type / format name present (used for privacy markers). On
    /// Windows `CanIncludeInClipboardHistory` appears as
    /// `CanIncludeInClipboardHistory=0` when it says no.
    pub types: Vec<String>,
    pub source: Option<SourceApp>,
}

/// Why a clipboard change was not recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Paused,
    /// Marked concealed / transient / auto-generated / excluded from history.
    Marked(String),
    /// Copied from an excluded app.
    ExcludedApp(String),
    TooLarge(u64),
    /// No format the history keeps.
    Empty,
}

/// Types that mean "don't record this" (nspasteboard.org conventions plus a
/// few app-specific ones, and Windows' clipboard-history opt-outs).
pub const PRIVATE_TYPES: &[&str] = &[
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.TransientType",
    "org.nspasteboard.AutoGeneratedType",
    "com.agilebits.onepassword",
    "de.petermaurer.TransientPasteboardType",
    "com.typeit4me.clipping",
    "Pasteboard generator type",
    "ExcludeClipboardContentFromMonitorProcessing",
    "CanIncludeInClipboardHistory=0",
];

/// Normalize an app identifier: lower case, file name only, no `.exe`/`.app`.
pub fn normalize_app(s: &str) -> String {
    keyboard_manager::engine::normalize_app(s)
}

/// Whether to skip a capture (checked before reading the heavy formats
/// where the platform allows).
pub fn skip_reason(types: &[String], source: Option<&SourceApp>, cfg: &Config) -> Option<Skip> {
    if cfg.paused {
        return Some(Skip::Paused);
    }
    if cfg.skip_concealed {
        if let Some(t) = types
            .iter()
            .find(|t| PRIVATE_TYPES.iter().any(|p| p.eq_ignore_ascii_case(t)))
        {
            return Some(Skip::Marked(t.clone()));
        }
    }
    if let Some(src) = source {
        let excluded: Vec<String> = cfg.excluded_apps.iter().map(|a| normalize_app(a)).collect();
        if let Some(id) = src.ids().find(|id| excluded.contains(&normalize_app(id))) {
            return Some(Skip::ExcludedApp(id.to_string()));
        }
    }
    None
}

/// Full check of a capture; on success the content is restricted to what
/// the config keeps.
pub fn filter(cap: &mut Capture, cfg: &Config) -> Result<(), Skip> {
    if let Some(s) = skip_reason(&cap.types, cap.source.as_ref(), cfg) {
        return Err(s);
    }
    if !cap.content.restrict(cfg) {
        return Err(Skip::Empty);
    }
    let size = cap.content.size();
    if size > cfg.max_item_bytes() {
        return Err(Skip::TooLarge(size));
    }
    Ok(())
}

/// Decode any image the `image` crate understands (PNG, TIFF, BMP) and
/// return it as PNG with its size. PNG input is passed through unchanged.
pub fn to_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let fmt = image::guess_format(bytes).ok()?;
    if fmt == image::ImageFormat::Png {
        let (w, h) = png_size(bytes)?;
        return Some((bytes.to_vec(), w, h));
    }
    let img = image::load_from_memory_with_format(bytes, fmt).ok()?;
    let (w, h) = (img.width(), img.height());
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some((out, w, h))
}

/// Width and height from a PNG header.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

/// A thumbnail (PNG) that fits in `max_w` x `max_h`.
pub fn thumbnail(png: &[u8], max_w: u32, max_h: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let t = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::new();
    t.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// TIFF bytes for an image (for apps that only read `public.tiff`).
pub fn png_to_tiff(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let mut out = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut out),
        image::ImageFormat::Tiff,
    )
    .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                128,
                255,
            ])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn kinds() {
        assert_eq!(Content::text("hi").kind(), Some(Kind::Text));
        assert_eq!(Content::text("  \n").kind(), None);
        let rich = Content {
            text: Some("hi".into()),
            html: Some("<b>hi</b>".into()),
            png: Some(tiny_png(2, 2)),
            ..Default::default()
        };
        assert_eq!(rich.kind(), Some(Kind::RichText));
        let files = Content {
            text: Some("a.txt".into()),
            files: vec!["/tmp/a.txt".into()],
            ..Default::default()
        };
        assert_eq!(files.kind(), Some(Kind::Files));
        let img = Content {
            png: Some(tiny_png(2, 2)),
            ..Default::default()
        };
        assert_eq!(img.kind(), Some(Kind::Image));
    }

    #[test]
    fn restrict_drops_unwanted_formats() {
        let mut c = Content {
            text: Some("hi".into()),
            html: Some("<b>hi</b>".into()),
            png: Some(tiny_png(2, 2)),
            ..Default::default()
        };
        assert!(c.restrict(&Config::default()));
        assert!(
            c.png.is_none() && c.html.is_some(),
            "selection picture dropped"
        );
        let cfg = Config {
            capture_rich_text: false,
            ..Default::default()
        };
        let mut c2 = c.clone();
        assert!(c2.restrict(&cfg));
        assert_eq!(c2.kind(), Some(Kind::Text));
        let cfg = Config {
            capture_images: false,
            ..Default::default()
        };
        let mut img = Content {
            png: Some(tiny_png(2, 2)),
            ..Default::default()
        };
        assert!(!img.restrict(&cfg));
        let cfg = Config {
            capture_files: false,
            ..Default::default()
        };
        let mut files = Content {
            text: Some("a.txt".into()),
            files: vec!["/tmp/a.txt".into()],
            ..Default::default()
        };
        assert!(files.restrict(&cfg));
        assert_eq!(files.kind(), Some(Kind::Text), "falls back to the names");
    }

    #[test]
    fn dedupe_ignores_formatting_and_source() {
        let a = Content {
            text: Some("same".into()),
            html: Some("<i>same</i>".into()),
            ..Default::default()
        };
        let b = Content::text("same");
        assert_eq!(a.dedupe_hash(), b.dedupe_hash());
        assert_ne!(b.dedupe_hash(), Content::text("same ").dedupe_hash());
        let f = Content {
            files: vec!["same".into()],
            ..Default::default()
        };
        assert_ne!(f.dedupe_hash(), b.dedupe_hash());
    }

    #[test]
    fn privacy_markers() {
        let cfg = Config::default();
        let types = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for m in [
            "org.nspasteboard.ConcealedType",
            "org.nspasteboard.TransientType",
            "org.nspasteboard.AutoGeneratedType",
            "ExcludeClipboardContentFromMonitorProcessing",
            "CanIncludeInClipboardHistory=0",
        ] {
            assert_eq!(
                skip_reason(&types(&["public.utf8-plain-text", m]), None, &cfg),
                Some(Skip::Marked(m.into())),
                "{m}"
            );
        }
        assert_eq!(
            skip_reason(&types(&["public.utf8-plain-text"]), None, &cfg),
            None
        );
        // Marker check can be turned off.
        let off = Config {
            skip_concealed: false,
            ..Default::default()
        };
        assert_eq!(
            skip_reason(&types(&["org.nspasteboard.ConcealedType"]), None, &off),
            None
        );
        // Paused wins.
        let paused = Config {
            paused: true,
            ..Default::default()
        };
        assert_eq!(skip_reason(&[], None, &paused), Some(Skip::Paused));
    }

    #[test]
    fn excluded_apps() {
        let cfg = Config::default();
        for (name, id) in [
            ("1Password", "com.1password.1password"),
            ("Bitwarden", "com.bitwarden.desktop"),
            ("KeePassXC", "org.keepassxc.keepassxc"),
            ("Keychain Access", "com.apple.keychainaccess"),
            ("Passwords", "com.apple.Passwords"),
        ] {
            let by_id = SourceApp {
                name: None,
                id: Some(id.into()),
            };
            assert!(
                matches!(
                    skip_reason(&[], Some(&by_id), &cfg),
                    Some(Skip::ExcludedApp(_))
                ),
                "{id}"
            );
            let by_name = SourceApp {
                name: Some(name.into()),
                id: None,
            };
            assert!(
                matches!(
                    skip_reason(&[], Some(&by_name), &cfg),
                    Some(Skip::ExcludedApp(_))
                ),
                "{name}"
            );
        }
        // Windows exe names.
        let exe = SourceApp {
            name: None,
            id: Some("KeePassXC.exe".into()),
        };
        assert!(skip_reason(&[], Some(&exe), &cfg).is_some());
        let safari = SourceApp {
            name: Some("Safari".into()),
            id: Some("com.apple.Safari".into()),
        };
        assert_eq!(skip_reason(&[], Some(&safari), &cfg), None);
    }

    #[test]
    fn filter_size_and_empty() {
        let cfg = Config {
            max_item_mb: 1,
            ..Default::default()
        };
        let mut big = Capture {
            content: Content::text("x".repeat(2 << 20)),
            ..Default::default()
        };
        assert!(matches!(filter(&mut big, &cfg), Err(Skip::TooLarge(_))));
        let mut empty = Capture::default();
        assert_eq!(filter(&mut empty, &cfg), Err(Skip::Empty));
        let mut ok = Capture {
            content: Content::text("fine"),
            ..Default::default()
        };
        assert_eq!(filter(&mut ok, &cfg), Ok(()));
    }

    #[test]
    fn image_conversion() {
        let png = tiny_png(40, 20);
        assert_eq!(png_size(&png), Some((40, 20)));
        let tiff = png_to_tiff(&png).unwrap();
        let (back, w, h) = to_png(&tiff).unwrap();
        assert_eq!((w, h), (40, 20));
        assert_eq!(png_size(&back), Some((40, 20)));
        let t = thumbnail(&png, 10, 10).unwrap();
        assert_eq!(png_size(&t), Some((10, 5)));
        assert!(to_png(b"not an image").is_none());
    }
}
