//! Serializable settings (camelCase JSON, every field optional).
//!
//! Defaults follow PowerToys' Mouse Utilities defaults where they exist.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Top-level Mouse Utilities settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub find_my_mouse: FindMyMouseConfig,
    pub highlighter: HighlighterConfig,
    pub crosshairs: CrosshairsConfig,
}

/// A global hotkey: modifiers plus a Windows virtual-key code.
///
/// Same shape as PowerToys' hotkey settings JSON
/// (`{"win":true,"ctrl":false,"alt":false,"shift":true,"code":72}`).
///
/// * `win` is the Windows key on Windows and **Command** on macOS.
/// * `alt` is Alt on Windows and **Option** on macOS.
/// * `code` is a Windows `VK_*` code (letters are their ASCII capital, e.g.
///   `0x48` = H; digits are `0x30..0x39`; F1 = `0x70`). On macOS the key is
///   matched position-for-position via `pm_input::vk` (US layout).
///   `code == 0` disables the hotkey.
///
/// Modifiers must match exactly (Cmd+Shift+H does not fire on Cmd+Shift+Ctrl+H).
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

    /// A hotkey that never fires.
    pub const fn none() -> Self {
        Self::new(false, false, false, false, 0)
    }

    pub fn is_set(&self) -> bool {
        self.code != 0
    }

    /// Whether a key-down of `vk` with exactly `mods` held triggers this hotkey.
    pub fn matches(&self, vk: u32, mods: Mods) -> bool {
        self.is_set()
            && normalize_vk(vk) == normalize_vk(self.code)
            && mods.win == self.win
            && mods.ctrl == self.ctrl
            && mods.alt == self.alt
            && mods.shift == self.shift
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
            c @ (0x30..=0x39 | 0x41..=0x5A) => char::from_u32(c).unwrap().to_string(),
            c @ 0x70..=0x87 => format!("F{}", c - 0x6F),
            c => format!("VK 0x{c:02X}"),
        };
        parts.push(key);
        write!(f, "{}", parts.join("+"))
    }
}

/// Map left/right-specific modifier VKs to the generic ones so `code` can
/// use either form.
fn normalize_vk(vk: u32) -> u32 {
    match vk {
        0xA0 | 0xA1 => 0x10, // shift
        0xA2 | 0xA3 => 0x11, // ctrl
        0xA4 | 0xA5 => 0x12, // alt
        0x5C => 0x5B,        // win
        v => v,
    }
}

/// Held modifier state at the time of a key event (the key itself excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const fn new(win: bool, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self { win, ctrl, alt, shift }
    }
}

/// RGBA color. Serialized as a hex string: `"#RRGGBB"` (opaque) or
/// `"#AARRGGBB"` (alpha first, like PowerToys' highlighter colors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn argb(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn alpha_f(&self) -> f64 {
        self.a as f64 / 255.0
    }
    /// Same color with alpha multiplied by `k` (0..1).
    pub fn scaled_alpha(&self, k: f64) -> Self {
        let a = (self.a as f64 * k.clamp(0.0, 1.0)).round() as u8;
        Self { a, ..*self }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02X}{:02X}{:02X}{:02X}", self.a, self.r, self.g, self.b)
        }
    }
}

impl FromStr for Color {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let h = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| format!("invalid color {s:?}"));
        if !h.is_ascii() {
            return Err(format!("invalid color {s:?}"));
        }
        match h.len() {
            6 => Ok(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Ok(Color::argb(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => Err(format!("invalid color {s:?} (expected #RRGGBB or #AARRGGBB)")),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// How Find My Mouse is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ActivationMethod {
    /// Press Left Ctrl twice (PowerToys default).
    #[default]
    DoubleLeftControl,
    /// Press Right Ctrl twice.
    DoubleRightControl,
    /// Shake the mouse.
    ShakeMouse,
    /// Double Left Ctrl *or* shake.
    #[serde(alias = "both")]
    DoubleLeftControlOrShake,
    /// The `findMyMouse.hotkey` shortcut (toggles).
    Shortcut,
}

impl ActivationMethod {
    pub fn uses_double_ctrl(self) -> Option<bool /* right ctrl */> {
        match self {
            Self::DoubleLeftControl | Self::DoubleLeftControlOrShake => Some(false),
            Self::DoubleRightControl => Some(true),
            _ => None,
        }
    }
    pub fn uses_shake(self) -> bool {
        matches!(self, Self::ShakeMouse | Self::DoubleLeftControlOrShake)
    }
}

/// Find My Mouse: dims the screen except for a spotlight around the cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FindMyMouseConfig {
    pub enabled: bool,
    pub activation_method: ActivationMethod,
    /// Used when `activation_method` is `shortcut`. Default Cmd+Shift+F / Win+Shift+F.
    pub hotkey: Hotkey,
    /// Spotlight radius in points (DIPs on Windows).
    pub spotlight_radius: u32,
    /// Backdrop color (alpha in the color is ignored; see `overlay_opacity`).
    pub backdrop_color: Color,
    /// Backdrop opacity, 0..1.
    pub overlay_opacity: f64,
    /// Fade-in/out and zoom animation duration.
    pub animation_duration_ms: u32,
    /// Spotlight starts `initial_zoom` times larger and shrinks to its radius.
    pub initial_zoom: u32,
    /// After the mouse has moved, the spotlight hides once the mouse has been
    /// still this long.
    pub idle_timeout_ms: u32,
    /// Shake detection: minimum distance travelled (pixels) within `shake_interval_ms`.
    pub shake_minimum_distance: u32,
    pub shake_interval_ms: u32,
    /// Shake detection: distance travelled / diagonal of the movement's
    /// bounding box must exceed `shake_factor / 100`.
    pub shake_factor: u32,
}

impl Default for FindMyMouseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            activation_method: ActivationMethod::DoubleLeftControl,
            hotkey: Hotkey::new(true, false, false, true, 0x46),
            spotlight_radius: 100,
            backdrop_color: Color::rgb(0, 0, 0),
            overlay_opacity: 0.5,
            animation_duration_ms: 500,
            initial_zoom: 9,
            idle_timeout_ms: 1000,
            shake_minimum_distance: 1000,
            shake_interval_ms: 1000,
            shake_factor: 400,
        }
    }
}

/// Mouse Highlighter: colored circles at clicks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HighlighterConfig {
    pub enabled: bool,
    /// Toggle hotkey. Default Cmd+Shift+H (macOS) / Win+Shift+H (Windows).
    pub hotkey: Hotkey,
    /// Turn the highlighter on as soon as the module starts.
    pub auto_activate: bool,
    pub left_button_color: Color,
    pub right_button_color: Color,
    /// Also draw a circle at the cursor when no button is held.
    pub highlight_cursor: bool,
    pub always_color: Color,
    /// Circle radius in points.
    pub radius: u32,
    /// After release, the circle stays this long before fading.
    pub fade_delay_ms: u32,
    pub fade_duration_ms: u32,
}

impl Default for HighlighterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: Hotkey::new(true, false, false, true, 0x48),
            auto_activate: false,
            left_button_color: Color::argb(166, 191, 255, 0),
            right_button_color: Color::argb(166, 0, 191, 255),
            highlight_cursor: false,
            always_color: Color::argb(80, 255, 0, 0),
            radius: 30,
            fade_delay_ms: 400,
            fade_duration_ms: 400,
        }
    }
}

/// Which crosshair lines to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    #[default]
    Both,
    Vertical,
    Horizontal,
}

/// Mouse Pointer Crosshairs: lines through the cursor across the screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CrosshairsConfig {
    pub enabled: bool,
    /// Toggle hotkey. Default Cmd+Option+P (macOS) / Win+Alt+P (Windows).
    pub hotkey: Hotkey,
    pub auto_activate: bool,
    pub color: Color,
    pub border_color: Color,
    /// 0..1, applied to the whole crosshair.
    pub opacity: f64,
    /// Gap around the cursor (points).
    pub center_radius: u32,
    /// Line thickness (points), excluding the border.
    pub thickness: u32,
    /// Border drawn around each line (points); 0 = none.
    pub border_size: u32,
    pub orientation: Orientation,
    /// If non-zero, each arm is this long (points) instead of reaching the screen edge.
    pub fixed_length: u32,
}

impl Default for CrosshairsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: Hotkey::new(true, false, true, false, 0x50),
            auto_activate: false,
            color: Color::rgb(255, 0, 0),
            border_color: Color::rgb(255, 255, 255),
            opacity: 0.75,
            center_radius: 20,
            thickness: 5,
            border_size: 1,
            orientation: Orientation::Both,
            fixed_length: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parse_and_format() {
        assert_eq!("#FF0000".parse::<Color>().unwrap(), Color::rgb(255, 0, 0));
        assert_eq!("#A6BFFF00".parse::<Color>().unwrap(), Color::argb(0xA6, 0xBF, 0xFF, 0));
        assert_eq!("00bfff".parse::<Color>().unwrap(), Color::rgb(0, 0xBF, 0xFF));
        assert!("#12345".parse::<Color>().is_err());
        assert!("#GG0000".parse::<Color>().is_err());
        assert_eq!(Color::argb(0xA6, 0xBF, 0xFF, 0).to_string(), "#A6BFFF00");
        assert_eq!(Color::rgb(1, 2, 3).to_string(), "#010203");
        assert_eq!(Color::rgb(10, 10, 10).scaled_alpha(0.5).a, 128);
    }

    #[test]
    fn config_roundtrip_and_defaults() {
        let c = Config::default();
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"findMyMouse\""));
        assert!(s.contains("\"activationMethod\":\"doubleLeftControl\""));
        assert!(s.contains("\"leftButtonColor\":\"#A6BFFF00\""));
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);

        // Everything optional; partial objects keep defaults for the rest.
        let p: Config = serde_json::from_str(
            r##"{"findMyMouse":{"activationMethod":"both","spotlightRadius":50},
                "crosshairs":{"color":"#00FF00","orientation":"vertical","hotkey":{"ctrl":true,"code":67}}}"##,
        )
        .unwrap();
        assert_eq!(p.find_my_mouse.activation_method, ActivationMethod::DoubleLeftControlOrShake);
        assert_eq!(p.find_my_mouse.spotlight_radius, 50);
        assert_eq!(p.find_my_mouse.animation_duration_ms, 500);
        assert_eq!(p.crosshairs.color, Color::rgb(0, 255, 0));
        assert_eq!(p.crosshairs.orientation, Orientation::Vertical);
        assert_eq!(p.crosshairs.hotkey, Hotkey::new(false, true, false, false, 67));
        assert_eq!(p.highlighter, HighlighterConfig::default());
        let empty: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Config::default());
    }

    #[test]
    fn hotkey_matching() {
        let h = HighlighterConfig::default().hotkey; // Win/Cmd+Shift+H
        assert!(h.matches(0x48, Mods::new(true, false, false, true)));
        assert!(!h.matches(0x48, Mods::new(true, false, false, false)));
        assert!(!h.matches(0x48, Mods::new(true, true, false, true)));
        assert!(!h.matches(0x47, Mods::new(true, false, false, true)));
        assert!(!Hotkey::none().matches(0, Mods::default()));
        // left/right specific VKs normalize
        let ctrl_hk = Hotkey::new(false, false, false, true, 0x11);
        assert!(ctrl_hk.matches(0xA3, Mods::new(false, false, false, true)));
        assert!(Hotkey::new(false, true, true, false, 0x70).to_string().ends_with("F1"));
    }
}
