//! Shelf contents and their persistence.
//!
//! Items are kept in `<data_dir>/notch-shelf.json`. Files that the shelf
//! owns (copies of files dragged from temporary locations, images and other
//! raw data dropped without a file) live in `<data_dir>/notch-shelf-files/<id>/`.
//! Copies whose item was removed are deleted at the next start, so a copy
//! that was just dragged out is never pulled from under the receiver.
//!
//! The item model and the store itself live in `pm_shelf_core::store`
//! (shared with the Drag Shelf); this module fixes the notch's file names.

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

pub use pm_shelf_core::store::{
    as_url, default_temp_roots, is_image_path, is_temporary_path, url_host, AddOptions, ItemKind, NewItem, ShelfItem, StoreNames, MAX_ITEMS, MAX_TEXT,
};

pub const STORE_FILE: &str = "notch-shelf.json";
pub const FILES_DIR: &str = "notch-shelf-files";

/// The notch shelf's store (`notch-shelf.json` + `notch-shelf-files/`).
pub struct Store(pm_shelf_core::store::Store);

impl Store {
    /// Load the shelf from `data_dir`, dropping items whose file is gone
    /// and deleting owned copies nothing refers to any more.
    pub fn open(data_dir: impl Into<PathBuf>) -> Store {
        Store(pm_shelf_core::store::Store::open_with(data_dir, StoreNames::new(STORE_FILE, FILES_DIR, "notch-shelf")))
    }
}

impl std::ops::Deref for Store {
    type Target = pm_shelf_core::store::Store;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Store {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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
