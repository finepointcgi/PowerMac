//! Apple Shortcuts: listing (for the settings UI) and running them with the
//! `shortcuts` command-line tool.

use std::path::Path;
use std::time::Duration;

pub const SHORTCUTS_BIN: &str = "/usr/bin/shortcuts";

/// Names from `shortcuts list` output (one per line), deduplicated and
/// sorted case-insensitively.
pub fn parse_list(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    v.sort_by_key(|a| a.to_lowercase());
    v.dedup();
    v
}

/// The user's shortcuts (`shortcuts list`; read-only). Empty when the tool
/// isn't available (Windows, macOS before 12).
pub fn list_shortcuts() -> Vec<String> {
    if !Path::new(SHORTCUTS_BIN).exists() {
        return vec![];
    }
    match super::run::run(&[SHORTCUTS_BIN.into(), "list".into()], None, &[], Duration::from_secs(15)) {
        Ok(o) if o.ok() => parse_list(&o.stdout),
        Ok(o) => {
            log::info!("notch-shelf: shortcuts list: {}", o.error_message());
            vec![]
        }
        Err(e) => {
            log::info!("notch-shelf: shortcuts list: {e}");
            vec![]
        }
    }
}

/// Arguments for running `name`, writing its output to `output`.
pub fn run_argv(name: &str, output: &Path) -> Vec<String> {
    vec![SHORTCUTS_BIN.into(), "run".into(), name.into(), "--output-path".into(), output.to_string_lossy().into_owned(), "--output-type".into(), "public.plain-text".into()]
}

/// Run a shortcut and return its text output (trimmed, may be empty).
pub fn run_shortcut(name: &str, scratch: &Path, timeout: Duration) -> Result<String, String> {
    let _ = std::fs::create_dir_all(scratch);
    let out = scratch.join(format!("shortcut-{}.txt", super::content::hash_bytes(name.as_bytes())));
    let _ = std::fs::remove_file(&out);
    let r = super::run::run(&run_argv(name, &out), None, &[], timeout)?;
    if !r.ok() {
        return Err(r.error_message());
    }
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parsing_and_argv() {
        assert_eq!(parse_list("Log Water\n\n  Focus  \nadd todo\nFocus\n"), vec!["add todo", "Focus", "Log Water"]);
        let a = run_argv("Log Water", Path::new("/tmp/o.txt"));
        assert_eq!(a[..3], ["/usr/bin/shortcuts", "run", "Log Water"]);
        assert!(a.contains(&"public.plain-text".to_string()));
    }

    /// Read-only: lists the user's shortcuts (never runs one).
    #[test]
    #[cfg(target_os = "macos")]
    fn listing_is_read_only_and_works() {
        let v = list_shortcuts();
        println!("{} shortcuts", v.len());
        assert!(v.iter().all(|s| !s.trim().is_empty()));
    }
}
