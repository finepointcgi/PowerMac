//! PowerRename's engine: the items to rename, the options, the preview
//! ([`plan`]) and applying it ([`apply`], [`undo_last`]).
//!
//! The preview is computed once per change of options; the very same new
//! names (including counters and random values) are what [`apply`] renames
//! to, so the preview is an exact dry run.

pub mod apply;
pub mod diff;
pub mod replace;
pub mod transform;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use rand::Rng;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub use apply::{apply, undo_last, ApplyReport, Journal, JournalOp};

/// Which part of a file name the search and formatting apply to (folders
/// always use the whole name; "extension only" skips folders).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ApplyTo {
    #[default]
    NameAndExtension,
    NameOnly,
    ExtensionOnly,
}

/// Text formatting applied after the replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TextCase {
    #[default]
    None,
    Upper,
    Lower,
    Title,
    Capitalize,
}

/// The file time used by the date/time variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TimeSource {
    #[default]
    Created,
    Modified,
    Accessed,
}

/// Everything that decides the new names (PowerToys `PowerRenameFlags` plus
/// the search and replace terms).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RenameOptions {
    pub search: String,
    pub replace: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub match_all: bool,
    pub apply_to: ApplyTo,
    pub exclude_files: bool,
    pub exclude_folders: bool,
    /// Leave the contents of selected folders alone (PowerToys "Exclude
    /// subfolder items"); off = subfolder items are included.
    pub exclude_subfolder_items: bool,
    pub text_case: TextCase,
    /// `${…}` in the replacement is a counter.
    pub enumerate: bool,
    /// `${rstringalnum=N}` etc. in the replacement are random values.
    pub randomize: bool,
    pub time_source: TimeSource,
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self {
            search: String::new(),
            replace: String::new(),
            use_regex: false,
            case_sensitive: false,
            match_all: true,
            apply_to: ApplyTo::NameAndExtension,
            exclude_files: false,
            exclude_folders: false,
            exclude_subfolder_items: false,
            text_case: TextCase::None,
            enumerate: true,
            randomize: true,
            time_source: TimeSource::Created,
        }
    }
}

/// One file or folder in the list.
#[derive(Debug, Clone, PartialEq)]
pub struct RenameItem {
    pub path: PathBuf,
    /// The current leaf name.
    pub name: String,
    pub is_dir: bool,
    /// 0 for selected items, 1 for their children, …
    pub depth: usize,
    /// Whether the user ticked it (unticked items are left alone).
    pub selected: bool,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

impl RenameItem {
    /// An item for `path` (metadata read from disk, `lstat`: symlinks are
    /// renamed themselves, not followed).
    pub fn from_path(path: &Path, depth: usize) -> Option<Self> {
        let md = std::fs::symlink_metadata(path).ok()?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some(Self {
            path: path.to_path_buf(),
            name,
            is_dir: md.is_dir(),
            depth,
            selected: true,
            created: md.created().ok(),
            modified: md.modified().ok(),
            accessed: md.accessed().ok(),
        })
    }

    fn time(&self, src: TimeSource) -> Option<SystemTime> {
        match src {
            TimeSource::Created => self.created,
            TimeSource::Modified => self.modified,
            TimeSource::Accessed => self.accessed,
        }
        .or(self.modified)
        .or(self.created)
    }
}

/// At most this many items are listed (subfolder contents are cut off).
pub const MAX_ITEMS: usize = 50_000;

/// "file2" < "file10", case-insensitive.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(c) = y.peek().copied().filter(char::is_ascii_digit) {
                    n2.push(c);
                    y.next();
                }
                let t1 = n1.trim_start_matches('0');
                let t2 = n2.trim_start_matches('0');
                let o = t1.len().cmp(&t2.len()).then_with(|| t1.cmp(t2));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The list for `paths`: each path (sorted by name), and after each folder
/// its contents, recursively (sorted, hidden dot-files skipped, symlinked
/// folders not followed). Duplicates and paths inside other selected folders
/// appear once.
pub fn collect_items(paths: &[PathBuf]) -> Vec<RenameItem> {
    let mut top: Vec<PathBuf> = paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect();
    top.sort_by(|a, b| {
        natural_cmp(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    top.dedup();
    let selected_dirs: Vec<PathBuf> = top.iter().filter(|p| p.is_dir()).cloned().collect();
    let mut out = Vec::new();
    for p in &top {
        // Listed as part of a selected parent already.
        if selected_dirs.iter().any(|d| d != p && p.starts_with(d)) {
            continue;
        }
        if let Some(item) = RenameItem::from_path(p, 0) {
            let dir = item.is_dir;
            out.push(item);
            if dir {
                walk(p, 1, &mut out);
            }
        }
    }
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<RenameItem>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    children.sort_by(|a, b| {
        natural_cmp(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    for c in children {
        if out.len() >= MAX_ITEMS {
            return;
        }
        if let Some(item) = RenameItem::from_path(&c, depth) {
            let d = item.is_dir;
            out.push(item);
            if d {
                walk(&c, depth + 1, out);
            }
        }
    }
}

/// Unicode NFC (macOS file names are often decomposed).
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// `name` split at the last dot ("a.tar.gz" → "a.tar", ".gz"; ".bashrc" and
/// "README" have no extension).
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Whether this platform's file systems are usually case-insensitive
/// (APFS/HFS+ and NTFS defaults).
pub const CASE_INSENSITIVE_FS: bool = cfg!(any(target_os = "macos", windows));

/// The key two names collide on: NFC, and lowercase where the file system
/// ignores case.
pub fn name_key(name: &str) -> String {
    let n = nfc(name);
    if CASE_INSENSITIVE_FS {
        n.to_lowercase()
    } else {
        n
    }
}

/// The result for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not selected, or excluded by the options.
    Skipped,
    /// The new name is the same as the old one.
    Unchanged,
    /// Will be renamed.
    Rename,
    /// The new name has characters the file system doesn't allow.
    InvalidChars,
    /// The new name is empty, "." or "..".
    InvalidName,
    /// The new name is longer than 255.
    TooLong,
    /// Another item in the batch gets the same name.
    Duplicate,
    /// A file that stays in that folder already has the name.
    Exists,
}

impl Status {
    pub fn is_error(self) -> bool {
        !matches!(self, Status::Skipped | Status::Unchanged | Status::Rename)
    }

    pub fn message(self) -> &'static str {
        match self {
            Status::Skipped => "Excluded",
            Status::Unchanged => "Unchanged",
            Status::Rename => "Will be renamed",
            Status::InvalidChars if cfg!(windows) => "Names can't contain \\ / : * ? \" < > |",
            Status::InvalidChars => "Names can't contain / or control characters",
            Status::InvalidName => "Name can't be empty",
            Status::TooLong => "Name is longer than 255 characters",
            Status::Duplicate => "Another item gets the same name",
            Status::Exists => "An item with this name already exists",
        }
    }
}

/// The preview of one item.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    /// The new leaf name (`Some` for `Rename` and for the error statuses).
    pub new_name: Option<String>,
    pub status: Status,
}

/// The preview of a batch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// One entry per item, same order.
    pub items: Vec<Planned>,
    /// The regex doesn't compile (nothing is renamed).
    pub error: Option<String>,
    /// Items with status `Rename`.
    pub rename_count: usize,
    /// Items with an error status.
    pub error_count: usize,
}

impl Plan {
    /// (from, to) for every item that will be renamed.
    pub fn renames<'a>(
        &'a self,
        items: &'a [RenameItem],
    ) -> impl Iterator<Item = (usize, PathBuf, PathBuf)> + 'a {
        self.items
            .iter()
            .zip(items)
            .enumerate()
            .filter(|(_, (p, _))| p.status == Status::Rename)
            .map(|(i, (p, it))| {
                let to = it
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(p.new_name.as_deref().unwrap_or_default());
                (i, it.path.clone(), to)
            })
    }
}

fn invalid_chars(name: &str) -> bool {
    name.chars().any(|c| {
        c == '/'
            || c == '\0'
            || c.is_control()
            || (cfg!(windows) && matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    })
}

fn too_long(name: &str) -> bool {
    if cfg!(windows) {
        name.encode_utf16().count() > 255
    } else {
        name.len() > 255
    }
}

fn local(t: SystemTime) -> DateTime<Local> {
    DateTime::<Local>::from(t)
}

/// The new name of one item, before validation (`None`: unchanged or excluded).
fn new_name(
    item: &RenameItem,
    opts: &RenameOptions,
    matcher: Option<&replace::Matcher>,
    parts: &[replace::Part],
    date_vars: bool,
    index: &mut u64,
    rng: &mut impl Rng,
) -> Result<Option<String>, String> {
    let original = nfc(&item.name);
    let (stem, ext) = split_ext(&original);
    let source: &str = match (item.is_dir, opts.apply_to) {
        (true, _) | (false, ApplyTo::NameAndExtension) => &original,
        (false, ApplyTo::NameOnly) => stem,
        (false, ApplyTo::ExtensionOnly) => ext.strip_prefix('.').unwrap_or(""),
    };
    let mut replaced = None;
    if let Some(m) = matcher {
        if !source.is_empty() {
            let time = if date_vars {
                item.time(opts.time_source).map(local)
            } else {
                None
            };
            let segs = replace::resolve(parts, *index, time.as_ref(), rng);
            let (r, matched) = m.replace(source, &segs)?;
            if matched {
                *index += 1;
            }
            replaced = Some(r);
        }
    }
    if replaced.is_none() && opts.text_case != TextCase::None {
        replaced = Some(source.to_string());
    }
    let Some(new) = replaced else {
        return Ok(None);
    };
    let full = match (item.is_dir, opts.apply_to) {
        (true, _) | (false, ApplyTo::NameAndExtension) => new,
        (false, ApplyTo::NameOnly) => format!("{new}{ext}"),
        (false, ApplyTo::ExtensionOnly) if ext.is_empty() => original.clone(),
        (false, ApplyTo::ExtensionOnly) => format!("{stem}.{new}"),
    };
    let mut full = transform::trim_name(&full);
    if opts.text_case != TextCase::None {
        full = transform::transform(&full, opts.text_case, opts.apply_to, item.is_dir);
    }
    if full == original {
        return Ok(None);
    }
    Ok(Some(full))
}

/// Compute the new names of `items` with `opts`.
pub fn plan(items: &[RenameItem], opts: &RenameOptions) -> Plan {
    plan_with_rng(items, opts, &mut rand::thread_rng())
}

/// [`plan`] with a given random generator (for tests).
pub fn plan_with_rng(items: &[RenameItem], opts: &RenameOptions, rng: &mut impl Rng) -> Plan {
    let mut out = Plan::default();
    let matcher = match replace::Matcher::new(
        &opts.search,
        opts.use_regex,
        opts.case_sensitive,
        opts.match_all,
    ) {
        Ok(m) => m,
        Err(e) => {
            out.error = Some(e);
            None
        }
    };
    let replace_nfc = nfc(&opts.replace);
    let parts = replace::parse_template(&replace_nfc, opts.enumerate, opts.randomize);
    let date_vars = parts.iter().any(|p| match p {
        replace::Part::Text(t) => crate::vars::uses_date_vars(t),
        _ => false,
    });
    let mut index = 0u64;
    for item in items {
        let excluded = !item.selected
            || (item.is_dir && opts.exclude_folders)
            || (!item.is_dir && opts.exclude_files)
            || (item.depth > 0 && opts.exclude_subfolder_items)
            || (item.is_dir && opts.apply_to == ApplyTo::ExtensionOnly);
        let planned = if excluded || out.error.is_some() {
            Planned {
                new_name: None,
                status: Status::Skipped,
            }
        } else {
            match new_name(
                item,
                opts,
                matcher.as_ref(),
                &parts,
                date_vars,
                &mut index,
                rng,
            ) {
                Ok(Some(n)) => {
                    let status = if n.is_empty() || n == "." || n == ".." {
                        Status::InvalidName
                    } else if invalid_chars(&n) {
                        Status::InvalidChars
                    } else if too_long(&n) {
                        Status::TooLong
                    } else {
                        Status::Rename
                    };
                    Planned {
                        new_name: Some(n),
                        status,
                    }
                }
                Ok(None) => Planned {
                    new_name: None,
                    status: Status::Unchanged,
                },
                Err(e) => {
                    out.error = Some(e);
                    Planned {
                        new_name: None,
                        status: Status::Skipped,
                    }
                }
            }
        };
        out.items.push(planned);
    }
    if out.error.is_some() {
        out.items.fill(Planned {
            new_name: None,
            status: Status::Skipped,
        });
    }
    check_collisions(items, &mut out);
    out.rename_count = out
        .items
        .iter()
        .filter(|p| p.status == Status::Rename)
        .count();
    out.error_count = out.items.iter().filter(|p| p.status.is_error()).count();
    out
}

/// Mark new names that collide with another new name in the same folder or
/// with an entry of that folder that isn't renamed away.
fn check_collisions(items: &[RenameItem], plan: &mut Plan) {
    // Folder → (keys renamed away, items renamed into it).
    let mut by_dir: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, p) in plan.items.iter().enumerate() {
        if p.status == Status::Rename {
            let dir = items[i]
                .path
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf();
            by_dir.entry(dir).or_default().push(i);
        }
    }
    for (dir, idx) in by_dir {
        let leaving: std::collections::HashSet<String> =
            idx.iter().map(|&i| name_key(&items[i].name)).collect();
        let existing: std::collections::HashSet<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| name_key(&e.file_name().to_string_lossy()))
                    .collect()
            })
            .unwrap_or_default();
        let mut targets: HashMap<String, Vec<usize>> = HashMap::new();
        for &i in &idx {
            let n = plan.items[i].new_name.as_deref().unwrap_or_default();
            targets.entry(name_key(n)).or_default().push(i);
        }
        for (key, v) in targets {
            if v.len() > 1 {
                for i in v {
                    plan.items[i].status = Status::Duplicate;
                }
            } else if existing.contains(&key) && !leaving.contains(&key) {
                plan.items[v[0]].status = Status::Exists;
            }
        }
    }
    // An item whose target is taken by an item that now fails stays put, so
    // its name is still taken: repeat until stable.
    loop {
        let mut changed = false;
        let mut stuck: HashMap<PathBuf, std::collections::HashSet<String>> = HashMap::new();
        for (i, p) in plan.items.iter().enumerate() {
            if p.status.is_error() {
                let dir = items[i]
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_path_buf();
                stuck
                    .entry(dir)
                    .or_default()
                    .insert(name_key(&items[i].name));
            }
        }
        for (i, p) in plan.items.iter_mut().enumerate() {
            if p.status != Status::Rename {
                continue;
            }
            let dir = items[i].path.parent().unwrap_or(Path::new(""));
            let key = name_key(p.new_name.as_deref().unwrap_or_default());
            if key != name_key(&items[i].name) && stuck.get(dir).is_some_and(|s| s.contains(&key)) {
                p.status = Status::Exists;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
