//! The app's top-level config file: which modules run, plus MWB and Awake
//! settings. FancyZones keeps its own file (`fancyzones.json`) next to it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Module<T> {
    pub enabled: bool,
    #[serde(flatten)]
    pub settings: T,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Toggle {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(rename = "mouseWithoutBorders")]
    pub mwb: Module<mwb::Config>,
    pub fancyzones: Toggle,
    pub awake: Module<awake::AwakeConfig>,
    /// Has its own `enabled` switch.
    pub scroll_reverser: scroll_reverser::Config,
    /// Find My Mouse, Mouse Highlighter, Crosshairs (each has its own `enabled`).
    pub mouse_utils: mouse_utils::Config,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            mwb: Module { enabled: true, settings: mwb::Config::default() },
            fancyzones: Toggle { enabled: true },
            awake: Module { enabled: false, settings: awake::AwakeConfig::default() },
            // Windows has no "natural scrolling" default to undo, so only reverse on macOS.
            scroll_reverser: scroll_reverser::Config { enabled: cfg!(target_os = "macos"), ..Default::default() },
            mouse_utils: mouse_utils::Config::default(),
        }
    }
}

impl AppConfig {
    /// `<config dir>/PowerMac/config.json`
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("PowerMac").join("config.json"))
    }

    /// Defaults with a new MWB key, this machine in the matrix, and a fixed machine id
    /// (MWB persists the id; a random one per start confuses peers).
    pub fn with_new_key() -> Self {
        let mut c = AppConfig::default();
        c.mwb.settings.key = mwb::create_random_key();
        c.mwb.settings.matrix = vec![c.mwb.settings.effective_machine_name()];
        c.mwb.settings.machine_id = Some(rand_id());
        c
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
    }

    /// Atomic write (temp file + rename) so the config watcher never reads half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Load, change and save the config file at `path`.
pub fn edit_at(path: &Path, f: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let mut c = AppConfig::load_or_default(path)?;
    f(&mut c);
    c.save(path)
}

fn rand_id() -> u32 {
    let b: [u8; 4] = mwb::crypto::random_bytes();
    // Avoid the reserved ids NONE (0) and ALL (255).
    u32::from_le_bytes(b).max(256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_files() {
        let c = AppConfig::with_new_key();
        let json = serde_json::to_string(&c).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mwb.settings.key, c.mwb.settings.key);
        assert!(mwb::crypto::is_key_valid(&back.mwb.settings.key));

        let partial: AppConfig = serde_json::from_str(
            r#"{"mouseWithoutBorders":{"enabled":true,"key":"abcdefghjkmnpqrs","matrix":["mac","pc"]},
                "awake":{"enabled":true,"mode":{"type":"indefinite"},"keepDisplayOn":true}}"#,
        )
        .unwrap();
        assert_eq!(partial.mwb.settings.matrix, ["mac", "pc"]);
        assert_eq!(partial.mwb.settings.port, mwb::DEFAULT_PORT);
        assert!(partial.awake.enabled && partial.awake.settings.keep_display_on);
        assert!(partial.fancyzones.enabled);
    }
}
