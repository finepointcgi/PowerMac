//! Serializable settings (camelCase JSON, every field optional).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Top-level display-control settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// Handle the brightness keys for external displays (the display under
    /// the pointer). Keys pressed while the pointer is on the built-in
    /// display, or on a display that can't be controlled, are left to macOS.
    /// No effect on Windows (brightness keys aren't keyboard events there).
    pub take_over_brightness_keys: bool,
    /// Handle the volume / mute keys for external displays with DDC volume
    /// (the display under the pointer); otherwise they reach the system.
    pub take_over_volume_keys: bool,
    /// Step per key press, in percent (1–50). Option+Shift (Alt+Shift on
    /// Windows) with a key uses a quarter step, like macOS.
    pub step: u8,
    /// Show the on-screen level indicator (macOS) after a key / hotkey.
    pub show_osd: bool,
    /// Dim displays that offer no hardware control (no DDC, no native
    /// brightness) through their gamma table. The table is restored when
    /// the module stops.
    pub software_dim_fallback: bool,
    pub hotkeys: Hotkeys,
    /// Per-display overrides keyed by stable display id (`DisplayInfo::id`).
    pub displays: BTreeMap<String, DisplayOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            take_over_brightness_keys: cfg!(target_os = "macos"),
            take_over_volume_keys: false,
            step: 6,
            show_osd: true,
            software_dim_fallback: true,
            hotkeys: Hotkeys::default(),
            displays: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn override_for(&self, id: &str) -> DisplayOverride {
        self.displays.get(id).cloned().unwrap_or_default()
    }

    /// Effective step (percent) for a display.
    pub fn step_for(&self, id: &str) -> u8 {
        let o = self.override_for(id);
        let s = if o.step > 0 { o.step } else { self.step };
        s.clamp(1, 50)
    }
}

/// Settings for one display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DisplayOverride {
    /// `false` excludes the display from keys, hotkeys and setters.
    pub enabled: bool,
    /// Use gamma dimming even if the display has DDC / native brightness.
    pub force_software_dim: bool,
    /// Raw maximum to use instead of the one the monitor reports (some
    /// report 100 but accept more, or report 0xFFFF). 0 = as reported.
    pub max_brightness: u16,
    pub max_contrast: u16,
    pub max_volume: u16,
    /// Step override in percent; 0 = the global step.
    pub step: u8,
}

impl Default for DisplayOverride {
    fn default() -> Self {
        Self { enabled: true, force_software_dim: false, max_brightness: 0, max_contrast: 0, max_volume: 0, step: 0 }
    }
}

/// Optional global hotkeys acting on the display under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkeys {
    pub brightness_up: Hotkey,
    pub brightness_down: Hotkey,
    pub contrast_up: Hotkey,
    pub contrast_down: Hotkey,
    pub volume_up: Hotkey,
    pub volume_down: Hotkey,
    pub mute: Hotkey,
}

/// Modifier state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    /// Command on macOS, Windows key on Windows.
    pub win: bool,
    pub ctrl: bool,
    /// Option on macOS.
    pub alt: bool,
    pub shift: bool,
}

/// A global hotkey: modifiers plus a Windows virtual-key code — the same
/// shape as PowerToys' hotkey JSON and `mouse_utils::Hotkey`
/// (`{"win":false,"ctrl":true,"alt":true,"shift":false,"code":38}`).
/// `win` is Command and `alt` is Option on macOS; keys are matched by
/// position (US layout) via `pm_input::vk`. `code == 0` disables it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
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

    /// Whether a key-down of `vk` with exactly `mods` held triggers it.
    pub fn matches(&self, vk: u32, mods: Mods) -> bool {
        self.is_set() && vk == self.code && mods.win == self.win && mods.ctrl == self.ctrl && mods.alt == self.alt && mods.shift == self.shift
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_set() {
            return write!(f, "(none)");
        }
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
        let key = match self.code {
            0x41..=0x5A | 0x30..=0x39 => (self.code as u8 as char).to_string(),
            0x70..=0x87 => format!("F{}", self.code - 0x6F),
            0x25 => "Left".into(),
            0x26 => "Up".into(),
            0x27 => "Right".into(),
            0x28 => "Down".into(),
            0xBB => "=".into(),
            0xBD => "-".into(),
            0xDB => "[".into(),
            0xDD => "]".into(),
            c => format!("VK 0x{c:02X}"),
        };
        parts.push(key);
        write!(f, "{}", parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_json() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert!(c.enabled);
        assert_eq!(c.take_over_brightness_keys, cfg!(target_os = "macos"));
        assert!(!c.take_over_volume_keys);
        let c: Config = serde_json::from_str(
            r#"{"step":10,"takeOverVolumeKeys":true,"displays":{"LG#1":{"maxVolume":50}},"hotkeys":{"brightnessUp":{"ctrl":true,"alt":true,"code":38}}}"#,
        )
        .unwrap();
        assert_eq!(c.step, 10);
        assert!(c.take_over_volume_keys);
        let o = c.override_for("LG#1");
        assert!(o.enabled, "missing fields default to enabled");
        assert_eq!(o.max_volume, 50);
        assert_eq!(c.override_for("nope"), DisplayOverride::default());
        assert!(c.hotkeys.brightness_up.matches(0x26, Mods { ctrl: true, alt: true, ..Default::default() }));
        assert!(!c.hotkeys.brightness_up.matches(0x26, Mods { ctrl: true, ..Default::default() }));
        assert!(!c.hotkeys.volume_up.matches(0, Mods::default()));
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert!(serde_json::to_string(&c).unwrap().contains("\"softwareDimFallback\":true"));
    }

    #[test]
    fn step_override_and_clamp() {
        let mut c = Config::default();
        assert_eq!(c.step_for("x"), 6);
        c.displays.insert("x".into(), DisplayOverride { step: 25, ..Default::default() });
        assert_eq!(c.step_for("x"), 25);
        c.step = 0;
        assert_eq!(c.step_for("y"), 1);
        c.step = 200;
        assert_eq!(c.step_for("y"), 50);
    }

    #[test]
    fn hotkey_display() {
        let h = Hotkey::new(false, true, true, false, 0x26);
        let s = h.to_string();
        assert!(s.starts_with("Ctrl+") && s.ends_with("+Up"), "{s}");
        assert_eq!(Hotkey::default().to_string(), "(none)");
        assert!(Hotkey::new(false, false, false, false, 0x71).to_string().ends_with("F2"));
    }
}
