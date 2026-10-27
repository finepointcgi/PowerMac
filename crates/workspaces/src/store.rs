//! `workspaces.json` in the data directory: load, atomic save, lookups.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::Workspace;

pub const FILE_NAME: &str = "workspaces.json";
pub const FORMAT_VERSION: u32 = 1;

/// The file's top level.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspacesFile {
    pub version: u32,
    pub workspaces: Vec<Workspace>,
}

pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// A GUID-shaped random id like PowerToys' (`{8C1D…}`).
pub fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut words = [0u64; 2];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_usize(i);
        h.write_u32(std::process::id());
        *w = h.finish();
    }
    let b: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let hex = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02X}")).collect::<String>();
    format!("{{{}-{}-{}-{}-{}}}", hex(0..4), hex(4..6), hex(6..8), hex(8..10), hex(10..16))
}

/// Load the workspaces (empty if the file doesn't exist).
pub fn load(data_dir: &Path) -> Result<Vec<Workspace>> {
    let p = path_in(data_dir);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let f: WorkspacesFile = serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
    Ok(f.workspaces)
}

/// Save atomically (write a temp file next to it, fsync, rename).
pub fn save(data_dir: &Path, workspaces: &[Workspace]) -> Result<()> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let f = WorkspacesFile { version: FORMAT_VERSION, workspaces: workspaces.to_vec() };
    let text = serde_json::to_string_pretty(&f)?;
    write_atomic(&path_in(data_dir), text.as_bytes())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| anyhow!("no parent directory for {}", path.display()))?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Modification stamp (to notice changes made by another process).
pub fn stamp(data_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path_in(data_dir)).and_then(|m| m.modified()).ok()
}

/// A workspace by id, else by name (case-insensitive), else by a unique
/// name prefix.
pub fn find<'a>(list: &'a [Workspace], name_or_id: &str) -> Option<&'a Workspace> {
    let q = name_or_id.trim();
    let qb = q.trim_start_matches('{').trim_end_matches('}');
    if let Some(w) = list.iter().find(|w| w.id == q || w.id.trim_start_matches('{').trim_end_matches('}').eq_ignore_ascii_case(qb)) {
        return Some(w);
    }
    if let Some(w) = list.iter().find(|w| w.name.eq_ignore_ascii_case(q)) {
        return Some(w);
    }
    let lq = q.to_lowercase();
    let mut it = list.iter().filter(|w| !lq.is_empty() && w.name.to_lowercase().starts_with(&lq));
    match (it.next(), it.next()) {
        (Some(w), None) => Some(w),
        _ => None,
    }
}

/// Insert or replace (by id) and save.
pub fn upsert(data_dir: &Path, ws: Workspace) -> Result<()> {
    let mut list = load(data_dir)?;
    match list.iter_mut().find(|w| w.id == ws.id) {
        Some(w) => *w = ws,
        None => list.push(ws),
    }
    save(data_dir, &list)
}

/// Remove by id and save; `false` if there was none.
pub fn remove(data_dir: &Path, id: &str) -> Result<bool> {
    let mut list = load(data_dir)?;
    let n = list.len();
    list.retain(|w| w.id != id);
    if list.len() == n {
        return Ok(false);
    }
    save(data_dir, &list)?;
    Ok(true)
}

/// A name not used yet: "Workspace", "Workspace 2", …
pub fn unique_name(list: &[Workspace], base: &str) -> String {
    let base = if base.trim().is_empty() { "Workspace" } else { base.trim() };
    if !list.iter().any(|w| w.name.eq_ignore_ascii_case(base)) {
        return base.to_string();
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !list.iter().any(|w| w.name.eq_ignore_ascii_case(n))).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Monitor, Rect};
    use crate::model::*;

    pub(crate) fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-test-{tag}-{}-{}", std::process::id(), new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Workspace {
        Workspace {
            id: new_id(),
            name: "Coding".into(),
            creation_time: 1_700_000_000,
            last_launched_time: Some(1_700_000_500),
            move_existing_windows: true,
            other_windows: OtherWindows::Minimize,
            is_shortcut_needed: false,
            monitors: vec![Monitor {
                id: "1E6D-5B11-0001F3A2".into(),
                name: "LG".into(),
                number: 0,
                frame: Rect::new(0, 0, 2560, 1440),
                work_area: Rect::new(0, 25, 2560, 1415),
                scale: 1.0,
                primary: true,
            }],
            apps: vec![App {
                id: new_id(),
                name: "TextEdit".into(),
                bundle_id: "com.apple.TextEdit".into(),
                path: "/System/Applications/TextEdit.app".into(),
                title: "notes.txt".into(),
                document: Some("/Users/me/notes.txt".into()),
                url: None,
                command_line_args: String::new(),
                launch_as_admin: false,
                frame: Rect::new(100, 100, 800, 600),
                monitor_id: "1E6D-5B11-0001F3A2".into(),
                state: WindowState::Zoomed,
                space: Some(2),
                options: AppOptions { launch_new_instance: false, existing_windows: ExistingWindows::Leave, delay_ms: 500 },
            }],
        }
    }

    #[test]
    fn ids_are_unique_and_guid_shaped() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 38);
        assert!(a.starts_with('{') && a.ends_with('}'));
        assert_eq!(a.matches('-').count(), 4);
    }

    #[test]
    fn json_round_trip() {
        let d = tmp_dir("rt");
        assert!(load(&d).unwrap().is_empty());
        let ws = sample();
        save(&d, std::slice::from_ref(&ws)).unwrap();
        let text = std::fs::read_to_string(path_in(&d)).unwrap();
        assert!(text.contains("\"moveExistingWindows\": true"), "{text}");
        assert!(text.contains("\"existingWindows\": \"leave\""));
        assert!(text.contains("\"workArea\""));
        let back = load(&d).unwrap();
        assert_eq!(back, vec![ws.clone()]);
        // no temp file left behind
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![FILE_NAME.to_string()]);
        // upsert / remove
        let mut ws2 = ws.clone();
        ws2.name = "Renamed".into();
        upsert(&d, ws2).unwrap();
        assert_eq!(load(&d).unwrap()[0].name, "Renamed");
        assert!(remove(&d, &ws.id).unwrap());
        assert!(!remove(&d, &ws.id).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_fields_default() {
        let ws: Workspace = serde_json::from_str(r#"{"id":"x","name":"n","apps":[{"name":"A"}]}"#).unwrap();
        assert_eq!(ws.apps[0].state, WindowState::Normal);
        assert_eq!(ws.other_windows, OtherWindows::Leave);
        assert!(!ws.move_existing_windows);
    }

    #[test]
    fn lookup() {
        let mut a = sample();
        a.id = "{AAAA}".into();
        let mut b = sample();
        b.name = "Writing".into();
        let mut c = sample();
        c.name = "Work".into();
        let list = vec![a, b, c];
        assert_eq!(find(&list, "AAAA").unwrap().name, "Coding");
        assert_eq!(find(&list, "coding").unwrap().id, "{AAAA}");
        assert_eq!(find(&list, "wri").unwrap().name, "Writing");
        assert!(find(&list, "w").is_none(), "ambiguous prefix");
        assert!(find(&list, "nothing").is_none());
        assert_eq!(unique_name(&list, "Coding"), "Coding 2");
        assert_eq!(unique_name(&list, "New"), "New");
    }
}
