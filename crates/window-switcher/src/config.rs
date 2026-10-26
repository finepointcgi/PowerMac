//! Serializable settings (camelCase JSON, every field optional).

use std::fmt;

use pm_input::vk;
use serde::{Deserialize, Serialize};

/// Window switcher settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch. On by default: the default shortcuts (Option+Tab /
    /// Option+` on macOS, Alt+` on Windows) don't override system shortcuts.
    pub enabled: bool,
    /// Hold the modifier(s), press the key: switch between all windows.
    /// Default Option+Tab on macOS; unset on Windows (its own Alt+Tab
    /// already switches windows; see [`Config::replace_cmd_tab`]).
    pub trigger: Hotkey,
    /// Same, but only the windows of the frontmost app. Default Option+`
    /// (macOS) / Alt+` (Windows).
    pub app_windows_trigger: Hotkey,
    /// Also take over Cmd+Tab and Cmd+Shift+Tab (macOS; the system app
    /// switcher is disabled while this is on and restored when the switcher
    /// stops, even after a crash) or Alt+Tab (Windows). [`Config::trigger`]
    /// keeps working as a second shortcut.
    pub replace_cmd_tab: bool,
    /// macOS: Cmd+` opens the switcher for the front app's windows (the
    /// system's "Move focus to next window" is off while this is on). Only
    /// used together with [`Config::replace_cmd_tab`].
    pub replace_cmd_backtick: bool,
    /// While the switcher is open, show the selected window's live image at
    /// its real position on screen (AltTab's "Preview selected window").
    /// Minimized windows and windows on other Spaces are shown centered.
    pub preview_selected_window: bool,
    /// Dim the rest of the screen behind the preview.
    pub dim_background: bool,
    /// How dark the dimming is (0 = none, 1 = black).
    pub dim_opacity: f64,
    /// List minimized windows.
    pub show_minimized: bool,
    /// List windows of hidden apps.
    pub show_hidden: bool,
    /// List windows on other Spaces (macOS) / virtual desktops (Windows).
    pub show_other_spaces: bool,
    /// Only list windows on the screen that has the mouse pointer.
    pub only_current_screen: bool,
    /// Show live window thumbnails (macOS: needs Screen Recording; without it
    /// the switcher shows large app icons instead).
    pub thumbnails: bool,
    /// How long the shortcut must be held before the switcher appears. A
    /// quicker tap-and-release switches straight to the previous window
    /// without showing anything ("fast switch").
    pub show_delay_ms: u32,
    /// Apps whose windows are never listed: bundle ids (`com.apple.Safari`),
    /// app names (`Safari`) or executable names (`notepad.exe`),
    /// case-insensitive.
    pub excluded_apps: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger: Hotkey::default_trigger(),
            app_windows_trigger: Hotkey::default_app_trigger(),
            replace_cmd_tab: false,
            replace_cmd_backtick: false,
            preview_selected_window: true,
            dim_background: true,
            dim_opacity: DEFAULT_DIM_OPACITY,
            show_minimized: true,
            show_hidden: true,
            show_other_spaces: true,
            only_current_screen: false,
            thumbnails: true,
            show_delay_ms: DEFAULT_SHOW_DELAY_MS,
            excluded_apps: Vec::new(),
        }
    }
}

/// Default for [`Config::show_delay_ms`]: short enough to feel instant,
/// long enough that a quick Option+Tab tap doesn't flash the overlay.
pub const DEFAULT_SHOW_DELAY_MS: u32 = 90;

/// Default for [`Config::dim_opacity`].
pub const DEFAULT_DIM_OPACITY: f64 = 0.25;

impl Config {
    /// Whether the system switcher (Cmd+Tab) is taken over.
    pub fn replaces_system_switcher(&self) -> bool {
        self.enabled && self.replace_cmd_tab
    }

    /// Whether Cmd+` is taken over (macOS only).
    pub fn replaces_cmd_backtick(&self) -> bool {
        cfg!(target_os = "macos") && self.replaces_system_switcher() && self.replace_cmd_backtick
    }

    /// Dimming behind the preview (`None` = no dimming), clamped to a sane range.
    pub fn effective_dim(&self) -> Option<f64> {
        let o = if self.dim_opacity.is_finite() { self.dim_opacity.clamp(0.0, 0.85) } else { DEFAULT_DIM_OPACITY };
        (self.preview_selected_window && self.dim_background && o > 0.0).then_some(o)
    }

    /// Whether `app` (bundle id, name or path) is in [`Config::excluded_apps`].
    pub fn is_excluded(&self, ids: &[&str]) -> bool {
        self.excluded_apps.iter().any(|ex| {
            let ex = ex.trim();
            !ex.is_empty()
                && ids.iter().any(|id| {
                    let id = id.trim();
                    if id.is_empty() {
                        return false;
                    }
                    if id.eq_ignore_ascii_case(ex) {
                        return true;
                    }
                    // Match the last path component of paths (`/Applications/Foo.app`, `C:\x\foo.exe`).
                    let base = id.rsplit(['/', '\\']).next().unwrap_or(id);
                    base.eq_ignore_ascii_case(ex)
                        || base.strip_suffix(".app").is_some_and(|b| b.eq_ignore_ascii_case(ex))
                        || base.strip_suffix(".exe").is_some_and(|b| b.eq_ignore_ascii_case(ex))
                })
        })
    }
}

/// A global shortcut: modifiers plus a Windows virtual-key code — the same
/// JSON shape as PowerToys / the other PowerMac modules
/// (`{"win":false,"ctrl":false,"alt":true,"shift":false,"code":9}`).
///
/// * `win` is the Windows key on Windows and **Command** on macOS.
/// * `alt` is Alt on Windows and **Option** on macOS.
/// * `code` is a Windows `VK_*` code (Tab = 9, ` = 0xC0). `0` disables it.
///
/// For the switcher the modifiers are the ones you *hold*; Shift is extra
/// (it reverses the direction) unless the hotkey itself includes Shift.
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

    pub const fn none() -> Self {
        Self::new(false, false, false, false, 0)
    }

    /// Option+Tab (macOS) / unset (Windows).
    pub const fn default_trigger() -> Self {
        if cfg!(target_os = "windows") {
            Self::none()
        } else {
            Self::new(false, false, true, false, vk::VK_TAB)
        }
    }

    /// Option+` (macOS) / Alt+` (Windows).
    pub const fn default_app_trigger() -> Self {
        Self::new(false, false, true, false, vk::VK_OEM_3)
    }

    /// Cmd+` (macOS), used by `replace_cmd_backtick`.
    pub const fn system_app_switcher() -> Self {
        Self::new(true, false, false, false, vk::VK_OEM_3)
    }

    /// Cmd+Tab (macOS) / Alt+Tab (Windows), used by `replace_cmd_tab`.
    pub const fn system_switcher() -> Self {
        if cfg!(target_os = "windows") {
            Self::new(false, false, true, false, vk::VK_TAB)
        } else {
            Self::new(true, false, false, false, vk::VK_TAB)
        }
    }

    pub fn is_set(&self) -> bool {
        self.code != 0
    }

    /// The modifiers that must stay held while the switcher is open
    /// (Shift only if the hotkey includes it).
    pub fn held(&self) -> Mods {
        Mods { win: self.win, ctrl: self.ctrl, alt: self.alt, shift: self.shift }
    }

    /// Whether a key-down of `vk` with `mods` held fires this hotkey.
    /// Shift may be added (it means "backwards") unless the hotkey uses it.
    pub fn matches(&self, vk: u32, mods: Mods) -> bool {
        self.is_set()
            && vk == self.code
            && mods.win == self.win
            && mods.ctrl == self.ctrl
            && mods.alt == self.alt
            && (self.shift && mods.shift || !self.shift)
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
        let key = match self.code {
            0 => "(none)".to_string(),
            vk::VK_TAB => "Tab".into(),
            vk::VK_OEM_3 => "`".into(),
            vk::VK_SPACE => "Space".into(),
            vk::VK_ESCAPE => "Esc".into(),
            c @ (0x30..=0x39 | 0x41..=0x5A) => char::from_u32(c).unwrap_or('?').to_string(),
            c @ 0x70..=0x87 => format!("F{}", c - 0x6F),
            c => format!("VK 0x{c:02X}"),
        };
        parts.push(key);
        write!(f, "{}", parts.join("+"))
    }
}

/// Held modifiers at the time of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Mods {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { win: false, ctrl: false, alt: false, shift: false };

    pub const fn new(win: bool, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self { win, ctrl, alt, shift }
    }

    /// Every modifier set in `other` is also set here.
    pub fn contains(&self, other: Mods) -> bool {
        (!other.win || self.win) && (!other.ctrl || self.ctrl) && (!other.alt || self.alt) && (!other.shift || self.shift)
    }

    pub fn is_empty(&self) -> bool {
        *self == Mods::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_camel_case() {
        let c = Config::default();
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(j["enabled"], true);
        assert_eq!(j["replaceCmdTab"], false);
        assert_eq!(j["replaceCmdBacktick"], false);
        assert_eq!(j["previewSelectedWindow"], true);
        assert_eq!(j["dimBackground"], true);
        assert_eq!(j["dimOpacity"], 0.25);
        assert_eq!(j["showMinimized"], true);
        assert_eq!(j["showOtherSpaces"], true);
        assert_eq!(j["onlyCurrentScreen"], false);
        assert_eq!(j["appWindowsTrigger"]["code"], 0xC0);
        let back: Config = serde_json::from_value(j).unwrap();
        assert_eq!(back, c);
        // Missing fields fall back to defaults.
        let partial: Config = serde_json::from_str(r#"{"thumbnails":false}"#).unwrap();
        assert!(!partial.thumbnails);
        assert!(partial.enabled);
        assert_eq!(partial.show_delay_ms, DEFAULT_SHOW_DELAY_MS);
    }

    #[test]
    fn system_switcher_and_dim_rules() {
        let mut c = Config::default();
        assert!(!c.replaces_system_switcher());
        assert_eq!(c.effective_dim(), Some(0.25));
        c.replace_cmd_tab = true;
        c.replace_cmd_backtick = true;
        assert!(c.replaces_system_switcher());
        assert_eq!(c.replaces_cmd_backtick(), cfg!(target_os = "macos"));
        c.enabled = false;
        assert!(!c.replaces_system_switcher() && !c.replaces_cmd_backtick());
        c.dim_opacity = 3.0;
        assert_eq!(c.effective_dim(), Some(0.85));
        c.dim_opacity = f64::NAN;
        assert_eq!(c.effective_dim(), Some(DEFAULT_DIM_OPACITY));
        c.dim_opacity = 0.0;
        assert_eq!(c.effective_dim(), None);
        c.dim_opacity = 0.4;
        c.preview_selected_window = false;
        assert_eq!(c.effective_dim(), None, "dimming belongs to the preview");
    }

    #[test]
    fn hotkey_shift_is_direction() {
        let t = Hotkey::new(false, false, true, false, vk::VK_TAB);
        assert!(t.matches(vk::VK_TAB, Mods::new(false, false, true, false)));
        assert!(t.matches(vk::VK_TAB, Mods::new(false, false, true, true)));
        assert!(!t.matches(vk::VK_TAB, Mods::new(true, false, true, false)));
        assert!(!t.matches(vk::VK_TAB, Mods::NONE));
        assert!(!Hotkey::none().matches(0, Mods::NONE));
        let s = Hotkey::new(false, true, false, true, vk::VK_TAB);
        assert!(!s.matches(vk::VK_TAB, Mods::new(false, true, false, false)));
        assert!(s.matches(vk::VK_TAB, Mods::new(false, true, false, true)));
    }

    #[test]
    fn exclusion_matching() {
        let c = Config { excluded_apps: vec!["com.apple.Safari".into(), "Notes".into(), "notepad.exe".into()], ..Default::default() };
        assert!(c.is_excluded(&["COM.APPLE.SAFARI"]));
        assert!(c.is_excluded(&["", "/System/Applications/Notes.app"]));
        assert!(c.is_excluded(&["C:\\Windows\\notepad.exe"]));
        assert!(!c.is_excluded(&["com.apple.TextEdit", "TextEdit"]));
        assert!(!Config::default().is_excluded(&["anything"]));
    }

    #[test]
    fn display() {
        let s = Hotkey::new(false, false, true, false, vk::VK_TAB).to_string();
        assert!(s == "Option+Tab" || s == "Alt+Tab");
    }
}
