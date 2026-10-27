//! Pasteboard type identifiers the shelves read and register for
//! (platform-neutral strings, so the drag-detection logic can be tested
//! everywhere).

pub const T_FILE_URL: &str = "public.file-url";
pub const T_URL: &str = "public.url";
pub const T_URL_NAME: &str = "public.url-name";
pub const T_TEXT: &str = "public.utf8-plain-text";
pub const T_PNG: &str = "public.png";
pub const T_TIFF: &str = "public.tiff";
pub const T_JPEG: &str = "public.jpeg";
pub const T_HEIC: &str = "public.heic";
pub const T_PROMISE: &str = "com.apple.NSFilePromiseItemMetaData";
pub const T_PROMISE_URL: &str = "com.apple.pasteboard.promised-file-url";
pub const T_PROMISE_TYPE: &str = "com.apple.pasteboard.promised-file-content-type";
pub const T_LEGACY_FILES: &str = "NSFilenamesPboardType";

/// Types the shelf registers for and that make a drag count as "shelf-able".
pub const ACCEPTED: &[&str] = &[T_FILE_URL, T_URL, T_TEXT, T_PNG, T_TIFF, T_JPEG, T_HEIC, T_PROMISE, T_PROMISE_URL, T_PROMISE_TYPE, T_LEGACY_FILES];

pub fn accepts(types: &[String]) -> bool {
    types.iter().any(|t| ACCEPTED.contains(&t.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_types() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(accepts(&t(&["dyn.x", T_FILE_URL])));
        assert!(accepts(&t(&[T_PROMISE])) && accepts(&t(&[T_LEGACY_FILES])));
        assert!(!accepts(&t(&["com.apple.finder.node", "dyn.ah62d4rv4gu8y"])));
        assert!(!accepts(&[]));
    }
}
