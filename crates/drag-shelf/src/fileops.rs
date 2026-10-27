//! "Copy to…" / "Move to…": put a shelf's items into a folder. Files and
//! folders are copied or moved (Finder-style "name 2.ext" when the name is
//! taken; across volumes a move is a copy and delete); links become
//! `.webloc` files (`.url` on Windows) and text snippets `.txt` files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use pm_shelf_core::store::{copy_recursive, sanitize_name, truncate_chars, url_host, ItemKind, ShelfItem};

/// What happened to one item.
#[derive(Debug)]
pub struct Outcome {
    pub id: u64,
    /// Where it is now (`Err`: why not).
    pub result: io::Result<PathBuf>,
    /// The item's file itself moved (the shelf should point at `result`).
    pub moved: bool,
}

/// `dir/name`, or `dir/name 2.ext`, `dir/name 3.ext`, … if that exists.
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !s.is_empty() => (s.to_string_lossy().into_owned(), format!(".{}", e.to_string_lossy())),
        _ => (name.to_string(), String::new()),
    };
    (2..10_000).map(|n| dir.join(format!("{stem} {n}{ext}"))).find(|c| fs::symlink_metadata(c).is_err()).unwrap_or(first)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The file a link or text item becomes: (file name, contents).
pub fn as_file(kind: &ItemKind) -> Option<(String, Vec<u8>)> {
    match kind {
        ItemKind::File { .. } => None,
        ItemKind::Url { url, title } => {
            let base = title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).or_else(|| url_host(url).map(str::to_string)).unwrap_or_else(|| "Link".into());
            let base = sanitize_name(&truncate_chars(&base, 60));
            if cfg!(windows) {
                Some((format!("{base}.url"), format!("[InternetShortcut]\r\nURL={url}\r\n").into_bytes()))
            } else {
                let plist = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>URL</key>\n\t<string>{}</string>\n</dict>\n</plist>\n",
                    xml_escape(url)
                );
                Some((format!("{base}.webloc"), plist.into_bytes()))
            }
        }
        ItemKind::Text { text } => {
            let words: Vec<&str> = text.split_whitespace().take(6).collect();
            let base = if words.is_empty() { "Text".to_string() } else { sanitize_name(&truncate_chars(&words.join(" "), 40)) };
            Some((format!("{base}.txt"), text.as_bytes().to_vec()))
        }
    }
}

fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Another volume (or a rename that isn't allowed): copy, then
            // delete the original only once the copy is complete.
            copy_recursive(from, to)?;
            let md = fs::symlink_metadata(from)?;
            if md.is_dir() {
                fs::remove_dir_all(from)
            } else {
                fs::remove_file(from)
            }
        }
    }
}

/// Copy (or with `mv`, move) `items` into the folder `dest`.
pub fn put_into(items: &[ShelfItem], dest: &Path, mv: bool) -> Vec<Outcome> {
    items
        .iter()
        .map(|it| {
            let (result, moved) = match &it.kind {
                ItemKind::File { path, .. } => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
                    if path.parent() == Some(dest) && mv {
                        // Already there.
                        (Ok(path.clone()), false)
                    } else if dest.starts_with(path) {
                        (Err(io::Error::new(io::ErrorKind::InvalidInput, format!("can't put {name} into itself"))), false)
                    } else {
                        let to = unique_dest(dest, &name);
                        let r = if mv { move_path(path, &to) } else { copy_recursive(path, &to) };
                        (r.map(|_| to), mv)
                    }
                }
                other => match as_file(other) {
                    Some((name, bytes)) => {
                        let to = unique_dest(dest, &name);
                        (fs::write(&to, bytes).map(|_| to), false)
                    }
                    None => (Err(io::Error::other("nothing to write")), false),
                },
            };
            let moved = moved && result.is_ok();
            Outcome { id: it.id, result, moved }
        })
        .collect()
}

/// "Copied 3 items to Desktop" / "Moved 1 item to Documents (1 failed: …)".
pub fn report(outcomes: &[Outcome], dest: &Path, mv: bool) -> String {
    let ok = outcomes.iter().filter(|o| o.result.is_ok()).count();
    let folder = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dest.display().to_string());
    let mut s = format!("{} {ok} item{} to {folder}", if mv { "Moved" } else { "Copied" }, if ok == 1 { "" } else { "s" });
    let failed: Vec<String> = outcomes.iter().filter_map(|o| o.result.as_ref().err().map(|e| e.to_string())).collect();
    if !failed.is_empty() {
        s.push_str(&format!(" ({} failed: {})", failed.len(), failed[0]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("drag-shelf-ops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn file(id: u64, p: PathBuf) -> ShelfItem {
        ShelfItem { id, kind: ItemKind::File { path: p, original: None, owned: false }, added_at: 0 }
    }

    #[test]
    fn unique_names() {
        let d = scratch("unique");
        assert_eq!(unique_dest(&d, "a.txt"), d.join("a.txt"));
        fs::write(d.join("a.txt"), "").unwrap();
        assert_eq!(unique_dest(&d, "a.txt"), d.join("a 2.txt"));
        fs::write(d.join("a 2.txt"), "").unwrap();
        assert_eq!(unique_dest(&d, "a.txt"), d.join("a 3.txt"));
        fs::create_dir(d.join("Folder")).unwrap();
        assert_eq!(unique_dest(&d, "Folder"), d.join("Folder 2"));
        fs::write(d.join(".hidden"), "").unwrap();
        assert_eq!(unique_dest(&d, ".hidden"), d.join(".hidden 2"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_and_move_items() {
        let d = scratch("ops");
        let src = d.join("src");
        let dest = d.join("dest");
        fs::create_dir_all(src.join("Album")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("a.txt"), "A").unwrap();
        fs::write(src.join("Album/p.png"), "P").unwrap();
        fs::write(dest.join("a.txt"), "old").unwrap();
        let items = vec![
            file(1, src.join("a.txt")),
            file(2, src.join("Album")),
            ShelfItem { id: 3, kind: ItemKind::Url { url: "https://example.com/?a=1&b=<2>".into(), title: Some("Example".into()) }, added_at: 0 },
            ShelfItem { id: 4, kind: ItemKind::Text { text: "Buy milk and eggs\nand bread".into() }, added_at: 0 },
            file(5, src.join("missing.txt")),
        ];
        let out = put_into(&items, &dest, false);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].result.as_ref().unwrap(), &dest.join("a 2.txt"), "the existing a.txt is kept");
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(dest.join("Album/p.png")).unwrap(), "P");
        assert!(src.join("a.txt").exists(), "copied, not moved");
        let link = out[2].result.as_ref().unwrap();
        if cfg!(windows) {
            assert!(link.ends_with("Example.url"));
        } else {
            assert!(link.ends_with("Example.webloc"));
            assert!(fs::read_to_string(link).unwrap().contains("<string>https://example.com/?a=1&amp;b=&lt;2&gt;</string>"));
        }
        let txt = out[3].result.as_ref().unwrap();
        assert!(txt.ends_with("Buy milk and eggs and bread.txt"), "{}", txt.display());
        assert!(out[4].result.is_err());
        assert!(out.iter().all(|o| !o.moved));
        assert!(report(&out, &dest, false).starts_with("Copied 4 items to dest (1 failed"));

        // Move: the files go, the shelf learns where they are.
        let dest2 = d.join("dest2");
        fs::create_dir_all(&dest2).unwrap();
        let out = put_into(&items[..2], &dest2, true);
        assert!(out.iter().all(|o| o.moved && o.result.is_ok()));
        assert!(!src.join("a.txt").exists() && !src.join("Album").exists());
        assert_eq!(fs::read_to_string(dest2.join("Album/p.png")).unwrap(), "P");
        assert_eq!(report(&out, &dest2, true), "Moved 2 items to dest2");
        // Moving into the folder it is in: nothing to do.
        let again = put_into(&[file(1, dest2.join("a.txt"))], &dest2, true);
        assert!(again[0].result.is_ok() && !again[0].moved);
        // Into itself: refused.
        let inside = put_into(&[file(2, dest2.join("Album"))], &dest2.join("Album"), false);
        assert!(inside[0].result.is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
