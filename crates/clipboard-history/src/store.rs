//! The history: items newest first, de-duplication, limits, pinning and
//! persistence.
//!
//! On disk (`<config dir>/PowerMac/clipboard/`):
//!
//! ```text
//! index.json            {"version": 1, "nextId": 42, "items": [Item, ...]}
//! blobs/<hash>-html.html, -rtf.rtf, -text.txt, -png.png, -thumb.png
//! ```
//!
//! Every file is written to a temporary name and renamed into place, so a
//! crash never leaves a half-written index. Blobs nothing refers to are
//! removed when the index is saved. With `persist` off the same structure
//! lives in memory only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::content::{png_size, thumbnail, Capture, Content, Kind, SourceApp};

/// Longest text kept inline in the index (searchable); longer texts are
/// stored in a blob and only their head is searched.
const INLINE_TEXT: usize = 4096;
/// Characters of preview text.
const PREVIEW_CHARS: usize = 400;
/// Thumbnail bounds (pixels; drawn at about half that on Retina screens).
pub const THUMB_W: u32 = 480;
pub const THUMB_H: u32 = 240;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// One history entry (the index record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub kind: Kind,
    /// [`Content::dedupe_hash`].
    pub hash: u64,
    /// First copied (unix ms).
    pub created: i64,
    /// Last copied or pasted (unix ms); the list is ordered by this.
    pub last_used: i64,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceApp>,
    /// Display text: the start of the text, file names, or "Image".
    pub preview: String,
    /// The text (or its first 4 KiB when `textBlob` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtf_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Total size of the content in bytes.
    #[serde(default)]
    pub bytes: u64,
    /// Characters of text (for "1,204 characters").
    #[serde(default)]
    pub chars: usize,
}

/// What the tray menu and the picker list show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummary {
    pub id: u64,
    pub kind: Kind,
    pub preview: String,
    pub pinned: bool,
    pub last_used: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<(u32, u32)>,
    #[serde(default)]
    pub file_count: usize,
    #[serde(default)]
    pub chars: usize,
    #[serde(default)]
    pub has_thumbnail: bool,
}

impl ItemSummary {
    /// A one-line label of at most `max` characters, e.g. for a menu item.
    pub fn label(&self, max: usize) -> String {
        let line = self
            .preview
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        truncate(&line, max)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t = t.trim_end().to_string();
        t.push('…');
        t
    }
}

impl Item {
    pub fn summary(&self) -> ItemSummary {
        ItemSummary {
            id: self.id,
            kind: self.kind,
            preview: self.preview.clone(),
            pinned: self.pinned,
            last_used: self.last_used,
            source_name: self
                .source
                .as_ref()
                .and_then(|s| s.name.clone().or_else(|| s.id.clone())),
            image_size: self.image_size,
            file_count: self.files.len(),
            chars: self.chars,
            has_thumbnail: self.thumb_blob.is_some(),
        }
    }

    fn blobs(&self) -> impl Iterator<Item = &String> {
        [
            &self.text_blob,
            &self.html_blob,
            &self.rtf_blob,
            &self.image_blob,
            &self.thumb_blob,
        ]
        .into_iter()
        .flatten()
    }

    /// Whether `q` (lower case) matches.
    fn matches(&self, q: &str) -> bool {
        let has = |s: &str| s.to_lowercase().contains(q);
        has(&self.preview)
            || self.text.as_deref().is_some_and(has)
            || self.files.iter().any(|f| has(f))
            || self
                .source
                .as_ref()
                .is_some_and(|s| s.name.as_deref().is_some_and(has))
            || (self.kind == Kind::Image && "image".contains(q))
    }
}

/// Text shown in the list.
fn preview_for(c: &Content, kind: Kind, size: Option<(u32, u32)>) -> String {
    match kind {
        Kind::Files => {
            let names: Vec<String> = c
                .files
                .iter()
                .map(|f| {
                    Path::new(f)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| f.clone())
                })
                .collect();
            match names.len() {
                0 => String::new(),
                1..=3 => names.join(", "),
                n => format!("{}, … ({n} items)", names[..2].join(", ")),
            }
        }
        Kind::Text | Kind::RichText => {
            let t = c.text.as_deref().unwrap_or("");
            // Drop leading blank lines and common indentation-only lines.
            let t = t.trim_start_matches(['\n', '\r']).trim_end();
            truncate(t, PREVIEW_CHARS)
        }
        Kind::Image => match size {
            Some((w, h)) => format!("Image {w} × {h}"),
            None => "Image".into(),
        },
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u32,
    next_id: u64,
    items: Vec<Item>,
}

/// Result of [`Store::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    /// A new item.
    New(u64),
    /// Already in the history: moved to the top (content refreshed).
    Moved(u64),
}

impl Added {
    pub fn id(self) -> u64 {
        match self {
            Added::New(i) | Added::Moved(i) => i,
        }
    }
}

/// The clipboard history.
#[derive(Debug, Default)]
pub struct Store {
    /// `None`: memory only.
    dir: Option<PathBuf>,
    /// Newest (`last_used`) first.
    items: Vec<Item>,
    next_id: u64,
    mem: HashMap<String, Vec<u8>>,
}

impl Store {
    /// A store that keeps nothing on disk.
    pub fn in_memory() -> Self {
        Store {
            next_id: 1,
            ..Default::default()
        }
    }

    /// Open (or create) the store in `dir`. A corrupt index is moved aside
    /// (`index.json.bad`) and the history starts empty.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir.join("blobs"))
            .with_context(|| format!("creating {}", dir.display()))?;
        restrict_permissions(dir);
        let mut s = Store {
            dir: Some(dir.to_path_buf()),
            next_id: 1,
            ..Default::default()
        };
        let idx = dir.join("index.json");
        match std::fs::read(&idx) {
            Ok(bytes) => match serde_json::from_slice::<Index>(&bytes) {
                Ok(i) => {
                    s.items = i.items;
                    s.next_id = i.next_id.max(1);
                    s.sort();
                    let max = s.items.iter().map(|i| i.id).max().unwrap_or(0);
                    s.next_id = s.next_id.max(max + 1);
                }
                Err(e) => {
                    log::warn!(
                        "clipboard history: {} is corrupt ({e}); starting empty",
                        idx.display()
                    );
                    let _ = std::fs::rename(&idx, dir.join("index.json.bad"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", idx.display())),
        }
        // Items whose blobs vanished are dropped.
        let before = s.items.len();
        let dir2 = dir.to_path_buf();
        s.items
            .retain(|i| i.blobs().all(|b| dir2.join("blobs").join(b).exists()));
        if s.items.len() != before {
            log::warn!(
                "clipboard history: dropped {} item(s) with missing data",
                before - s.items.len()
            );
            s.save()?;
        }
        Ok(s)
    }

    /// Open `dir` when persisting, otherwise an empty in-memory store.
    pub fn for_config(dir: &Path, cfg: &Config) -> Result<Self> {
        if cfg.persist {
            Store::open(dir)
        } else {
            Ok(Store::in_memory())
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Switch between persistent (`Some(dir)`) and memory-only storage.
    /// Going to memory-only deletes the history from disk; going to disk
    /// writes the current history there (merged with nothing: what's in
    /// memory wins).
    pub fn set_dir(&mut self, dir: Option<&Path>) -> Result<()> {
        match (self.dir.clone(), dir) {
            (Some(a), Some(b)) if a == b => Ok(()),
            (Some(old), None) => {
                for i in &self.items {
                    for b in i.blobs() {
                        if let Ok(bytes) = std::fs::read(old.join("blobs").join(b)) {
                            self.mem.insert(b.clone(), bytes);
                        }
                    }
                }
                self.dir = None;
                remove_store_files(&old);
                Ok(())
            }
            (_, Some(new)) => {
                let blobs: Vec<(String, Vec<u8>)> = self
                    .items
                    .iter()
                    .flat_map(|i| i.blobs().cloned().collect::<Vec<_>>())
                    .filter_map(|b| self.read_blob(&b).map(|d| (b, d)))
                    .collect();
                if let Some(old) = self.dir.take() {
                    remove_store_files(&old);
                }
                std::fs::create_dir_all(new.join("blobs"))?;
                restrict_permissions(new);
                self.dir = Some(new.to_path_buf());
                for (name, data) in blobs {
                    self.write_blob(&name, &data)?;
                }
                self.mem.clear();
                self.save()
            }
            (None, None) => Ok(()),
        }
    }

    fn sort(&mut self) {
        self.items
            .sort_by(|a, b| b.last_used.cmp(&a.last_used).then(b.id.cmp(&a.id)));
    }

    /// All items, most recently used first.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items for the picker: pinned first, then by recency, filtered by a
    /// case-insensitive search (all words must match).
    pub fn search(&self, query: &str) -> Vec<&Item> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        let mut v: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| words.iter().all(|w| i.matches(w)))
            .collect();
        v.sort_by_key(|i| !i.pinned); // stable: keeps recency order
        v
    }

    /// The `n` most recently used items (pinned or not).
    pub fn recent(&self, n: usize) -> Vec<ItemSummary> {
        self.items.iter().take(n).map(Item::summary).collect()
    }

    /// Record a capture (already filtered). The same content again moves the
    /// existing item to the top and refreshes its formats; it keeps its pin.
    pub fn add(&mut self, cap: Capture, cfg: &Config, now: i64) -> Result<Added> {
        let content = cap.content;
        let kind = content
            .kind()
            .ok_or_else(|| anyhow::anyhow!("nothing to store"))?;
        let hash = content.dedupe_hash();
        let existing = self
            .items
            .iter()
            .position(|i| i.hash == hash && i.kind_compatible(kind));
        let (id, created, pinned) = match existing {
            Some(pos) => {
                let old = self.items.remove(pos);
                let r = (old.id, old.created, old.pinned);
                self.forget_blobs(&old, None);
                r
            }
            None => {
                let id = self.next_id;
                self.next_id += 1;
                (id, now, false)
            }
        };
        let item = self.build_item(id, kind, hash, &content, cap.source, created, now, pinned)?;
        self.items.insert(0, item);
        self.prune(cfg, now);
        self.save()?;
        Ok(if existing.is_some() {
            Added::Moved(id)
        } else {
            Added::New(id)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_item(
        &mut self,
        id: u64,
        kind: Kind,
        hash: u64,
        c: &Content,
        source: Option<SourceApp>,
        created: i64,
        now: i64,
        pinned: bool,
    ) -> Result<Item> {
        let stem = format!("{hash:016x}");
        let mut item = Item {
            id,
            kind,
            hash,
            created,
            last_used: now,
            pinned,
            source,
            preview: String::new(),
            text: None,
            text_blob: None,
            html_blob: None,
            rtf_blob: None,
            image_blob: None,
            thumb_blob: None,
            image_size: None,
            files: c.files.clone(),
            bytes: c.size(),
            chars: 0,
        };
        if let Some(t) = &c.text {
            item.chars = t.chars().count();
            if t.len() <= INLINE_TEXT {
                item.text = Some(t.clone());
            } else {
                let mut cut = INLINE_TEXT;
                while !t.is_char_boundary(cut) {
                    cut -= 1;
                }
                item.text = Some(t[..cut].to_string());
                let name = format!("{stem}-text.txt");
                self.write_blob(&name, t.as_bytes())?;
                item.text_blob = Some(name);
            }
        }
        if let Some(h) = &c.html {
            let name = format!("{stem}-html.html");
            self.write_blob(&name, h.as_bytes())?;
            item.html_blob = Some(name);
        }
        if let Some(r) = &c.rtf {
            let name = format!("{stem}-rtf.rtf");
            self.write_blob(&name, r)?;
            item.rtf_blob = Some(name);
        }
        if let Some(p) = &c.png {
            item.image_size = png_size(p);
            let name = format!("{stem}-png.png");
            self.write_blob(&name, p)?;
            item.image_blob = Some(name);
            if let Some(t) = thumbnail(p, THUMB_W, THUMB_H) {
                let name = format!("{stem}-thumb.png");
                self.write_blob(&name, &t)?;
                item.thumb_blob = Some(name);
            }
        }
        item.preview = preview_for(c, kind, item.image_size);
        Ok(item)
    }

    /// Apply `maxAgeDays` and `maxItems` (pinned items are exempt). Returns
    /// how many items were removed. Doesn't save.
    pub fn prune(&mut self, cfg: &Config, now: i64) -> usize {
        let before = self.items.len();
        let max_age = cfg.max_age_days as i64 * 86_400_000;
        let mut unpinned = 0usize;
        let mut removed = Vec::new();
        let items = std::mem::take(&mut self.items);
        for i in items {
            let keep = i.pinned
                || ((max_age == 0 || now - i.last_used <= max_age) && {
                    unpinned += 1;
                    unpinned <= cfg.max_items
                });
            if keep {
                self.items.push(i);
            } else {
                removed.push(i);
            }
        }
        for i in &removed {
            self.forget_blobs(i, None);
        }
        before - self.items.len()
    }

    /// Move an item to the top (it was just pasted).
    pub fn touch(&mut self, id: u64, now: i64) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        let mut i = self.items.remove(pos);
        i.last_used = now;
        self.items.insert(0, i);
        true
    }

    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        match self.items.iter_mut().find(|i| i.id == id) {
            Some(i) => {
                i.pinned = pinned;
                true
            }
            None => false,
        }
    }

    pub fn delete(&mut self, id: u64) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        let i = self.items.remove(pos);
        self.forget_blobs(&i, None);
        true
    }

    /// Remove everything (or everything but the pinned items).
    pub fn clear(&mut self, keep_pinned: bool) {
        let items = std::mem::take(&mut self.items);
        for i in items {
            if keep_pinned && i.pinned {
                self.items.push(i);
            } else {
                self.forget_blobs(&i, None);
            }
        }
    }

    /// The full content of an item.
    pub fn content(&self, id: u64) -> Option<Content> {
        let i = self.get(id)?;
        let text = match &i.text_blob {
            Some(b) => Some(String::from_utf8_lossy(&self.read_blob(b)?).into_owned()),
            None => i.text.clone(),
        };
        let html = match &i.html_blob {
            Some(b) => Some(String::from_utf8_lossy(&self.read_blob(b)?).into_owned()),
            None => None,
        };
        let rtf = match &i.rtf_blob {
            Some(b) => Some(self.read_blob(b)?),
            None => None,
        };
        let png = match &i.image_blob {
            Some(b) => Some(self.read_blob(b)?),
            None => None,
        };
        Some(Content {
            text,
            html,
            rtf,
            png,
            files: i.files.clone(),
        })
    }

    /// The item's thumbnail (PNG), for images.
    pub fn thumbnail(&self, id: u64) -> Option<Vec<u8>> {
        self.read_blob(self.get(id)?.thumb_blob.as_ref()?)
    }

    // -- blobs / files ------------------------------------------------------

    fn read_blob(&self, name: &str) -> Option<Vec<u8>> {
        match &self.dir {
            Some(d) => std::fs::read(d.join("blobs").join(name)).ok(),
            None => self.mem.get(name).cloned(),
        }
    }

    fn write_blob(&mut self, name: &str, data: &[u8]) -> Result<()> {
        match &self.dir {
            Some(d) => write_atomic(&d.join("blobs").join(name), data),
            None => {
                self.mem.insert(name.to_string(), data.to_vec());
                Ok(())
            }
        }
    }

    /// Delete an item's blobs unless another item still uses them.
    fn forget_blobs(&mut self, item: &Item, _keep: Option<&Item>) {
        let names: Vec<String> = item.blobs().cloned().collect();
        for n in names {
            let used = self.items.iter().any(|i| i.blobs().any(|b| *b == n));
            if used {
                continue;
            }
            match &self.dir {
                Some(d) => {
                    let _ = std::fs::remove_file(d.join("blobs").join(&n));
                }
                None => {
                    self.mem.remove(&n);
                }
            }
        }
    }

    /// Write the index (atomically) and delete stray blobs. No-op in memory.
    pub fn save(&self) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let idx = Index {
            version: 1,
            next_id: self.next_id,
            items: self.items.clone(),
        };
        let json = serde_json::to_vec(&idx)?;
        write_atomic(&dir.join("index.json"), &json)?;
        // Garbage-collect blobs nothing refers to (e.g. after a crash).
        if let Ok(rd) = std::fs::read_dir(dir.join("blobs")) {
            let live: std::collections::HashSet<&String> =
                self.items.iter().flat_map(|i| i.blobs()).collect();
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !live.contains(&name) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(())
    }
}

impl Item {
    fn kind_compatible(&self, k: Kind) -> bool {
        matches!(
            (self.kind, k),
            (Kind::Text | Kind::RichText, Kind::Text | Kind::RichText)
        ) || self.kind == k
    }
}

/// Write `data` to `path` via a temporary file and a rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all().ok();
    }
    restrict_file(&tmp);
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn remove_store_files(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.join("blobs"));
    let _ = std::fs::remove_file(dir.join("index.json"));
    let _ = std::fs::remove_file(dir.join("index.json.bad"));
}

#[cfg(unix)]
fn restrict_permissions(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn restrict_permissions(_dir: &Path) {}

#[cfg(unix)]
pub(crate) fn restrict_file(f: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
pub(crate) fn restrict_file(_f: &Path) {}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("pm-cliphist-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                160,
                255,
            ])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn cap(c: Content) -> Capture {
        Capture {
            content: c,
            types: vec![],
            source: Some(SourceApp {
                name: Some("TextEdit".into()),
                id: Some("com.apple.TextEdit".into()),
            }),
        }
    }

    fn text(s: &str) -> Capture {
        cap(Content::text(s))
    }

    const MIN: i64 = 60_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn add_dedupe_moves_to_top() {
        let cfg = Config::default();
        let mut s = Store::in_memory();
        let a = s.add(text("alpha"), &cfg, 1000).unwrap();
        let b = s.add(text("beta"), &cfg, 2000).unwrap();
        assert!(matches!(a, Added::New(_)) && matches!(b, Added::New(_)));
        assert_eq!(s.items()[0].preview, "beta");
        // Same text again (now with HTML): moved, same id, richer content.
        let rich = cap(Content {
            text: Some("alpha".into()),
            html: Some("<b>alpha</b>".into()),
            ..Default::default()
        });
        let again = s.add(rich, &cfg, 3000).unwrap();
        assert_eq!(again, Added::Moved(a.id()));
        assert_eq!(s.len(), 2);
        assert_eq!(s.items()[0].id, a.id());
        assert_eq!(s.items()[0].kind, Kind::RichText);
        assert_eq!(s.items()[0].created, 1000);
        assert_eq!(s.items()[0].last_used, 3000);
        assert_eq!(
            s.content(a.id()).unwrap().html.as_deref(),
            Some("<b>alpha</b>")
        );
    }

    #[test]
    fn max_items_and_pinned_are_kept() {
        let cfg = Config {
            max_items: 3,
            ..Default::default()
        };
        let mut s = Store::in_memory();
        let first = s.add(text("0"), &cfg, 0).unwrap().id();
        assert!(s.set_pinned(first, true));
        for i in 1..10 {
            s.add(text(&i.to_string()), &cfg, i * MIN).unwrap();
        }
        // 3 unpinned + the pinned one.
        assert_eq!(s.len(), 4);
        let previews: Vec<&str> = s.items().iter().map(|i| i.preview.as_str()).collect();
        assert_eq!(previews, vec!["9", "8", "7", "0"]);
        // Pinned first in the picker order.
        let order: Vec<&str> = s.search("").iter().map(|i| i.preview.as_str()).collect();
        assert_eq!(order, vec!["0", "9", "8", "7"]);
    }

    #[test]
    fn max_age_expires_unpinned_only() {
        let cfg = Config {
            max_age_days: 2,
            ..Default::default()
        };
        let mut s = Store::in_memory();
        let old = s.add(text("old"), &cfg, 0).unwrap().id();
        let pinned = s.add(text("pinned"), &cfg, 0).unwrap().id();
        s.set_pinned(pinned, true);
        s.add(text("new"), &cfg, 2 * DAY).unwrap();
        assert!(s.get(old).is_some(), "exactly 2 days: kept");
        assert_eq!(s.prune(&cfg, 3 * DAY), 1);
        assert!(s.get(old).is_none());
        assert!(s.get(pinned).is_some());
        assert_eq!(s.len(), 2);
        // 0 = no age limit.
        let forever = Config {
            max_age_days: 0,
            ..Default::default()
        };
        assert_eq!(s.prune(&forever, 1000 * DAY), 0);
    }

    #[test]
    fn touch_pin_delete_clear() {
        let cfg = Config::default();
        let mut s = Store::in_memory();
        let a = s.add(text("a"), &cfg, 1).unwrap().id();
        let b = s.add(text("b"), &cfg, 2).unwrap().id();
        let c = s.add(text("c"), &cfg, 3).unwrap().id();
        assert!(s.touch(a, 4));
        assert_eq!(s.items()[0].id, a);
        assert!(s.set_pinned(b, true));
        assert!(s.delete(c));
        assert!(!s.delete(c));
        s.clear(true);
        assert_eq!(s.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![b]);
        s.clear(false);
        assert!(s.is_empty());
        assert!(!s.set_pinned(b, false) && !s.touch(b, 9));
    }

    #[test]
    fn search() {
        let cfg = Config::default();
        let mut s = Store::in_memory();
        s.add(text("Hello World"), &cfg, 1).unwrap();
        s.add(text("goodbye moon"), &cfg, 2).unwrap();
        s.add(
            cap(Content {
                files: vec!["/Users/me/Report Q3.pdf".into()],
                text: Some("Report Q3.pdf".into()),
                ..Default::default()
            }),
            &cfg,
            3,
        )
        .unwrap();
        let p = |q: &str| {
            s.search(q)
                .iter()
                .map(|i| i.preview.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(p("hello"), vec!["Hello World"]);
        assert_eq!(p("WORLD hello"), vec!["Hello World"]);
        assert_eq!(p("o"), vec!["Report Q3.pdf", "goodbye moon", "Hello World"]);
        assert_eq!(p("/users/me"), vec!["Report Q3.pdf"]);
        assert_eq!(p("textedit").len(), 3, "source app name");
        assert!(p("zzz").is_empty());
    }

    #[test]
    fn long_text_goes_to_a_blob() {
        let cfg = Config::default();
        let mut s = Store::in_memory();
        let long = "ünïcødé ".repeat(2000);
        let id = s.add(text(&long), &cfg, 1).unwrap().id();
        let it = s.get(id).unwrap();
        assert!(it.text_blob.is_some());
        assert!(it.text.as_ref().unwrap().len() <= INLINE_TEXT);
        assert!(it.preview.chars().count() <= PREVIEW_CHARS);
        assert_eq!(s.content(id).unwrap().text.unwrap(), long);
        assert_eq!(it.chars, long.chars().count());
    }

    #[test]
    fn persist_reload_and_atomic_files() {
        let d = scratch("persist");
        let cfg = Config::default();
        let (img, files, rich);
        {
            let mut s = Store::open(&d).unwrap();
            s.add(text("plain"), &cfg, 1).unwrap();
            rich = s
                .add(
                    cap(Content {
                        text: Some("rich".into()),
                        html: Some("<i>rich</i>".into()),
                        rtf: Some(b"{\\rtf1 rich}".to_vec()),
                        ..Default::default()
                    }),
                    &cfg,
                    2,
                )
                .unwrap()
                .id();
            img = s
                .add(
                    cap(Content {
                        png: Some(png(900, 300)),
                        ..Default::default()
                    }),
                    &cfg,
                    3,
                )
                .unwrap()
                .id();
            files = s
                .add(
                    cap(Content {
                        files: vec!["/tmp/a".into(), "/tmp/b".into()],
                        ..Default::default()
                    }),
                    &cfg,
                    4,
                )
                .unwrap()
                .id();
            s.set_pinned(rich, true);
            s.save().unwrap();
        }
        let s = Store::open(&d).unwrap();
        assert_eq!(s.len(), 4);
        assert!(s.get(rich).unwrap().pinned);
        let c = s.content(rich).unwrap();
        assert_eq!(c.html.as_deref(), Some("<i>rich</i>"));
        assert_eq!(c.rtf.as_deref(), Some(&b"{\\rtf1 rich}"[..]));
        let ii = s.get(img).unwrap();
        assert_eq!(ii.image_size, Some((900, 300)));
        assert_eq!(ii.preview, "Image 900 × 300");
        let th = s.thumbnail(img).unwrap();
        assert_eq!(png_size(&th), Some((THUMB_W, 160)));
        assert_eq!(s.content(files).unwrap().files, vec!["/tmp/a", "/tmp/b"]);
        assert_eq!(s.get(files).unwrap().preview, "a, b");
        // No temp files left behind; ids continue.
        for e in std::fs::read_dir(&d)
            .unwrap()
            .chain(std::fs::read_dir(d.join("blobs")).unwrap())
        {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!n.contains(".tmp-"), "{n}");
        }
        let mut s = s;
        let next = s.add(text("next"), &cfg, 5).unwrap().id();
        assert!(next > files);
        // Deleting removes the blobs.
        let blob_count = || std::fs::read_dir(d.join("blobs")).unwrap().count();
        let n = blob_count();
        s.delete(img);
        s.save().unwrap();
        assert_eq!(blob_count(), n - 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_index_and_missing_blobs() {
        let d = scratch("corrupt");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("index.json"), b"{not json").unwrap();
        let s = Store::open(&d).unwrap();
        assert!(s.is_empty());
        assert!(d.join("index.json.bad").exists());
        let cfg = Config::default();
        let mut s = s;
        let id = s
            .add(
                cap(Content {
                    text: Some("x".into()),
                    html: Some("<p>x</p>".into()),
                    ..Default::default()
                }),
                &cfg,
                1,
            )
            .unwrap()
            .id();
        s.add(text("y"), &cfg, 2).unwrap();
        let blob = s.get(id).unwrap().html_blob.clone().unwrap();
        std::fs::remove_file(d.join("blobs").join(blob)).unwrap();
        // A stray blob is collected on save.
        std::fs::write(d.join("blobs").join("stray.png"), b"x").unwrap();
        let s = Store::open(&d).unwrap();
        assert_eq!(s.len(), 1);
        assert!(!d.join("blobs").join("stray.png").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn switch_between_memory_and_disk() {
        let d = scratch("switch");
        let cfg = Config::default();
        let mut s = Store::open(&d).unwrap();
        let id = s
            .add(
                cap(Content {
                    png: Some(png(20, 20)),
                    ..Default::default()
                }),
                &cfg,
                1,
            )
            .unwrap()
            .id();
        s.set_dir(None).unwrap();
        assert!(
            !d.join("index.json").exists() && !d.join("blobs").exists(),
            "disk history deleted"
        );
        assert!(s.thumbnail(id).is_some(), "still in memory");
        s.add(text("mem"), &cfg, 2).unwrap();
        s.set_dir(Some(&d)).unwrap();
        let back = Store::open(&d).unwrap();
        assert_eq!(back.len(), 2);
        assert!(back.content(id).unwrap().png.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn labels() {
        let cfg = Config::default();
        let mut s = Store::in_memory();
        s.add(
            text("  line one\n\n   line two that is quite long indeed"),
            &cfg,
            1,
        )
        .unwrap();
        let r = s.recent(10);
        assert_eq!(r[0].label(20), "line one line two t…");
        assert_eq!(r[0].source_name.as_deref(), Some("TextEdit"));
    }
}
