//! Pasteboards (drops, drag-out writers, copy), Quick Look thumbnails,
//! file promises and NSWorkspace actions: shared with the Drag Shelf in
//! `pm_shelf_core::mac::pboard`.

use std::path::PathBuf;
use std::sync::Arc;

use objc2::runtime::AnyObject;

#[allow(unused_imports)]
pub use pm_shelf_core::mac::pboard::*;

use crate::store::NewItem;
use crate::Shared;

/// Ask the drag source to write its promised files into the shelf folder;
/// they are added as they arrive.
pub fn receive_promises(pb: &AnyObject, shared: Arc<Shared>, dest: PathBuf, on_done: impl Fn() + Clone + 'static) {
    pm_shelf_core::mac::pboard::receive_promises(pb, dest, move |p| {
        shared.add(vec![NewItem::File(p)]);
        on_done();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ItemKind, ShelfItem};
    use objc2::rc::Retained;
    use objc2::{class, msg_send};
    use objc2_foundation::NSArray;
    use std::path::Path;

    #[test]
    fn classify_prefers_files_then_images_then_urls_then_text() {
        let s = |m: Vec<(&'static str, &'static str)>| move |t: &str| m.iter().find(|(k, _)| *k == t).map(|(_, v)| v.to_string());
        let no_data = |_: &str| None;
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let got = classify(&t(&[T_URL, T_TEXT, T_URL_NAME]), &s(vec![(T_URL, "https://x.y/"), (T_URL_NAME, "X"), (T_TEXT, "https://x.y/")]), &no_data);
        assert_eq!(got, Some(NewItem::Url { url: "https://x.y/".into(), title: Some("X".into()) }));
        let got = classify(&t(&[T_TEXT]), &s(vec![(T_TEXT, "hi")]), &no_data);
        assert_eq!(got, Some(NewItem::Text("hi".into())));
        let got = classify(&t(&[T_PNG, T_URL]), &s(vec![(T_URL, "https://x.y/a.png")]), &|_| Some(vec![1]));
        assert!(matches!(got, Some(NewItem::Data { ref bytes, ref name }) if bytes == &vec![1] && name.ends_with(".png")));
        let got = classify(&t(&[T_FILE_URL]), &s(vec![(T_FILE_URL, "file:///tmp/")]), &no_data);
        assert_eq!(got, Some(NewItem::File(PathBuf::from("/tmp"))));
        assert_eq!(classify(&t(&[T_PROMISE]), &s(vec![]), &no_data), None);
        assert_eq!(classify(&t(&[T_TEXT]), &s(vec![(T_TEXT, "  ")]), &no_data), None);
        assert!(accepts(&t(&["public.tiff"])) && !accepts(&t(&["com.example.private"])));
    }

    #[test]
    fn reads_a_real_pasteboard() {
        let dir = std::env::temp_dir().join(format!("notch-shelf-pb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a file.txt");
        std::fs::write(&f, "x").unwrap();
        let shelf = ShelfItem { id: 1, kind: ItemKind::File { path: f.clone(), original: None, owned: false }, added_at: 0 };
        let link = ShelfItem { id: 2, kind: ItemKind::Url { url: "https://example.com/a".into(), title: None }, added_at: 0 };
        let text = ShelfItem { id: 3, kind: ItemKind::Text { text: "hello shelf".into() }, added_at: 0 };
        unsafe {
            let pb: Retained<AnyObject> = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
            let _: isize = msg_send![&*pb, clearContents];
            let ws: Vec<Retained<AnyObject>> = [&shelf, &link, &text].into_iter().map(writer).collect();
            let arr = NSArray::from_retained_slice(&ws);
            let ok: bool = msg_send![&*pb, writeObjects: &*arr];
            assert!(ok);
            assert!(accepts(&types_of(&pb)));
            let (items, promises) = read(&pb);
            let _: () = msg_send![&*pb, releaseGlobally];
            assert!(!promises);
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap();
            match &items[0] {
                NewItem::File(p) => assert_eq!(canon(p), canon(&f)),
                other => panic!("{other:?}"),
            }
            assert_eq!(items[1], NewItem::Url { url: "https://example.com/a".into(), title: None });
            assert_eq!(items[2], NewItem::Text("hello shelf".into()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
