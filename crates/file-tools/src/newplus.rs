//! New+: create files and folders from templates.
//!
//! Every file or folder in the templates folder is a template. Creating one
//! copies it into the target folder under a unique name ("Notes 2.md"),
//! expanding variables as PowerToys New+ does:
//!
//! * in names: `$YYYY $MM $DD $hh $mm $ss …` (all of PowerRename's date
//!   variables, current time), `$PARENT_FOLDER_NAME` (the folder the item is
//!   created in; for items inside a folder template, their parent), and
//!   `%VAR%` environment variables. Characters that can't be in a file name
//!   become spaces. Leading ordering digits ("01. ") are dropped when
//!   `hide_starting_digits` is on.
//! * in the contents of text files (UTF-8, no NUL bytes, ≤ 1 MB): the
//!   multi-letter date variables, `$PARENT_FOLDER_NAME` and `$FILE_NAME` (the
//!   new item's name without extension), each only as a whole word, so
//!   `$HOME` or `$s` in a script are left alone; `\$YYYY` stays literal.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::config::{Config, NewPlusConfig};
use crate::rename::{natural_cmp, split_ext};
use crate::vars;

/// A template in the templates folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub path: PathBuf,
    /// The file or folder name in the templates folder.
    pub file_name: String,
    pub is_dir: bool,
}

impl Template {
    pub fn from_path(path: &Path) -> Option<Self> {
        let md = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            file_name: path.file_name()?.to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
        })
    }

    /// The name shown in the picker.
    pub fn display_name(&self, hide_extension: bool, hide_starting_digits: bool) -> String {
        let mut n = if hide_starting_digits {
            remove_starting_digits(&self.file_name, self.is_dir)
        } else {
            self.file_name.clone()
        };
        if hide_extension && !self.is_dir {
            let (stem, _) = split_ext(&n);
            n = stem.to_string();
        }
        n
    }

    /// The extension, lowercase without the dot ("" for folders).
    pub fn extension(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        split_ext(&self.file_name)
            .1
            .trim_start_matches('.')
            .to_lowercase()
    }
}

/// `<config dir>/PowerMac/Templates`.
pub fn default_templates_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("PowerMac").join("Templates"))
}

/// The templates folder: the `newPlus.templatesDir` setting, or
/// `<config dir>/PowerMac/Templates`.
pub fn templates_dir() -> PathBuf {
    templates_dir_for(&Config::load().new_plus)
}

pub fn templates_dir_for(cfg: &NewPlusConfig) -> PathBuf {
    cfg.templates_dir
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(default_templates_dir)
        .unwrap_or_else(|| PathBuf::from("Templates"))
}

/// PowerToys `remove_starting_digits_from_filename`: "01. Notes.txt" →
/// "Notes.txt", "03 Third" → "Third"; names that are only digits stay.
pub fn remove_starting_digits(name: &str, is_dir: bool) -> String {
    let (stem, ext) = split_ext(name);
    if !stem.is_empty() && stem.chars().all(|c| c.is_ascii_digit()) && (!is_dir || ext.is_empty()) {
        return name.to_string();
    }
    let digits = name.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return name.to_string();
    }
    let b = name.as_bytes();
    let mut n = digits;
    if b.get(n) == Some(&b'.') {
        n += 1;
        if b.get(n) == Some(&b' ') {
            n += 1;
        }
    } else if b.get(n) == Some(&b' ') {
        n += 1;
        if b.get(n) == Some(&b'.') {
            n += 1;
        }
    }
    name[n..].to_string()
}

/// The templates in `dir`, sorted by name ("01. …" ordering works), hidden
/// files skipped.
pub fn list_templates(dir: &Path) -> anyhow::Result<Vec<Template>> {
    let mut v: Vec<Template> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| Template::from_path(&e.path()))
        .collect();
    v.sort_by(|a, b| natural_cmp(&a.file_name, &b.file_name));
    Ok(v)
}

const MARKDOWN: &str = "# $FILE_NAME\n\nCreated $YYYY-$MM-$DD in $PARENT_FOLDER_NAME.\n";
const RTF: &str = "{\\rtf1\\ansi\\ansicpg1252\\cocoartf2580\n{\\fonttbl\\f0\\fswiss\\fcharset0 Helvetica;}\n{\\colortbl;\\red255\\green255\\blue255;}\n\\paperw11900\\paperh16840\\margl1440\\margr1440\\vieww11520\\viewh8400\\viewkind0\n\\pard\\tx720\\tx1440\\tx2160\\tx2880\\pardirnatural\\partightenfactor0\n\n\\f0\\fs24 \\cf0 }\n";
const SHELL: &str =
    "#!/bin/sh\n# $FILE_NAME — created $YYYY-$MM-$DD\nset -eu\n\necho \"Hello from $0\"\n";
const HTML: &str = "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>$FILE_NAME</title>\n</head>\n<body>\n  <h1>$FILE_NAME</h1>\n</body>\n</html>\n";
const PROJECT_README: &str =
    "# $PARENT_FOLDER_NAME\n\nStarted $YYYY-$MM-$DD.\n\n- `src/` — code\n- `docs/` — notes and documents\n";
const PROJECT_NOTES: &str = "# Notes — $YYYY-$MM-$DD\n\n";

/// Fill `dir` with the default templates when it doesn't exist yet. Returns
/// whether it did.
pub fn ensure_templates(dir: &Path) -> anyhow::Result<bool> {
    if dir.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("Empty text file.txt"), "")?;
    std::fs::write(dir.join("Markdown.md"), MARKDOWN)?;
    std::fs::write(dir.join("Rich text.rtf"), RTF)?;
    let sh = dir.join("Shell script.sh");
    std::fs::write(&sh, SHELL)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(dir.join("HTML file.html"), HTML)?;
    let proj = dir.join("Project");
    std::fs::create_dir_all(proj.join("src"))?;
    std::fs::create_dir_all(proj.join("docs"))?;
    std::fs::write(proj.join("README.md"), PROJECT_README)?;
    std::fs::write(
        proj.join("docs").join("Notes $YYYY-$MM-$DD.md"),
        PROJECT_NOTES,
    )?;
    Ok(true)
}

/// Replace `/ \ : * ? " < > |` and control characters with spaces.
pub fn make_valid_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// `%NAME%` → the environment variable (left as is when it isn't set).
pub fn expand_env_vars(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let after = &rest[a + 1..];
        match after.find('%') {
            Some(b) if b > 0 => {
                let name = &after[..b];
                match std::env::var(name) {
                    Ok(v) if !v.is_empty() => {
                        out.push_str(&v);
                        rest = &after[b + 1..];
                    }
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        rest = &after[b..];
                    }
                }
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Expand the variables of a file or folder *name* (see the module docs).
pub fn expand_name(name: &str, parent_folder_name: &str, now: &DateTime<Local>) -> String {
    let s = vars::expand_date_vars(name, now);
    let s = expand_env_vars(&s);
    let s = s.replace("$PARENT_FOLDER_NAME", parent_folder_name);
    make_valid_filename(&s)
}

/// Expand the variables of text *contents* (see the module docs).
pub fn expand_text(
    text: &str,
    parent_folder_name: &str,
    file_name: &str,
    now: &DateTime<Local>,
) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut s = vars::expand_date_vars_in_text(text, now);
    for (var, val) in [
        ("$PARENT_FOLDER_NAME", parent_folder_name),
        ("$FILE_NAME", file_name),
    ] {
        let mut out = String::with_capacity(s.len());
        let mut rest = s.as_str();
        while let Some(i) = rest.find(var) {
            let escaped = rest[..i].ends_with('\\');
            let whole = !rest[i + var.len()..].starts_with(word);
            out.push_str(&rest[..i]);
            out.push_str(if whole && !escaped { val } else { var });
            rest = &rest[i + var.len()..];
        }
        out.push_str(rest);
        s = out;
    }
    s
}

/// Options for [`new_from_template_with`].
#[derive(Debug, Clone)]
pub struct NewOptions {
    /// The new item's name (before making it unique); default: the
    /// template's name with variables expanded.
    pub name: Option<String>,
    pub hide_starting_digits: bool,
    pub replace_variables: bool,
    pub now: DateTime<Local>,
}

impl Default for NewOptions {
    fn default() -> Self {
        Self {
            name: None,
            hide_starting_digits: true,
            replace_variables: true,
            now: Local::now(),
        }
    }
}

impl NewOptions {
    pub fn from_config(cfg: &NewPlusConfig) -> Self {
        Self {
            hide_starting_digits: cfg.hide_starting_digits,
            replace_variables: cfg.replace_variables,
            ..Default::default()
        }
    }
}

fn folder_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// The name a new item from `template` gets in `target_dir` before it is
/// made unique.
pub fn default_name(template: &Template, target_dir: &Path, opts: &NewOptions) -> String {
    let base = if opts.hide_starting_digits {
        remove_starting_digits(&template.file_name, template.is_dir)
    } else {
        template.file_name.clone()
    };
    if opts.replace_variables {
        expand_name(&base, &folder_name(target_dir), &opts.now)
    } else {
        base
    }
}

/// `name` in `dir`, or "stem 2.ext", "stem 3.ext", … if taken.
pub fn unique_path(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let first = dir.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = if is_dir { (name, "") } else { split_ext(name) };
    (2u32..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("a free name")
}

const MAX_TEXT: u64 = 1 << 20;

fn copy_file(src: &Path, dst: &Path, parent_name: &str, opts: &NewOptions) -> anyhow::Result<()> {
    use std::io::Write;
    let md = std::fs::metadata(src)?;
    let data = std::fs::read(src)?;
    let data = match std::str::from_utf8(&data) {
        Ok(text) if opts.replace_variables && md.len() <= MAX_TEXT && !text.contains('\0') => {
            let stem = split_ext(&folder_name(dst)).0.to_string();
            expand_text(text, parent_name, &stem, &opts.now).into_bytes()
        }
        _ => data,
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    f.write_all(&data)?;
    drop(f);
    std::fs::set_permissions(dst, md.permissions())?;
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path, opts: &NewOptions) -> anyhow::Result<()> {
    let parent_name = folder_name(dst);
    for e in std::fs::read_dir(src)?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n == ".DS_Store" {
            continue;
        }
        let name = if opts.replace_variables {
            expand_name(&n, &parent_name, &opts.now)
        } else {
            n
        };
        let ft = e.file_type()?;
        let is_dir = ft.is_dir();
        let target = unique_path(dst, &name, is_dir);
        if is_dir {
            std::fs::create_dir(&target)?;
            copy_dir_contents(&e.path(), &target, opts)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(e.path())?, &target)?;
            #[cfg(not(unix))]
            std::fs::copy(e.path(), &target).map(|_| ())?;
        } else {
            copy_file(&e.path(), &target, &parent_name, opts)?;
        }
    }
    Ok(())
}

/// Create a copy of `template` (a file or folder) in `target_dir` with the
/// default options, and return its path.
pub fn new_from_template(template: &Path, target_dir: &Path) -> anyhow::Result<PathBuf> {
    let cfg = Config::load().new_plus;
    new_from_template_with(template, target_dir, &NewOptions::from_config(&cfg))
}

/// [`new_from_template`] with explicit options.
pub fn new_from_template_with(
    template: &Path,
    target_dir: &Path,
    opts: &NewOptions,
) -> anyhow::Result<PathBuf> {
    let t = Template::from_path(template)
        .ok_or_else(|| anyhow::anyhow!("template not found: {}", template.display()))?;
    anyhow::ensure!(
        target_dir.is_dir(),
        "not a folder: {}",
        target_dir.display()
    );
    let name = match &opts.name {
        Some(n) => make_valid_filename(n.trim()),
        None => default_name(&t, target_dir, opts),
    };
    anyhow::ensure!(
        !name.trim().is_empty() && name != "." && name != "..",
        "the name is empty"
    );
    let parent_name = folder_name(target_dir);
    // Retry if another process takes the name between checking and creating.
    for _ in 0..100 {
        let dst = unique_path(target_dir, &name, t.is_dir);
        let r = if t.is_dir {
            std::fs::create_dir(&dst).map_err(anyhow::Error::from)
        } else {
            copy_file(&t.path, &dst, &parent_name, opts)
        };
        match r {
            Ok(()) => {
                if t.is_dir {
                    if let Err(e) = copy_dir_contents(&t.path, &dst, opts) {
                        return Err(e.context(format!("copying into {}", dst.display())));
                    }
                }
                return Ok(dst);
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue
            }
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("couldn't find a free name for {name}")
}

/// Show `path` selected in Finder / Explorer.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .status();
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .status();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .status();
    if let Err(e) = r {
        log::warn!("reveal {}: {e}", path.display());
    }
}

/// Open `path` with its default app (or the folder in Finder / Explorer).
pub fn open_path(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .status();
    #[cfg(windows)]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .status();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(path).status();
    if let Err(e) = r {
        log::warn!("open {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap()
    }

    fn opts() -> NewOptions {
        NewOptions {
            now: now(),
            ..Default::default()
        }
    }

    #[test]
    fn starting_digits() {
        let r = |n: &str, d: bool| remove_starting_digits(n, d);
        assert_eq!(r("01. First entry.txt", false), "First entry.txt");
        assert_eq!(r("02. Second entry", true), "Second entry");
        assert_eq!(r("03 Third entry", true), "Third entry");
        assert_eq!(r("04 Fourth entry.txt", false), "Fourth entry.txt");
        assert_eq!(r("05.Fifth entry.txt", false), "Fifth entry.txt");
        assert_eq!(r("001231", true), "001231");
        assert_eq!(r("001231.txt", false), "001231.txt");
        assert_eq!(r("13. 0123456789012345.txt", false), "0123456789012345.txt");
        assert_eq!(r("06 .Sixth", true), "Sixth");
        assert_eq!(r("07Seventh", true), "Seventh");
        assert_eq!(r("0123.Name", true), "Name");
    }

    #[test]
    fn name_variables() {
        std::env::set_var("PM_FT_TEST_VAR", "envval");
        let n = expand_name(
            "$YYYY-$MM-$DD $PARENT_FOLDER_NAME %PM_FT_TEST_VAR% %NOPE_X% 50% a:b",
            "Work",
            &now(),
        );
        assert_eq!(n, "2025-01-02 Work envval %NOPE_X% 50% a b");
    }

    #[test]
    fn text_variables() {
        let t = expand_text(
            "# $FILE_NAME in $PARENT_FOLDER_NAME on $YYYY-$MM-$DD\necho $HOME $FILE_NAMES \\$FILE_NAME $s",
            "Proj",
            "readme",
            &now(),
        );
        assert_eq!(
            t,
            "# readme in Proj on 2025-01-02\necho $HOME $FILE_NAMES \\$FILE_NAME $s"
        );
    }

    #[test]
    fn seeds_defaults_once() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("Templates");
        assert!(ensure_templates(&t).unwrap());
        assert!(!ensure_templates(&t).unwrap());
        let names: Vec<String> = list_templates(&t)
            .unwrap()
            .into_iter()
            .map(|t| t.file_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "Empty text file.txt",
                "HTML file.html",
                "Markdown.md",
                "Project",
                "Rich text.rtf",
                "Shell script.sh"
            ]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(t.join("Shell script.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o111, 0o111);
        }
        // Removing a template isn't undone by the next run.
        fs::remove_file(t.join("Markdown.md")).unwrap();
        ensure_templates(&t).unwrap();
        assert!(!t.join("Markdown.md").exists());
    }

    #[test]
    fn create_file_unique_names_and_contents() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("T");
        ensure_templates(&t).unwrap();
        let target = d.path().join("My Docs");
        fs::create_dir(&target).unwrap();
        let md = t.join("Markdown.md");
        let a = new_from_template_with(&md, &target, &opts()).unwrap();
        let b = new_from_template_with(&md, &target, &opts()).unwrap();
        let c = new_from_template_with(&md, &target, &opts()).unwrap();
        assert_eq!(a, target.join("Markdown.md"));
        assert_eq!(b, target.join("Markdown 2.md"));
        assert_eq!(c, target.join("Markdown 3.md"));
        assert_eq!(
            fs::read_to_string(&b).unwrap(),
            "# Markdown 2\n\nCreated 2025-01-02 in My Docs.\n"
        );
        // Template untouched.
        assert_eq!(fs::read_to_string(&md).unwrap(), MARKDOWN);
        // Executable bit kept.
        let sh = new_from_template_with(&t.join("Shell script.sh"), &target, &opts()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&sh).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
        assert!(fs::read_to_string(&sh)
            .unwrap()
            .contains("echo \"Hello from $0\""));
        // Custom name; replace_variables off keeps contents verbatim.
        let o = NewOptions {
            name: Some("notes.md".into()),
            replace_variables: false,
            ..opts()
        };
        let n = new_from_template_with(&md, &target, &o).unwrap();
        assert_eq!(n, target.join("notes.md"));
        assert_eq!(fs::read_to_string(&n).unwrap(), MARKDOWN);
        // Binary files are copied as is.
        let bin = t.join("blob.bin");
        fs::write(&bin, [0u8, 1, 2, b'$', b'Y']).unwrap();
        let nb = new_from_template_with(&bin, &target, &opts()).unwrap();
        assert_eq!(fs::read(nb).unwrap(), vec![0u8, 1, 2, b'$', b'Y']);
        assert!(new_from_template_with(&md, &target.join("missing"), &opts()).is_err());
    }

    #[test]
    fn create_folder_template() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("T");
        ensure_templates(&t).unwrap();
        fs::write(t.join("Project").join(".DS_Store"), "x").unwrap();
        let target = d.path().join("work");
        fs::create_dir(&target).unwrap();
        let p = new_from_template_with(&t.join("Project"), &target, &opts()).unwrap();
        assert_eq!(p, target.join("Project"));
        assert!(p.join("src").is_dir());
        assert!(!p.join(".DS_Store").exists());
        assert_eq!(
            fs::read_to_string(p.join("README.md")).unwrap(),
            PROJECT_README
                .replace("$PARENT_FOLDER_NAME", "Project")
                .replace("$YYYY-$MM-$DD", "2025-01-02")
        );
        assert!(p.join("docs/Notes 2025-01-02.md").is_file());
        let p2 = new_from_template_with(&t.join("Project"), &target, &opts()).unwrap();
        assert_eq!(p2, target.join("Project 2"));
        assert!(fs::read_to_string(p2.join("README.md"))
            .unwrap()
            .starts_with("# Project 2\n"));
    }

    #[test]
    fn template_names_with_variables_and_digits() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("T");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("02. Report $YYYY-$MM.txt"), "r").unwrap();
        fs::write(t.join("01. Log of $PARENT_FOLDER_NAME.txt"), "l").unwrap();
        let list = list_templates(&t).unwrap();
        assert_eq!(
            list[0].display_name(true, true),
            "Log of $PARENT_FOLDER_NAME"
        );
        assert_eq!(
            list[1].display_name(false, false),
            "02. Report $YYYY-$MM.txt"
        );
        assert_eq!(list[1].extension(), "txt");
        let target = d.path().join("Box");
        fs::create_dir(&target).unwrap();
        let p = new_from_template_with(&list[1].path, &target, &opts()).unwrap();
        assert_eq!(p, target.join("Report 2025-01.txt"));
        let p = new_from_template_with(&list[0].path, &target, &opts()).unwrap();
        assert_eq!(p, target.join("Log of Box.txt"));
        let o = NewOptions {
            hide_starting_digits: false,
            replace_variables: false,
            ..opts()
        };
        let p = new_from_template_with(&list[0].path, &target, &o).unwrap();
        assert_eq!(p, target.join("01. Log of $PARENT_FOLDER_NAME.txt"));
    }

    #[test]
    fn templates_dir_override() {
        let mut c = NewPlusConfig::default();
        assert!(templates_dir_for(&c).ends_with("PowerMac/Templates"));
        c.templates_dir = Some("/tmp/x".into());
        assert_eq!(templates_dir_for(&c), PathBuf::from("/tmp/x"));
    }
}
