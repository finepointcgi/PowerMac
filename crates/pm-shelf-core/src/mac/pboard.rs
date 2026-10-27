//! Pasteboards (drops, drag-out writers, copy), Quick Look thumbnails,
//! file promises and NSWorkspace actions.

use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSData, NSSize, NSString};

use super::ffi::*;
use crate::store::{ItemKind, NewItem, ShelfItem};

pub use crate::uti::*;

fn strs(arr: Option<Retained<NSArray<NSString>>>) -> Vec<String> {
    arr.map(|a| a.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
}

pub fn types_of(pb: &AnyObject) -> Vec<String> {
    strs(unsafe { msg_send![pb, types] })
}

pub fn drag_pasteboard() -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSPasteboard), pasteboardWithName: &*ns("Apple CFPasteboard drag")] }
}

pub fn change_count(pb: &AnyObject) -> isize {
    unsafe { msg_send![pb, changeCount] }
}

fn string_for(item: &AnyObject, ty: &str) -> Option<String> {
    let s: Option<Retained<NSString>> = unsafe { msg_send![item, stringForType: &*ns(ty)] };
    s.map(|s| s.to_string())
}

fn data_for(item: &AnyObject, ty: &str) -> Option<Vec<u8>> {
    let d: Option<Retained<NSData>> = unsafe { msg_send![item, dataForType: &*ns(ty)] };
    d.map(|d| d.to_vec())
}

/// `file://` URL string (including file reference URLs) to a path.
pub fn file_url_to_path(s: &str) -> Option<PathBuf> {
    unsafe {
        let url: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*ns(s)];
        let url = url?;
        let fp: Option<Retained<AnyObject>> = msg_send![&*url, filePathURL];
        let path: Option<Retained<NSString>> = msg_send![&*fp?, path];
        path.map(|p| PathBuf::from(p.to_string()))
    }
}

fn tiff_to_png(tiff: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let data = NSData::with_bytes(tiff);
        let rep: Option<Retained<AnyObject>> = msg_send![class!(NSBitmapImageRep), imageRepWithData: &*data];
        let props: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        let png: Option<Retained<NSData>> = msg_send![&*rep?, representationUsingType: 4usize, properties: &*props];
        png.map(|d| d.to_vec())
    }
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}")
}

/// What one pasteboard item contributes, by preference: a file, image data,
/// a URL, text. `None` if it only carries a file promise (or nothing usable).
pub fn classify(types: &[String], get_str: &dyn Fn(&str) -> Option<String>, get_data: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<NewItem> {
    let has = |t: &str| types.iter().any(|x| x == t);
    if has(T_FILE_URL) {
        if let Some(p) = get_str(T_FILE_URL).and_then(|s| file_url_to_path(&s)) {
            return Some(NewItem::File(p));
        }
    }
    for (t, ext) in [(T_PNG, "png"), (T_JPEG, "jpg"), (T_HEIC, "heic")] {
        if has(t) {
            if let Some(bytes) = get_data(t) {
                return Some(NewItem::Data { bytes, name: format!("Image {}.{ext}", stamp()) });
            }
        }
    }
    if has(T_TIFF) {
        if let Some(png) = get_data(T_TIFF).and_then(|t| tiff_to_png(&t)) {
            return Some(NewItem::Data { bytes: png, name: format!("Image {}.png", stamp()) });
        }
    }
    if has(T_URL) {
        if let Some(url) = get_str(T_URL).filter(|u| !u.trim().is_empty()) {
            return Some(NewItem::Url { url: url.trim().to_string(), title: get_str(T_URL_NAME) });
        }
    }
    if has(T_TEXT) {
        if let Some(t) = get_str(T_TEXT).filter(|t| !t.trim().is_empty()) {
            return Some(NewItem::Text(t));
        }
    }
    None
}

/// Read everything usable from a pasteboard. Returns the items and
/// whether some items only had file promises.
pub fn read(pb: &AnyObject) -> (Vec<NewItem>, bool) {
    let items: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![pb, pasteboardItems] };
    let mut out = vec![];
    let mut promises = false;
    for it in items.map(|a| a.to_vec()).unwrap_or_default() {
        let types = strs(unsafe { msg_send![&*it, types] });
        match classify(&types, &|t| string_for(&it, t), &|t| data_for(&it, t)) {
            Some(n) => out.push(n),
            None => promises |= types.iter().any(|t| t == T_PROMISE || t == T_PROMISE_URL || t == T_PROMISE_TYPE),
        }
    }
    (out, promises)
}

/// Ask the drag source to write its promised files into `dest`;
/// `on_file(path)` runs on the main thread for each file as it arrives.
pub fn receive_promises(pb: &AnyObject, dest: PathBuf, on_file: impl Fn(PathBuf) + Clone + 'static) {
    let Some(cls) = AnyClass::get(c"NSFilePromiseReceiver") else { return };
    unsafe {
        let cls_obj: &AnyObject = &*(cls as *const AnyClass as *const AnyObject);
        let classes = NSArray::from_slice(&[cls_obj]);
        let receivers: Option<Retained<NSArray<AnyObject>>> = msg_send![pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()];
        let receivers = receivers.map(|a| a.to_vec()).unwrap_or_default();
        if receivers.is_empty() {
            return;
        }
        if std::fs::create_dir_all(&dest).is_err() {
            return;
        }
        let dest_url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*ns(&dest.to_string_lossy()), isDirectory: true];
        let queue: Retained<AnyObject> = msg_send![class!(NSOperationQueue), mainQueue];
        let opts: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        for r in receivers {
            let on_file = on_file.clone();
            let block = RcBlock::new(move |url: *mut AnyObject, err: *mut AnyObject| {
                if !err.is_null() || url.is_null() {
                    log::info!("shelf: file promise failed");
                    return;
                }
                let path: Option<Retained<NSString>> = msg_send![url, path];
                if let Some(p) = path {
                    on_file(PathBuf::from(p.to_string()));
                }
            });
            let _: () = msg_send![&*r, receivePromisedFilesAtDestination: &*dest_url, options: &*opts, operationQueue: &*queue, reader: &*block];
        }
    }
}

pub fn file_url(p: &Path) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSURL), fileURLWithPath: &*ns(&p.to_string_lossy())] }
}

fn web_url(s: &str) -> Option<Retained<AnyObject>> {
    unsafe { msg_send![class!(NSURL), URLWithString: &*ns(s)] }
}

/// An object that can go on a pasteboard / into a share sheet for `item`.
pub fn writer(item: &ShelfItem) -> Retained<AnyObject> {
    match &item.kind {
        ItemKind::File { path, .. } => file_url(path),
        ItemKind::Url { url, .. } => web_url(url).unwrap_or_else(|| Retained::into_super(Retained::into_super(ns(url)))),
        ItemKind::Text { text } => Retained::into_super(Retained::into_super(ns(text))),
    }
}

pub fn copy_to_clipboard(items: &[ShelfItem]) {
    if items.is_empty() {
        return;
    }
    unsafe {
        let pb: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![&*pb, clearContents];
        let ws: Vec<Retained<AnyObject>> = items.iter().map(writer).collect();
        let arr = NSArray::from_retained_slice(&ws);
        let _: bool = msg_send![&*pb, writeObjects: &*arr];
    }
}

/// The first item of the general pasteboard (for the clipboard tile).
pub fn clipboard_item() -> (isize, Option<ItemKind>) {
    unsafe {
        let pb: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
        let count = change_count(&pb);
        let items: Option<Retained<NSArray<AnyObject>>> = msg_send![&*pb, pasteboardItems];
        let first = items.and_then(|a| a.to_vec().into_iter().next());
        let kind = first.and_then(|it| {
            let types = strs(msg_send![&*it, types]);
            let has = |t: &str| types.iter().any(|x| x == t);
            if has(T_FILE_URL) {
                return string_for(&it, T_FILE_URL).and_then(|s| file_url_to_path(&s)).map(|p| ItemKind::File { path: p, original: None, owned: false });
            }
            if has(T_URL) {
                if let Some(u) = string_for(&it, T_URL) {
                    return Some(ItemKind::Url { url: u, title: None });
                }
            }
            string_for(&it, T_TEXT).filter(|t| !t.trim().is_empty()).map(|text| match crate::store::as_url(&text) {
                Some(url) => ItemKind::Url { url, title: None },
                None => ItemKind::Text { text },
            })
        });
        (count, kind)
    }
}

pub fn open(items: &[ShelfItem]) {
    let ws: Retained<AnyObject> = unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] };
    let mut texts = vec![];
    for it in items {
        match &it.kind {
            ItemKind::File { path, .. } => unsafe {
                let _: bool = msg_send![&*ws, openURL: &*file_url(path)];
            },
            ItemKind::Url { url, .. } => {
                if let Some(u) = web_url(url) {
                    let _: bool = unsafe { msg_send![&*ws, openURL: &*u] };
                }
            }
            ItemKind::Text { .. } => texts.push(it.clone()),
        }
    }
    // Text has nowhere to open: copy it instead.
    copy_to_clipboard(&texts);
}

pub fn reveal(items: &[ShelfItem]) {
    let urls: Vec<Retained<AnyObject>> = items.iter().filter_map(|i| i.path()).map(file_url).collect();
    if urls.is_empty() {
        return;
    }
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let arr = NSArray::from_retained_slice(&urls);
        let _: () = msg_send![&*ws, activateFileViewerSelectingURLs: &*arr];
    }
}

/// The Finder icon of a file (any size, vector).
pub fn file_icon(p: &Path) -> Retained<AnyObject> {
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![&*ws, iconForFile: &*ns(&p.to_string_lossy())]
    }
}

struct SendImage(Image);
unsafe impl Send for SendImage {}

/// Ask Quick Look for a thumbnail; `done(id, image)` runs on the main thread.
pub fn request_thumbnail(id: u64, path: &Path, size: f64, scale: f64, done: fn(u64, Image, bool)) {
    let (Some(gen_cls), Some(req_cls)) = (AnyClass::get(c"QLThumbnailGenerator"), AnyClass::get(c"QLThumbnailGenerationRequest")) else { return };
    unsafe {
        let url = file_url(path);
        let a: Allocated<AnyObject> = msg_send![req_cls, alloc];
        let req: Retained<AnyObject> =
            msg_send![a, initWithFileAtURL: &*url, size: NSSize::new(size, size), scale: scale, representationTypes: usize::MAX];
        let gen: Retained<AnyObject> = msg_send![gen_cls, sharedGenerator];
        let block = RcBlock::new(move |rep: *mut AnyObject, _err: *mut AnyObject| {
            if rep.is_null() {
                return;
            }
            let cg: CgImage = msg_send![rep, CGImage];
            // QLThumbnailRepresentationTypeIcon = 0
            let ty: isize = msg_send![rep, type];
            if let Some(img) = Image::retain(cg.0) {
                let img = SendImage(img);
                dispatch_main(move || {
                    let img = img;
                    done(id, img.0, ty == 0)
                });
            }
        });
        let _: () = msg_send![&*gen, generateBestRepresentationForRequest: &*req, completionHandler: &*block];
    }
}

pub fn haptic() {
    unsafe {
        let p: Option<Retained<AnyObject>> = msg_send![class!(NSHapticFeedbackManager), defaultPerformer];
        if let Some(p) = p {
            // NSHapticFeedbackPatternAlignment, NSHapticFeedbackPerformanceTimeNow
            let _: () = msg_send![&*p, performFeedbackPattern: 1isize, performanceTime: 1usize];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("pm-shelf-core-pb-{}", std::process::id()));
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
