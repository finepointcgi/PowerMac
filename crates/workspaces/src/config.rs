//! Module settings (stored by the app in its config.json).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::capture::CaptureOptions;
use crate::launch::LaunchOptions;
use crate::shortcut::ShortcutKind;

/// A global hotkey in PowerToys' JSON shape
/// (`{"win":false,"ctrl":true,"alt":true,"shift":false,"code":87}`).
/// `win` is Command on macOS, `alt` is Option; `code` is a Windows
/// virtual-key code. `code == 0` = no hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkey {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub code: u32,
}

impl Hotkey {
    pub const fn new(win: bool, ctrl: bool, alt: bool, shift: bool, code: u32) -> Self {
        Self { win, ctrl, alt, shift, code }
    }

    pub fn is_set(&self) -> bool {
        self.code != 0
    }

    /// What PowerToys uses (Win+Ctrl+` opens its editor), for a settings
    /// "use the PowerToys shortcut" button; not set by default here.
    pub const fn powertoys() -> Self {
        Self::new(true, true, false, false, 0xC0)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mac = cfg!(target_os = "macos");
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push(if mac { "Option" } else { "Alt" }.into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push(if mac { "Cmd" } else { "Win" }.into());
        }
        parts.push(match self.code {
            0 => "(none)".to_string(),
            c @ (0x30..=0x39 | 0x41..=0x5A) => char::from_u32(c).unwrap().to_string(),
            c @ 0x70..=0x87 => format!("F{}", c - 0x6F),
            0xC0 => "`".into(),
            c => format!("VK 0x{c:02X}"),
        });
        write!(f, "{}", parts.join("+"))
    }
}

/// `workspaces` section of the app config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// Opens the editor. None by default.
    pub hotkey: Hotkey,
    /// Defaults for launching.
    pub launch: LaunchOptions,
    /// What "Capture current desktop" records.
    pub capture: CaptureOptions,
    /// Desktop shortcuts: a small app (no Terminal window) or a `.command`
    /// file (macOS). Windows always makes a `.lnk`.
    pub shortcut_kind: ShortcutKind,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: Hotkey::default(),
            launch: LaunchOptions::default(),
            capture: CaptureOptions::default(),
            shortcut_kind: ShortcutKind::default(),
        }
    }
}

impl Config {
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_json() {
        let c = Config::default();
        assert!(!c.hotkey.is_set());
        assert!(!c.capture.browser_urls);
        assert!(c.capture.documents);
        let c = Config::from_json(r#"{"hotkey":{"ctrl":true,"alt":true,"code":87},"launch":{"timeoutSecs":30}}"#).unwrap();
        assert_eq!(c.hotkey, Hotkey::new(false, true, true, false, 87));
        assert_eq!(c.launch.timeout_secs, 30);
        assert_eq!(c.launch.poll_ms, 250, "other launch fields keep their defaults");
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let s = serde_json::to_string(&Config::default()).unwrap();
        assert!(s.contains("\"missingMonitor\":\"scaleToPrimary\""), "{s}");
        assert!(s.contains("\"shortcutKind\":\"app\""), "{s}");
    }

    #[test]
    fn hotkey_display() {
        let h = Hotkey::new(false, true, true, false, 0x57);
        if cfg!(target_os = "macos") {
            assert_eq!(h.to_string(), "Ctrl+Option+W");
        } else {
            assert_eq!(h.to_string(), "Ctrl+Alt+W");
        }
    }
}
