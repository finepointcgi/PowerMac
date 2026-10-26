//! The app's top-level config file: which modules run, plus MWB and Awake
//! settings. FancyZones keeps its own file (`fancyzones.json`) next to it.

pub mod login;

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
    /// Key and shortcut remaps, paste as plain text (has its own `enabled`).
    pub keyboard_manager: keyboard_manager::Config,
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
            keyboard_manager: keyboard_manager::Config::default(),
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

/// Written next to `config.json` by the settings window (or anything else) to
/// ask the running app for a Mouse Without Borders discovery sweep now.
pub const MWB_DISCOVER_REQUEST_FILE: &str = "mwb-discover.json";
/// Written next to `config.json` by the running app: Mouse Without Borders
/// connection status for the settings window.
pub const MWB_STATUS_FILE: &str = "mwb-status.json";
/// The app rewrites the status file at least this often while it runs.
pub const MWB_STATUS_REFRESH_SECS: u64 = 5;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A request for a discovery sweep (see [`MWB_DISCOVER_REQUEST_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverRequest {
    /// Milliseconds since the Unix epoch; old requests are ignored.
    pub at: u64,
}

impl DiscoverRequest {
    pub fn now() -> Self {
        DiscoverRequest { at: now_ms() }
    }

    pub fn is_fresh(&self, max_age_ms: u64) -> bool {
        now_ms().saturating_sub(self.at) <= max_age_ms
    }

    /// Write a request next to `config_path`.
    pub fn write_for(config_path: &Path) -> Result<()> {
        let path = config_path.with_file_name(MWB_DISCOVER_REQUEST_FILE);
        std::fs::write(&path, serde_json::to_string(&Self::now())?).with_context(|| format!("writing {}", path.display()))
    }
}

/// One machine in [`MwbStatus`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MwbMachine {
    pub name: String,
    /// IP address (or `ip:port` when it uses another port).
    pub address: String,
    pub connected: bool,
    /// Found automatically this session.
    pub discovered: bool,
}

/// Mouse Without Borders status as the running app last saw it (see [`MWB_STATUS_FILE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MwbStatus {
    /// When it was written (milliseconds since the Unix epoch).
    pub at: u64,
    /// The engine is running.
    pub running: bool,
    pub machine_name: String,
    pub searching: bool,
    pub auto_discover: bool,
    /// When the last search started (milliseconds since the Unix epoch).
    pub last_search: Option<u64>,
    pub machines: Vec<MwbMachine>,
    /// Addresses that answered with a different security key.
    pub other_key: Vec<String>,
}

impl MwbStatus {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name(MWB_STATUS_FILE)
    }

    pub fn stamp(mut self) -> Self {
        self.at = now_ms();
        self
    }

    /// Whether the app wrote it recently enough to be running.
    pub fn is_fresh(&self) -> bool {
        now_ms().saturating_sub(self.at) <= MWB_STATUS_REFRESH_SECS * 3 * 1000
    }

    /// The status next to `config_path`, if any.
    pub fn load_for(config_path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(Self::path_for(config_path)).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Atomic write next to `config_path`.
    pub fn save_for(&self, config_path: &Path) -> Result<()> {
        let path = Self::path_for(config_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
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
        // Discovery is on by default, also for configs written before it existed.
        assert!(partial.mwb.settings.auto_discover);
        assert_eq!(partial.mwb.settings.discovery_interval_secs, 60);
        assert_eq!(partial.mwb.settings.discovery_interval_connected_secs, 300);
    }

    #[test]
    fn status_and_request_files() {
        let dir = std::env::temp_dir().join(format!("pm-config-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        assert!(MwbStatus::load_for(&cfg).is_none());
        let st = MwbStatus {
            running: true,
            machines: vec![MwbMachine { name: "pc".into(), address: "192.168.1.20".into(), connected: true, discovered: true }],
            ..Default::default()
        }
        .stamp();
        st.save_for(&cfg).unwrap();
        let back = MwbStatus::load_for(&cfg).unwrap();
        assert_eq!(back, st);
        assert!(back.is_fresh());
        assert!(!MwbStatus { at: 1, ..Default::default() }.is_fresh());
        DiscoverRequest::write_for(&cfg).unwrap();
        let r: DiscoverRequest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MWB_DISCOVER_REQUEST_FILE)).unwrap()).unwrap();
        assert!(r.is_fresh(5_000));
        assert!(!DiscoverRequest { at: 1 }.is_fresh(5_000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
