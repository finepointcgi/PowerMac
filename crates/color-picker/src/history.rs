//! The color history: `color-picker-history.json` in the data directory, a
//! JSON array of `#RRGGBB` strings, newest first. Shared by the picker
//! (running in the tray app) and the editor (its own process); writes are
//! atomic (temp file + rename).

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::Rgb;

pub const FILE_NAME: &str = "color-picker-history.json";

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// The saved colors, newest first (empty if there is no file or it's unreadable).
pub fn load(data_dir: &Path) -> Vec<Rgb> {
    let Ok(s) = std::fs::read_to_string(path(data_dir)) else {
        return Vec::new();
    };
    // Skip entries that don't parse instead of losing the whole history.
    let raw: Vec<String> = serde_json::from_str(&s).unwrap_or_default();
    raw.iter().filter_map(|h| Rgb::from_hex(h)).collect()
}

pub fn save(data_dir: &Path, colors: &[Rgb]) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let p = path(data_dir);
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(colors)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Put `c` first (moving it up if it is already there) and keep at most `limit`.
pub fn push(colors: &mut Vec<Rgb>, c: Rgb, limit: usize) {
    colors.retain(|x| *x != c);
    colors.insert(0, c);
    colors.truncate(limit.max(1));
}

/// [`push`] into the saved history.
pub fn add(data_dir: &Path, c: Rgb, limit: usize) -> anyhow::Result<Vec<Rgb>> {
    let mut v = load(data_dir);
    push(&mut v, c, limit);
    save(data_dir, &v)?;
    Ok(v)
}

/// Modification time of the history file (to notice changes from the other process).
pub fn stamp(data_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path(data_dir))
        .and_then(|m| m.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupes_and_limits() {
        let mut v = vec![];
        for i in 0..25u8 {
            push(&mut v, Rgb::new(i, 0, 0), 20);
        }
        assert_eq!(v.len(), 20);
        assert_eq!(v[0], Rgb::new(24, 0, 0));
        push(&mut v, Rgb::new(10, 0, 0), 20);
        assert_eq!(v[0], Rgb::new(10, 0, 0));
        assert_eq!(v.len(), 20);
        assert_eq!(v.iter().filter(|c| **c == Rgb::new(10, 0, 0)).count(), 1);
    }

    #[test]
    fn round_trip_on_disk() {
        let d = std::env::temp_dir().join(format!("cp-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(load(&d).is_empty());
        add(&d, Rgb::new(1, 2, 3), 20).unwrap();
        add(&d, Rgb::new(4, 5, 6), 20).unwrap();
        assert_eq!(load(&d), vec![Rgb::new(4, 5, 6), Rgb::new(1, 2, 3)]);
        // Bad entries are skipped.
        std::fs::write(path(&d), r##"["#010203", "nope", "#FFFFFF"]"##).unwrap();
        assert_eq!(load(&d), vec![Rgb::new(1, 2, 3), Rgb::new(255, 255, 255)]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
