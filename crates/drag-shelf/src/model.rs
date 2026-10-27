//! The open shelves and their items.
//!
//! Each shelf is a [`pm_shelf_core::store::Store`] in its own folder,
//! `<data_dir>/drag-shelves/<shelf id>/` (`items.json`, and `files/` for
//! copies of temporary files and dropped images). `drag-shelves/index.json`
//! lists the open shelves with their positions, so shelves that held items
//! when PowerMac quit come back at the next start. The folder of a closed
//! shelf is deleted at the next start (never right away: a copy that was
//! just dragged out may still be being read by the app it went to).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use pm_shelf_core::geom::{Point, Rect};
use pm_shelf_core::store::{self, AddOptions, ItemKind, NewItem, ShelfItem, Store, StoreNames};
use serde::{Deserialize, Serialize};

pub const ROOT_DIR: &str = "drag-shelves";
pub const INDEX_FILE: &str = "index.json";
pub const ITEMS_FILE: &str = "items.json";
pub const FILES_DIR: &str = "files";
/// Hard limit, whatever the config says.
pub const MAX_SHELVES: usize = 20;

fn names() -> StoreNames {
    StoreNames::new(ITEMS_FILE, FILES_DIR, "drag-shelf")
}

/// One open shelf.
pub struct ShelfModel {
    pub id: u64,
    pub store: Store,
    /// Top-left corner of the panel (global, top-left origin, points).
    pub origin: Point,
    /// Showing the grid of items instead of the stack.
    pub expanded: bool,
}

impl ShelfModel {
    pub fn items(&self) -> &[ShelfItem] {
        self.store.items()
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Index {
    version: u32,
    shelves: Vec<IndexEntry>,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase", default)]
struct IndexEntry {
    id: u64,
    x: f64,
    y: f64,
    expanded: bool,
}

/// All open shelves, oldest first.
pub struct ShelfSet {
    root: PathBuf,
    shelves: Vec<ShelfModel>,
    last_id: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl ShelfSet {
    /// Load the shelves of `data_dir`: the ones in the index that still hold
    /// items. Folders of other (closed or empty) shelves are deleted.
    pub fn open(data_dir: &Path) -> ShelfSet {
        let root = data_dir.join(ROOT_DIR);
        let index: Index = fs::read(root.join(INDEX_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut shelves = vec![];
        for e in index.shelves {
            if shelves.iter().any(|s: &ShelfModel| s.id == e.id) || shelves.len() >= MAX_SHELVES {
                continue;
            }
            let store = Store::open_with(root.join(e.id.to_string()), names());
            if store.items().is_empty() {
                continue;
            }
            shelves.push(ShelfModel { id: e.id, store, origin: Point::new(e.x, e.y), expanded: e.expanded });
        }
        let last_id = shelves.iter().map(|s| s.id).max().unwrap_or(0);
        let set = ShelfSet { root, shelves, last_id };
        set.sweep();
        let _ = set.save_index();
        set
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of shelf `id`.
    pub fn dir_of(&self, id: u64) -> PathBuf {
        self.root.join(id.to_string())
    }

    /// Delete folders of shelves that aren't open.
    fn sweep(&self) {
        let Ok(rd) = fs::read_dir(&self.root) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.chars().all(|c| c.is_ascii_digit()) && !self.shelves.iter().any(|s| s.id.to_string() == name) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.shelves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shelves.is_empty()
    }

    pub fn shelves(&self) -> &[ShelfModel] {
        &self.shelves
    }

    pub fn ids(&self) -> Vec<u64> {
        self.shelves.iter().map(|s| s.id).collect()
    }

    pub fn get(&self, id: u64) -> Option<&ShelfModel> {
        self.shelves.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut ShelfModel> {
        self.shelves.iter_mut().find(|s| s.id == id)
    }

    /// Open a new, empty shelf at `origin`; `None` when `max` shelves are
    /// already open.
    pub fn create(&mut self, origin: Point, max: usize) -> Option<u64> {
        if self.shelves.len() >= max.clamp(1, MAX_SHELVES) {
            return None;
        }
        self.last_id = (self.last_id + 1).max(now_ms());
        let id = self.last_id;
        // A folder left from an earlier run with the same id is stale.
        let _ = fs::remove_dir_all(self.dir_of(id));
        let store = Store::open_with(self.dir_of(id), names());
        self.shelves.push(ShelfModel { id, store, origin, expanded: false });
        let _ = self.save_index();
        Some(id)
    }

    /// Add options for shelf stores: files from other shelves' folders count
    /// as temporary (they are copied), since those folders go away when
    /// their shelf is closed.
    pub fn add_options(&self, copy_temporary_files: bool) -> AddOptions {
        let mut temp_roots = if copy_temporary_files { store::default_temp_roots() } else { vec![] };
        temp_roots.push(self.root.clone());
        AddOptions { copy_temporary_files: true, temp_roots }
    }

    /// Add items to shelf `id`; returns the ids of the items added.
    pub fn add(&mut self, id: u64, items: Vec<NewItem>, opts: &AddOptions) -> Vec<u64> {
        let Some(s) = self.get_mut(id) else { return vec![] };
        let mut ids = vec![];
        for it in items {
            match s.store.add(it, opts) {
                Ok(i) => ids.push(i),
                Err(e) => log::info!("drag-shelf: not added: {e:#}"),
            }
        }
        if !ids.is_empty() {
            if let Err(e) = s.store.save() {
                log::warn!("drag-shelf: saving shelf: {e:#}");
            }
            let _ = self.save_index();
        }
        ids
    }

    /// Change shelf `id`'s store; saves when `f` says it changed.
    pub fn edit(&mut self, id: u64, f: impl FnOnce(&mut Store) -> bool) -> bool {
        let Some(s) = self.get_mut(id) else { return false };
        let changed = f(&mut s.store);
        if changed {
            if let Err(e) = s.store.save() {
                log::warn!("drag-shelf: saving shelf: {e:#}");
            }
        }
        changed
    }

    pub fn remove_items(&mut self, id: u64, items: &[u64]) -> bool {
        self.edit(id, |s| s.remove(items))
    }

    pub fn clear(&mut self, id: u64) -> bool {
        self.edit(id, |s| s.clear())
    }

    /// Close shelf `id` (its folder goes at the next start).
    pub fn close(&mut self, id: u64) -> bool {
        let n = self.shelves.len();
        self.shelves.retain(|s| s.id != id);
        if n == self.shelves.len() {
            return false;
        }
        let _ = fs::remove_file(self.dir_of(id).join(ITEMS_FILE));
        let _ = self.save_index();
        true
    }

    pub fn set_origin(&mut self, id: u64, origin: Point) {
        if let Some(s) = self.get_mut(id) {
            if s.origin != origin {
                s.origin = origin;
                let _ = self.save_index();
            }
        }
    }

    pub fn set_expanded(&mut self, id: u64, on: bool) {
        if let Some(s) = self.get_mut(id) {
            if s.expanded != on {
                s.expanded = on;
                let _ = self.save_index();
            }
        }
    }

    /// Write `index.json` (atomically).
    pub fn save_index(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let index = Index {
            version: 1,
            shelves: self.shelves.iter().map(|s| IndexEntry { id: s.id, x: s.origin.x, y: s.origin.y, expanded: s.expanded }).collect(),
        };
        let tmp = self.root.join(format!("{INDEX_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&index)?)?;
        fs::rename(&tmp, self.root.join(INDEX_FILE))?;
        Ok(())
    }
}

/// What kind of thing an item is, for summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Folder,
    File,
    Link,
    Text,
}

pub fn kind_of(it: &ShelfItem) -> Kind {
    match &it.kind {
        ItemKind::File { path, .. } => {
            let is_bundle = path.extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "app" | "bundle" | "pkg" | "rtfd"));
            if it.is_image_file() {
                Kind::Image
            } else if path.is_dir() && !is_bundle {
                Kind::Folder
            } else {
                Kind::File
            }
        }
        ItemKind::Url { .. } => Kind::Link,
        ItemKind::Text { .. } => Kind::Text,
    }
}

fn kind_word(k: Kind, n: usize) -> String {
    let (one, many) = match k {
        Kind::Image => ("image", "images"),
        Kind::Folder => ("folder", "folders"),
        Kind::File => ("file", "files"),
        Kind::Link => ("link", "links"),
        Kind::Text => ("text snippet", "text snippets"),
    };
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The shelf's caption: (title, subtitle). One item: its name and what it
/// is; several: "4 items" and what they are ("2 images · 1 folder · 1 link").
pub fn summary(items: &[ShelfItem]) -> (String, String) {
    match items {
        [] => ("Drop here".into(), "Files, images, links or text".into()),
        [one] => {
            let what = match kind_of(one) {
                Kind::Image => one.path().and_then(|p| p.extension()).map(|e| format!("{} image", e.to_string_lossy().to_uppercase())).unwrap_or_else(|| "Image".into()),
                Kind::Folder => "Folder".into(),
                Kind::File => match one.path().and_then(|p| p.extension()) {
                    Some(e) => format!("{} file", e.to_string_lossy().to_uppercase()),
                    None => "File".into(),
                },
                Kind::Link => match &one.kind {
                    ItemKind::Url { url, .. } => store::url_host(url).map(|h| format!("Link · {h}")).unwrap_or_else(|| "Link".into()),
                    _ => "Link".into(),
                },
                Kind::Text => match &one.kind {
                    ItemKind::Text { text } => {
                        let words = text.split_whitespace().count();
                        format!("Text · {words} word{}", if words == 1 { "" } else { "s" })
                    }
                    _ => "Text".into(),
                },
            };
            (one.display_name(), what)
        }
        many => {
            let mut counts: Vec<(Kind, usize)> = vec![];
            for it in many {
                let k = kind_of(it);
                match counts.iter_mut().find(|(c, _)| *c == k) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((k, 1)),
                }
            }
            let order = [Kind::Image, Kind::File, Kind::Folder, Kind::Link, Kind::Text];
            counts.sort_by_key(|(k, _)| order.iter().position(|o| o == k));
            let parts: Vec<String> = counts.iter().map(|(k, n)| kind_word(*k, *n)).collect();
            (format!("{} items", many.len()), parts.join(" · "))
        }
    }
}

/// Where a new shelf of `size` goes for the cursor at `cursor`: centered on
/// it (so the drag in progress is right over the shelf), inside `screen`'s
/// visible area, and nudged diagonally off shelves already at that spot.
pub fn placement(cursor: Point, size: (f64, f64), screen: Rect, taken: &[Point]) -> Point {
    let inset = 10.0;
    let clamp = |p: Point| {
        Point::new(
            p.x.clamp(screen.x + inset, (screen.right() - size.0 - inset).max(screen.x + inset)),
            p.y.clamp(screen.y + inset, (screen.bottom() - size.1 - inset).max(screen.y + inset)),
        )
    };
    let mut p = clamp(Point::new(cursor.x - size.0 / 2.0, cursor.y - size.1 / 2.0));
    for _ in 0..MAX_SHELVES {
        if !taken.iter().any(|t| (t.x - p.x).abs() < 12.0 && (t.y - p.y).abs() < 12.0) {
            break;
        }
        let next = clamp(Point::new(p.x + 26.0, p.y + 26.0));
        p = if next == p { clamp(Point::new(p.x - 26.0 * 4.0, p.y - 26.0 * 4.0)) } else { next };
    }
    p
}

/// Keep a moved / resized shelf on screen (at least `keep` points of it
/// stay inside `screen`).
pub fn keep_on_screen(origin: Point, size: (f64, f64), screen: Rect, keep: f64) -> Point {
    Point::new(
        origin.x.clamp(screen.x - size.0 + keep, screen.right() - keep),
        origin.y.clamp(screen.y, screen.bottom() - keep.min(size.1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("drag-shelf-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn shelves_persist_and_empty_ones_are_dropped() {
        let dir = scratch("persist");
        let f = dir.join("report.pdf");
        fs::write(&f, "%PDF").unwrap();
        let mut set = ShelfSet::open(&dir);
        assert!(set.is_empty());
        // (The test folder is a temporary one: no copies here.)
        let opts = set.add_options(false);
        let a = set.create(Point::new(100.0, 200.0), 5).unwrap();
        let b = set.create(Point::new(300.0, 200.0), 5).unwrap();
        let c = set.create(Point::new(500.0, 200.0), 5).unwrap();
        assert!(a < b && b < c, "ids grow");
        assert_eq!(set.add(a, vec![NewItem::File(f.clone()), NewItem::Text("note".into())], &opts).len(), 2);
        assert_eq!(set.add(c, vec![NewItem::Url { url: "https://example.com".into(), title: None }], &opts).len(), 1);
        set.set_origin(a, Point::new(120.0, 220.0));
        set.set_expanded(a, true);
        // Max shelves.
        assert_eq!(set.create(Point::default(), 3), None);
        assert_eq!(set.len(), 3);
        drop(set);

        // b was empty: gone, its folder deleted. a and c come back as they were.
        let mut set = ShelfSet::open(&dir);
        assert_eq!(set.ids(), vec![a, c]);
        let sa = set.get(a).unwrap();
        assert_eq!((sa.origin, sa.expanded, sa.items().len()), (Point::new(120.0, 220.0), true, 2));
        assert_eq!(sa.items()[0].path(), Some(f.as_path()));
        assert!(!set.dir_of(b).exists());
        // Closing: the shelf is gone at once, its folder at the next start.
        assert!(set.close(c) && !set.close(c));
        assert!(set.dir_of(c).exists());
        assert!(set.remove_items(a, &[set.get(a).unwrap().items()[1].id]));
        drop(set);
        let set = ShelfSet::open(&dir);
        assert_eq!(set.ids(), vec![a]);
        assert_eq!(set.get(a).unwrap().items().len(), 1);
        assert!(!set.dir_of(c).exists());
        // A corrupt index starts empty instead of failing.
        fs::write(dir.join(ROOT_DIR).join(INDEX_FILE), "{nope").unwrap();
        assert!(ShelfSet::open(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_from_other_shelves_are_copied() {
        let dir = scratch("between");
        let mut set = ShelfSet::open(&dir);
        let opts = set.add_options(false);
        let a = set.create(Point::default(), 5).unwrap();
        let b = set.create(Point::default(), 5).unwrap();
        // An image dropped without a file becomes a copy in a's folder.
        let ids = set.add(a, vec![NewItem::Data { bytes: vec![1, 2, 3], name: "pic.png".into() }], &opts);
        let path = set.get(a).unwrap().store.get(ids[0]).unwrap().path().unwrap().to_path_buf();
        assert!(path.starts_with(set.dir_of(a)));
        // Dragged onto b: b gets its own copy (a's folder goes with a).
        set.add(b, vec![NewItem::File(path.clone())], &opts);
        let pb = set.get(b).unwrap().items()[0].path().unwrap().to_path_buf();
        assert!(pb.starts_with(set.dir_of(b)), "{}", pb.display());
        assert_eq!(fs::read(&pb).unwrap(), vec![1, 2, 3]);
        // Re-adding to a itself is the same item.
        assert_eq!(set.add(a, vec![NewItem::File(path)], &opts), ids);
        assert!(set.clear(a) && !set.clear(a));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn summaries() {
        let dir = scratch("summary");
        let folder = dir.join("Photos");
        fs::create_dir_all(&folder).unwrap();
        let mk = |id, kind| ShelfItem { id, kind, added_at: 0 };
        let file = |p: PathBuf| ItemKind::File { path: p, original: None, owned: false };
        let img = mk(1, file(dir.join("Beach.heic")));
        let pdf = mk(2, file(dir.join("Report.pdf")));
        let fold = mk(3, file(folder.clone()));
        let link = mk(4, ItemKind::Url { url: "https://www.apple.com/mac".into(), title: None });
        let text = mk(5, ItemKind::Text { text: "Call Ada about the launch".into() });
        assert_eq!(summary(&[]).0, "Drop here");
        assert_eq!(summary(std::slice::from_ref(&img)), ("Beach.heic".into(), "HEIC image".into()));
        assert_eq!(summary(std::slice::from_ref(&pdf)).1, "PDF file");
        assert_eq!(summary(std::slice::from_ref(&fold)).1, "Folder");
        assert_eq!(summary(std::slice::from_ref(&link)), ("apple.com".into(), "Link · apple.com".into()));
        assert_eq!(summary(std::slice::from_ref(&text)).1, "Text · 5 words");
        let all = [text.clone(), img.clone(), link.clone(), pdf, fold, mk(6, file(dir.join("b.png")))];
        assert_eq!(summary(&all), ("6 items".into(), "2 images · 1 file · 1 folder · 1 link · 1 text snippet".into()));
        assert_eq!(kind_of(&mk(7, file(dir.join("Safari.app")))), Kind::File);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn placement_centers_clamps_and_cascades() {
        let screen = Rect::new(0.0, 25.0, 1512.0, 920.0);
        let size = (208.0, 232.0);
        let p = placement(Point::new(700.0, 500.0), size, screen, &[]);
        assert_eq!(p, Point::new(596.0, 384.0));
        // At a corner: inside the visible area.
        let c = placement(Point::new(5.0, 30.0), size, screen, &[]);
        assert_eq!(c, Point::new(10.0, 35.0));
        let c2 = placement(Point::new(1510.0, 940.0), size, screen, &[]);
        assert_eq!((c2.x + size.0, c2.y + size.1), (1502.0, 935.0));
        // On top of an existing shelf: nudged.
        let n = placement(Point::new(700.0, 500.0), size, screen, &[p]);
        assert_eq!(n, Point::new(622.0, 410.0));
        let n2 = placement(Point::new(700.0, 500.0), size, screen, &[p, n]);
        assert_eq!(n2, Point::new(648.0, 436.0));
        // Off-screen after a move: pulled back so part of it can be grabbed.
        let k = keep_on_screen(Point::new(-500.0, -40.0), size, screen, 60.0);
        assert_eq!(k, Point::new(-148.0, 25.0));
    }
}
