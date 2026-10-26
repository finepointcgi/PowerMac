//! Shelf contents and their persistence.
//!
//! Items are kept in `<data_dir>/notch-shelf.json`. Files that the shelf
//! owns (copies of files dragged from temporary locations, images and other
//! raw data dropped without a file) live in `<data_dir>/notch-shelf-files/<id>/`.
//! Copies whose item was removed are deleted at the next start, so a copy
//! that was just dragged out is never pulled from under the receiver.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STORE_FILE: &str = "notch-shelf.json";
pub const FILES_DIR: &str = "notch-shelf-files";
pub const MAX_ITEMS: usize = 200;
/// Longest text snippet stored inline.
pub const MAX_TEXT: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemKind {
    /// A file or folder (by reference, or a copy the shelf owns).
    #[serde(rename_all = "camelCase")]
    File {
        path: PathBuf,
        /// Where a copied file came from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original: Option<PathBuf>,
        /// The file lives in the shelf's own folder.
        #[serde(default)]
        owned: bool,
    },
    #[serde(rename_all = "camelCase")]
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Text { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfItem {
    pub id: u64,
    #[serde(flatten)]
    pub kind: ItemKind,
    /// Unix time in milliseconds.
    #[serde(default)]
    pub added_at: u64,
}

impl ShelfItem {
    /// Short label for the tile.
    pub fn display_name(&self) -> String {
        match &self.kind {
            ItemKind::File { path, .. } => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            ItemKind::Url { url, title } => match title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(t) => t.to_string(),
                None => url_host(url).unwrap_or(url).to_string(),
            },
            ItemKind::Text { text } => {
                let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                truncate_chars(&one_line, 60)
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.kind {
            ItemKind::File { path, .. } => Some(path),
            _ => None,
        }
    }

    pub fn is_image_file(&self) -> bool {
        self.path().is_some_and(is_image_path)
    }
}

/// What can be put on the shelf.
#[derive(Clone, Debug, PartialEq)]
pub enum NewItem {
    File(PathBuf),
    Url { url: String, title: Option<String> },
    Text(String),
    /// Raw data without a file (e.g. an image dragged out of a browser);
    /// saved into the shelf folder as `name` (sanitized).
    Data { bytes: Vec<u8>, name: String },
}

#[derive(Clone, Debug, Default)]
pub struct AddOptions {
    pub copy_temporary_files: bool,
    /// Directories that count as temporary (see [`default_temp_roots`]).
    pub temp_roots: Vec<PathBuf>,
}

pub struct Store {
    data_dir: PathBuf,
    items: Vec<ShelfItem>,
    last_id: u64,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct OnDisk {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    items: Vec<ShelfItem>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// A single URL-looking token (http, https, ftp, mailto) or `None`.
pub fn as_url(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let ok = ["http://", "https://", "ftp://"].iter().any(|p| lower.starts_with(p) && t.len() > p.len()) || (lower.starts_with("mailto:") && t.contains('@'));
    ok.then(|| t.to_string())
}

pub fn is_image_path(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "heic" | "heif" | "tif" | "tiff" | "webp" | "bmp")
}

/// Typical locations of files that vanish soon after a drag: the per-user
/// temp folder, `/tmp`, `/private/var/folders` (screenshot thumbnails,
/// "TemporaryItems"), caches.
pub fn default_temp_roots() -> Vec<PathBuf> {
    let mut v = vec![std::env::temp_dir()];
    #[cfg(unix)]
    {
        for p in ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders"] {
            v.push(PathBuf::from(p));
        }
        if let Some(home) = std::env::var_os("HOME") {
            v.push(Path::new(&home).join("Library/Caches"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(Path::new(&l).join("Temp"));
            v.push(Path::new(&l).join("Microsoft\\Windows\\INetCache"));
        }
    }
    v
}

/// Whether `path` lives in one of `roots` or in a folder that is temporary
/// by name ("TemporaryItems", ".../T/...").
pub fn is_temporary_path(path: &Path, roots: &[PathBuf]) -> bool {
    if roots.iter().any(|r| !r.as_os_str().is_empty() && path.starts_with(r)) {
        return true;
    }
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "TemporaryItems" || s.starts_with("NSIRD_") || s == "com.apple.dragndrop"
    })
}

fn sanitize_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() {
        "item".into()
    } else {
        truncate_chars(&s, 120)
    }
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    let md = fs::symlink_metadata(from)?;
    if md.is_dir() {
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else if md.file_type().is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fs::read_link(from)?, to)
        }
        #[cfg(not(unix))]
        {
            fs::copy(from, to).map(|_| ())
        }
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

impl Store {
    /// Load the shelf from `data_dir`, dropping items whose file is gone
    /// and deleting owned copies nothing refers to any more.
    pub fn open(data_dir: impl Into<PathBuf>) -> Store {
        let data_dir = data_dir.into();
        let items = match fs::read(data_dir.join(STORE_FILE)) {
            Ok(bytes) => match serde_json::from_slice::<OnDisk>(&bytes) {
                Ok(d) => d.items,
                Err(e) => {
                    log::warn!("notch-shelf: ignoring unreadable {}: {e}", STORE_FILE);
                    vec![]
                }
            },
            Err(_) => vec![],
        };
        let last_id = items.iter().map(|i| i.id).max().unwrap_or(0);
        let mut s = Store { data_dir, items, last_id };
        let pruned = s.prune_missing();
        s.delete_orphans();
        if pruned {
            let _ = s.save();
        }
        s
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join(FILES_DIR)
    }

    pub fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&ShelfItem> {
        self.items.iter().find(|i| i.id == id)
    }

    fn next_id(&mut self) -> u64 {
        // Time-based so ids (and copy folders) stay unique across restarts
        // even if the JSON was lost.
        self.last_id = (self.last_id + 1).max(now_ms());
        self.last_id
    }

    /// Add an item; returns its id. Re-adding an existing file / URL / text
    /// moves it to the end instead of duplicating it.
    pub fn add(&mut self, item: NewItem, opts: &AddOptions) -> Result<u64> {
        let kind = match item {
            NewItem::File(path) => {
                anyhow::ensure!(path.exists(), "{} does not exist", path.display());
                if let Some(id) = self.find_file(&path) {
                    return Ok(self.bump(id));
                }
                if opts.copy_temporary_files && is_temporary_path(&path, &opts.temp_roots) && !path.starts_with(self.files_dir()) {
                    let id = self.next_id();
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
                    let dest_dir = self.files_dir().join(id.to_string());
                    let dest = dest_dir.join(sanitize_name(&name));
                    fs::create_dir_all(&dest_dir)?;
                    copy_recursive(&path, &dest).with_context(|| format!("copying {}", path.display()))?;
                    return Ok(self.push(id, ItemKind::File { path: dest, original: Some(path), owned: true }));
                }
                let owned = path.starts_with(self.files_dir());
                ItemKind::File { path, original: None, owned }
            }
            NewItem::Url { url, title } => {
                if let Some(i) = self.items.iter().find(|i| matches!(&i.kind, ItemKind::Url { url: u, .. } if *u == url)) {
                    let id = i.id;
                    return Ok(self.bump(id));
                }
                ItemKind::Url { url, title }
            }
            NewItem::Text(text) => {
                anyhow::ensure!(!text.trim().is_empty(), "empty text");
                if let Some(url) = as_url(&text) {
                    return self.add(NewItem::Url { url, title: None }, opts);
                }
                let text = if text.len() > MAX_TEXT {
                    let mut end = MAX_TEXT;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text[..end].to_string()
                } else {
                    text
                };
                if let Some(i) = self.items.iter().find(|i| matches!(&i.kind, ItemKind::Text { text: t } if *t == text)) {
                    let id = i.id;
                    return Ok(self.bump(id));
                }
                ItemKind::Text { text }
            }
            NewItem::Data { bytes, name } => {
                let id = self.next_id();
                let dest_dir = self.files_dir().join(id.to_string());
                fs::create_dir_all(&dest_dir)?;
                let dest = dest_dir.join(sanitize_name(&name));
                fs::write(&dest, bytes)?;
                return Ok(self.push(id, ItemKind::File { path: dest, original: None, owned: true }));
            }
        };
        let id = self.next_id();
        Ok(self.push(id, kind))
    }

    fn find_file(&self, path: &Path) -> Option<u64> {
        self.items
            .iter()
            .find(|i| matches!(&i.kind, ItemKind::File { path: p, original, .. } if p == path || original.as_deref() == Some(path)))
            .map(|i| i.id)
    }

    fn push(&mut self, id: u64, kind: ItemKind) -> u64 {
        self.items.push(ShelfItem { id, kind, added_at: now_ms() });
        while self.items.len() > MAX_ITEMS {
            self.items.remove(0);
        }
        id
    }

    fn bump(&mut self, id: u64) -> u64 {
        if let Some(pos) = self.items.iter().position(|i| i.id == id) {
            let it = self.items.remove(pos);
            self.items.push(it);
        }
        id
    }

    pub fn remove(&mut self, ids: &[u64]) -> bool {
        let before = self.items.len();
        self.items.retain(|i| !ids.contains(&i.id));
        self.items.len() != before
    }

    pub fn clear(&mut self) -> bool {
        let had = !self.items.is_empty();
        self.items.clear();
        had
    }

    /// Move item `id` to position `to` (clamped).
    pub fn reorder(&mut self, id: u64, to: usize) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else { return false };
        let it = self.items.remove(pos);
        let to = to.min(self.items.len());
        self.items.insert(to, it);
        pos != to
    }

    /// Drop file items whose file no longer exists. Returns true if any
    /// were removed.
    pub fn prune_missing(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|i| match &i.kind {
            ItemKind::File { path, .. } => path.exists(),
            _ => true,
        });
        self.items.len() != before
    }

    /// Delete owned copy folders that no item refers to.
    pub fn delete_orphans(&self) {
        let dir = self.files_dir();
        let Ok(rd) = fs::read_dir(&dir) else { return };
        let live: HashSet<String> = self
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::File { path, owned: true, .. } => path.strip_prefix(&dir).ok()?.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // Only folders the shelf itself created (numeric ids).
            if name.chars().all(|c| c.is_ascii_digit()) && !live.contains(&name) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    /// Atomically write the JSON file.
    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        let d = OnDisk { version: 1, items: self.items.clone() };
        let json = serde_json::to_vec_pretty(&d)?;
        let tmp = self.data_dir.join(format!("{STORE_FILE}.tmp"));
        fs::write(&tmp, json)?;
        fs::rename(&tmp, self.data_dir.join(STORE_FILE))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("notch-shelf-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn json_format_is_camel_case_and_tagged() {
        let it = ShelfItem { id: 7, kind: ItemKind::File { path: "/a/b.txt".into(), original: None, owned: false }, added_at: 5 };
        let v = serde_json::to_value(&it).unwrap();
        assert_eq!(v, serde_json::json!({"id": 7, "kind": "file", "path": "/a/b.txt", "owned": false, "addedAt": 5}));
        let u: ShelfItem = serde_json::from_value(serde_json::json!({"id": 1, "kind": "url", "url": "https://x.y"})).unwrap();
        assert_eq!(u.kind, ItemKind::Url { url: "https://x.y".into(), title: None });
    }

    #[test]
    fn persistence_round_trip_and_stale_files_dropped() {
        let dir = scratch("persist");
        let data = dir.join("data");
        let f1 = dir.join("keep.txt");
        let f2 = dir.join("gone.txt");
        fs::write(&f1, "a").unwrap();
        fs::write(&f2, "b").unwrap();
        let opts = AddOptions::default();
        let mut s = Store::open(&data);
        let a = s.add(NewItem::File(f1.clone()), &opts).unwrap();
        let b = s.add(NewItem::File(f2.clone()), &opts).unwrap();
        s.add(NewItem::Text("hello  world\nsecond line".into()), &opts).unwrap();
        s.add(NewItem::Text(" https://example.com/x ".into()), &opts).unwrap();
        assert!(b > a);
        s.save().unwrap();
        fs::remove_file(&f2).unwrap();
        let s2 = Store::open(&data);
        assert_eq!(s2.items().len(), 3);
        assert_eq!(s2.items()[0].path(), Some(f1.as_path()));
        assert_eq!(s2.items()[1].display_name(), "hello world second line");
        assert_eq!(s2.items()[2].kind, ItemKind::Url { url: "https://example.com/x".into(), title: None });
        assert_eq!(s2.items()[2].display_name(), "example.com");
        // Saved again without the stale entry.
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(data.join(STORE_FILE)).unwrap()).unwrap();
        assert_eq!(raw["items"].as_array().unwrap().len(), 3);
        // Corrupt file: starts empty instead of failing.
        fs::write(data.join(STORE_FILE), "{nope").unwrap();
        assert!(Store::open(&data).items().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicates_move_to_end_and_ids_unique() {
        let dir = scratch("dupes");
        let f = dir.join("x.png");
        fs::write(&f, "x").unwrap();
        let opts = AddOptions::default();
        let mut s = Store::open(dir.join("d"));
        let a = s.add(NewItem::File(f.clone()), &opts).unwrap();
        s.add(NewItem::Text("t".into()), &opts).unwrap();
        assert_eq!(s.add(NewItem::File(f.clone()), &opts).unwrap(), a);
        assert_eq!(s.items().len(), 2);
        assert_eq!(s.items()[1].id, a);
        assert!(s.items()[1].is_image_file());
        assert!(s.add(NewItem::File(dir.join("missing")), &opts).is_err());
        assert!(s.add(NewItem::Text("   ".into()), &opts).is_err());
        assert!(s.reorder(a, 0));
        assert_eq!(s.items()[0].id, a);
        assert!(s.remove(&[a]));
        assert!(!s.remove(&[a]));
        assert!(s.clear());
        assert!(!s.clear());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn temporary_files_are_copied_and_orphans_cleaned() {
        let dir = scratch("temp");
        let tmp_root = dir.join("T");
        fs::create_dir_all(tmp_root.join("folder")).unwrap();
        let f = tmp_root.join("shot.png");
        fs::write(&f, "png").unwrap();
        fs::write(tmp_root.join("folder/inner.txt"), "in").unwrap();
        let opts = AddOptions { copy_temporary_files: true, temp_roots: vec![tmp_root.clone()] };
        let data = dir.join("data");
        let mut s = Store::open(&data);
        let id = s.add(NewItem::File(f.clone()), &opts).unwrap();
        let ItemKind::File { path, original, owned } = s.get(id).unwrap().kind.clone() else { panic!() };
        assert!(owned);
        assert_eq!(original.as_deref(), Some(f.as_path()));
        assert!(path.starts_with(s.files_dir()));
        assert_eq!(fs::read_to_string(&path).unwrap(), "png");
        // Re-adding the original finds the copy.
        assert_eq!(s.add(NewItem::File(f.clone()), &opts).unwrap(), id);
        let fid = s.add(NewItem::File(tmp_root.join("folder")), &opts).unwrap();
        let fpath = s.get(fid).unwrap().path().unwrap().to_path_buf();
        assert_eq!(fs::read_to_string(fpath.join("inner.txt")).unwrap(), "in");
        let did = s.add(NewItem::Data { bytes: vec![1, 2, 3], name: "../evil/name.png".into() }, &opts).unwrap();
        let dpath = s.get(did).unwrap().path().unwrap().to_path_buf();
        assert!(dpath.starts_with(s.files_dir()));
        assert_eq!(fs::read(&dpath).unwrap(), vec![1, 2, 3]);
        // Not copied when the option is off.
        let off = AddOptions { copy_temporary_files: false, ..opts.clone() };
        let mut s2 = Store::open(dir.join("data2"));
        let id2 = s2.add(NewItem::File(f.clone()), &off).unwrap();
        assert_eq!(s2.get(id2).unwrap().path(), Some(f.as_path()));
        // Removing an owned item keeps its copy until the next start.
        s.remove(&[id]);
        s.save().unwrap();
        assert!(path.exists());
        let s3 = Store::open(&data);
        assert!(!path.exists());
        assert!(fpath.exists());
        assert_eq!(s3.items().len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_detection_and_url_parsing() {
        let roots = vec![PathBuf::from("/private/var/folders")];
        assert!(is_temporary_path(Path::new("/private/var/folders/ab/T/x.png"), &roots));
        assert!(is_temporary_path(Path::new("/Users/me/Library/Caches/TemporaryItems/x"), &[]));
        assert!(!is_temporary_path(Path::new("/Users/me/Desktop/x.png"), &roots));
        assert_eq!(as_url("https://a.b/c"), Some("https://a.b/c".into()));
        assert_eq!(as_url("mailto:me@x.y"), Some("mailto:me@x.y".into()));
        assert_eq!(as_url("https://"), None);
        assert_eq!(as_url("see https://a.b"), None);
        assert_eq!(as_url("file:///etc"), None);
        assert_eq!(url_host("https://user@www.rust-lang.org:443/x?y"), Some("rust-lang.org:443"));
        let long = ShelfItem { id: 1, kind: ItemKind::Text { text: "x".repeat(100) }, added_at: 0 };
        assert_eq!(long.display_name().chars().count(), 60);
        let titled = ShelfItem { id: 1, kind: ItemKind::Url { url: "https://a.b".into(), title: Some("  Title ".into()) }, added_at: 0 };
        assert_eq!(titled.display_name(), "Title");
    }
}
