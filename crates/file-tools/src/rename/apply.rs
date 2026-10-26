//! Renaming on disk.
//!
//! * Children before parents: renames are grouped by path depth and the
//!   deepest group goes first, so every path is still valid when it's used.
//! * Swaps and case-only renames (`a→b, b→a`, `photo.jpg → Photo.jpg` on
//!   case-insensitive APFS/NTFS) go through a temporary name in the same
//!   folder: first everything that needs it moves to a temp name, then to
//!   its final name.
//! * Nothing is overwritten: `renamex_np(RENAME_EXCL)` on macOS,
//!   `MoveFileExW` without `REPLACE_EXISTING` on Windows.
//! * Each batch is recorded in a journal (JSON) that [`undo_last`] reverts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{name_key, Plan, RenameItem};

/// One rename that was done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalOp {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// The renames of the last batch, in the order they were done: one group
/// per path depth, deepest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Journal {
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub levels: Vec<Vec<JournalOp>>,
}

impl Journal {
    pub fn len(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `<config dir>/PowerMac/rename-undo.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("PowerMac").join("rename-undo.json"))
    }

    pub fn load(path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<Self>(&s)
            .ok()
            .filter(|j| !j.is_empty())
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// What a batch did.
#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    /// Items renamed.
    pub renamed: usize,
    /// (path, error) for items that could not be renamed.
    pub errors: Vec<(PathBuf, String)>,
    /// The renames done (also written to the journal file, when given).
    pub journal: Journal,
}

/// Rename `from` to `to`, failing if `to` exists.
pub fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let f = CString::new(from.as_os_str().as_bytes())?;
        let t = CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: two valid NUL-terminated paths.
        let r = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        // File systems without RENAME_EXCL (some network/FAT volumes).
        if e.raw_os_error() != Some(libc::ENOTSUP) && e.raw_os_error() != Some(libc::EINVAL) {
            return Err(e);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let w = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
        let (f, t) = (w(from), w(to));
        // SAFETY: two valid NUL-terminated wide paths.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0)
        };
        return if ok != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
    }
    #[allow(unreachable_code)]
    {
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "an item with this name already exists",
            ));
        }
        std::fs::rename(from, to)
    }
}

fn temp_name(dir: &Path, n: usize) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!(
        ".powermac-rename-{}-{nanos:x}-{n}.tmp",
        std::process::id()
    ))
}

fn depth(p: &Path) -> usize {
    p.components().count()
}

/// Do the renames `ops` (from, to), grouped and ordered as described in the
/// module docs. `progress(done)` is called after each item.
pub fn execute(
    ops: Vec<(PathBuf, PathBuf)>,
    progress: &mut dyn FnMut(usize),
) -> (Journal, Vec<(PathBuf, String)>) {
    let mut levels: Vec<Vec<(PathBuf, PathBuf)>> = Vec::new();
    {
        let mut sorted = ops;
        sorted.sort_by_key(|(f, _)| std::cmp::Reverse(depth(f)));
        for op in sorted {
            match levels.last_mut() {
                Some(l) if depth(&l[0].0) == depth(&op.0) => l.push(op),
                _ => levels.push(vec![op]),
            }
        }
    }
    execute_levels(levels, progress)
}

fn execute_levels(
    levels: Vec<Vec<(PathBuf, PathBuf)>>,
    progress: &mut dyn FnMut(usize),
) -> (Journal, Vec<(PathBuf, String)>) {
    let mut journal = Journal {
        time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        levels: Vec::new(),
    };
    let mut errors = Vec::new();
    let mut done = 0usize;
    let mut tmp_n = 0usize;
    for level in levels {
        let mut done_level = Vec::new();
        // Names this level moves away from, per folder: a target among them
        // needs a temp name first.
        let sources: std::collections::HashSet<(PathBuf, String)> =
            level.iter().map(|(f, _)| (parent(f), key(f))).collect();
        let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (orig, temp, final)
        for (from, to) in &level {
            if sources.contains(&(parent(to), key(to))) {
                tmp_n += 1;
                let tmp = temp_name(&parent(from), tmp_n);
                match rename_noreplace(from, &tmp) {
                    Ok(()) => staged.push((from.clone(), tmp, to.clone())),
                    Err(e) => {
                        errors.push((from.clone(), e.to_string()));
                        done += 1;
                        progress(done);
                    }
                }
            } else {
                match rename_noreplace(from, to) {
                    Ok(()) => done_level.push(JournalOp {
                        from: from.clone(),
                        to: to.clone(),
                    }),
                    Err(e) => errors.push((from.clone(), e.to_string())),
                }
                done += 1;
                progress(done);
            }
        }
        for (orig, tmp, fin) in staged {
            match rename_noreplace(&tmp, &fin) {
                Ok(()) => done_level.push(JournalOp {
                    from: orig,
                    to: fin,
                }),
                Err(e) => {
                    let mut msg = e.to_string();
                    if let Err(e2) = rename_noreplace(&tmp, &orig) {
                        msg = format!("{msg}; left as {} ({e2})", tmp.display());
                    }
                    errors.push((orig, msg));
                }
            }
            done += 1;
            progress(done);
        }
        if !done_level.is_empty() {
            journal.levels.push(done_level);
        }
    }
    (journal, errors)
}

fn parent(p: &Path) -> PathBuf {
    p.parent().unwrap_or(Path::new("")).to_path_buf()
}

fn key(p: &Path) -> String {
    name_key(&p.file_name().unwrap_or_default().to_string_lossy())
}

/// Rename the items of `plan` that have status `Rename`. When `journal_path`
/// is given, the batch is written there for [`undo_last`] (replacing the
/// previous one; nothing is written when nothing was renamed).
pub fn apply(
    items: &[RenameItem],
    plan: &Plan,
    journal_path: Option<&Path>,
    progress: &mut dyn FnMut(usize),
) -> ApplyReport {
    let ops: Vec<(PathBuf, PathBuf)> = plan.renames(items).map(|(_, f, t)| (f, t)).collect();
    let (journal, errors) = execute(ops, progress);
    if let Some(p) = journal_path {
        if !journal.is_empty() {
            if let Err(e) = journal.save(p) {
                log::warn!("rename journal {}: {e}", p.display());
            }
        }
    }
    ApplyReport {
        renamed: journal.len(),
        errors,
        journal,
    }
}

/// Revert the batch recorded in `journal_path` (parents first, then their
/// children; swaps through temp names). The journal is removed when
/// everything was reverted, and keeps what failed otherwise.
pub fn undo_last(journal_path: &Path) -> anyhow::Result<ApplyReport> {
    let journal =
        Journal::load(journal_path).ok_or_else(|| anyhow::anyhow!("there is nothing to undo"))?;
    let levels: Vec<Vec<(PathBuf, PathBuf)>> = journal
        .levels
        .iter()
        .rev()
        .map(|l| l.iter().map(|o| (o.to.clone(), o.from.clone())).collect())
        .collect();
    let (done, errors) = execute_levels(levels, &mut |_| {});
    if errors.is_empty() {
        let _ = std::fs::remove_file(journal_path);
    } else {
        // Keep the ops that weren't reverted.
        let reverted: std::collections::HashSet<(PathBuf, PathBuf)> = done
            .levels
            .iter()
            .flatten()
            .map(|o| (o.to.clone(), o.from.clone()))
            .collect();
        let rest = Journal {
            time: journal.time,
            levels: journal
                .levels
                .iter()
                .map(|l| {
                    l.iter()
                        .filter(|o| !reverted.contains(&(o.from.clone(), o.to.clone())))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .filter(|l| !l.is_empty())
                .collect(),
        };
        rest.save(journal_path)?;
    }
    Ok(ApplyReport {
        renamed: done.len(),
        errors,
        journal: done,
    })
}
