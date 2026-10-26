//! Settings of PowerRename and New+, stored as camelCase JSON in
//! `<config dir>/PowerMac/file-tools.json` (next to `config.json`). Every
//! field is optional; missing fields take their defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rename::RenameOptions;

/// Top-level file-tools settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub rename: RenameConfig,
    pub new_plus: NewPlusConfig,
}

/// PowerRename settings and remembered state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RenameConfig {
    /// Restore the last used options (including search / replace text) when
    /// the window opens (PowerToys "Restore search and replace text").
    pub remember_options: bool,
    /// The last used options (the defaults when `remember_options` is off).
    pub options: RenameOptions,
    /// Recently used search terms, newest first.
    pub search_mru: Vec<String>,
    /// Recently used replace terms, newest first.
    pub replace_mru: Vec<String>,
    /// How many entries the MRU lists keep (0 disables them).
    pub mru_size: usize,
    /// The file list shows only the items that will be renamed.
    pub show_only_changed: bool,
}

impl Default for RenameConfig {
    fn default() -> Self {
        Self {
            remember_options: true,
            options: RenameOptions::default(),
            search_mru: Vec::new(),
            replace_mru: Vec::new(),
            mru_size: 10,
            show_only_changed: false,
        }
    }
}

/// New+ settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewPlusConfig {
    /// Use this folder for templates instead of `<config dir>/PowerMac/Templates`.
    pub templates_dir: Option<PathBuf>,
    /// Show template names without their file extension in the picker.
    pub hide_extensions: bool,
    /// Hide leading ordering digits ("01. Notes.txt" shows as "Notes.txt")
    /// in the picker and in the created item's name.
    pub hide_starting_digits: bool,
    /// Expand variables ($YYYY, $PARENT_FOLDER_NAME, %ENV% …) in the names
    /// (and text contents) of created items.
    pub replace_variables: bool,
    /// Reveal (select) the new item in Finder / Explorer after creating it.
    pub reveal_after_create: bool,
    /// Open the new item with its default app after creating it.
    pub open_after_create: bool,
}

impl Default for NewPlusConfig {
    fn default() -> Self {
        Self {
            templates_dir: None,
            hide_extensions: true,
            hide_starting_digits: true,
            replace_variables: true,
            reveal_after_create: true,
            open_after_create: false,
        }
    }
}

impl Config {
    /// `<config dir>/PowerMac/file-tools.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("PowerMac").join("file-tools.json"))
    }

    /// Load from `path`; a missing or unreadable file gives the defaults.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("{}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Load from [`Config::default_path`].
    pub fn load() -> Self {
        Self::default_path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Write to `path` (atomically, via a temp file next to it).
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Remember `term` at the front of an MRU list of at most `size` entries.
    pub fn push_mru(list: &mut Vec<String>, term: &str, size: usize) {
        if term.is_empty() {
            return;
        }
        list.retain(|t| t != term);
        list.insert(0, term.to_string());
        list.truncate(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_and_defaults() {
        let c: Config = serde_json::from_str(
            r#"{"rename":{"searchMru":["a"],"options":{"useRegex":true}},
                "newPlus":{"hideExtensions":true,"templatesDir":"/x"}}"#,
        )
        .unwrap();
        assert_eq!(c.rename.search_mru, vec!["a"]);
        assert!(c.rename.options.use_regex);
        assert!(c.rename.remember_options);
        assert_eq!(c.rename.mru_size, 10);
        assert!(c.new_plus.hide_extensions);
        assert!(c.new_plus.hide_starting_digits);
        assert_eq!(c.new_plus.templates_dir.as_deref(), Some(Path::new("/x")));
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"newPlus\"") && s.contains("\"showOnlyChanged\""));
    }

    #[test]
    fn mru() {
        let mut l = vec![];
        for t in ["a", "b", "a", "", "c"] {
            Config::push_mru(&mut l, t, 2);
        }
        assert_eq!(l, vec!["c", "a"]);
    }

    #[test]
    fn save_load_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/file-tools.json");
        let mut c = Config::default();
        c.rename.replace_mru.push("x".into());
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p), c);
        assert_eq!(
            Config::load_from(&d.path().join("missing.json")),
            Config::default()
        );
    }
}
