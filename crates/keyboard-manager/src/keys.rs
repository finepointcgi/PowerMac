//! Keys, modifiers and shortcuts, plus their human-readable names.
//!
//! Every key is a Windows virtual-key code (`VK_*`), on every platform. On
//! macOS the backend translates with [`pm_input::vk`]: Command is the Windows
//! key (`VK_LWIN`/`VK_RWIN`), Option is Alt (`VK_LMENU`/`VK_RMENU`).
//!
//! In config files a key can be written as a JSON number (`20`), a hex string
//! (`"0x14"`) or a name (`"CapsLock"`). Names are case-insensitive; see
//! [`parse_key`] for the full list. A shortcut is a string such as
//! `"Ctrl+Shift+V"` / `"Cmd+Option+Left"`, or an array of keys.

use std::fmt;

use pm_input::vk::*;
use serde::{Deserialize, Serialize};

/// PowerToys' artificial "either Windows key" code. Also what `"Win"`/`"Cmd"` parse to.
pub const VK_WIN_BOTH: u32 = 0x104;
/// PowerToys' "disabled" target code (only used by the PowerToys importer).
pub const VK_DISABLED: u32 = 0x100;
/// The unassigned key PowerToys injects to stop a lone Win/Alt release from
/// opening the Start menu / menu bar (only emitted on Windows).
pub const VK_DUMMY: u32 = 0xFF;

/// The four modifier families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModFamily {
    Win,
    Ctrl,
    Alt,
    Shift,
}

impl ModFamily {
    pub const ALL: [ModFamily; 4] = [
        ModFamily::Win,
        ModFamily::Ctrl,
        ModFamily::Alt,
        ModFamily::Shift,
    ];

    /// (left, right) sided VKs.
    pub fn sided(self) -> (u32, u32) {
        match self {
            ModFamily::Win => (VK_LWIN, VK_RWIN),
            ModFamily::Ctrl => (VK_LCONTROL, VK_RCONTROL),
            ModFamily::Alt => (VK_LMENU, VK_RMENU),
            ModFamily::Shift => (VK_LSHIFT, VK_RSHIFT),
        }
    }
}

/// Which side(s) of a modifier family a shortcut requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Side {
    /// Not part of the shortcut (and must not be held).
    #[default]
    None,
    Left,
    Right,
    /// Either side.
    Either,
}

/// The modifier family of a (generic or sided) modifier VK.
pub fn mod_family(vk: u32) -> Option<ModFamily> {
    Some(match vk {
        VK_LWIN | VK_RWIN | VK_WIN_BOTH => ModFamily::Win,
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => ModFamily::Ctrl,
        VK_MENU | VK_LMENU | VK_RMENU => ModFamily::Alt,
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => ModFamily::Shift,
        _ => return None,
    })
}

/// True for generic and sided modifier VKs (Win/Ctrl/Alt/Shift; not CapsLock).
pub fn is_modifier(vk: u32) -> bool {
    mod_family(vk).is_some()
}

/// Which side a modifier VK denotes (`Either` for generic codes).
pub fn mod_side(vk: u32) -> Side {
    match vk {
        VK_LWIN | VK_LCONTROL | VK_LMENU | VK_LSHIFT => Side::Left,
        VK_RWIN | VK_RCONTROL | VK_RMENU | VK_RSHIFT => Side::Right,
        VK_WIN_BOTH | VK_CONTROL | VK_MENU | VK_SHIFT => Side::Either,
        _ => Side::None,
    }
}

/// Map artificial / generic codes to a concrete sided key for injection
/// (`VK_WIN_BOTH`/generic modifiers -> the left key).
pub fn concrete(vk: u32) -> u32 {
    match vk {
        VK_WIN_BOTH => VK_LWIN,
        VK_CONTROL => VK_LCONTROL,
        VK_MENU => VK_LMENU,
        VK_SHIFT => VK_LSHIFT,
        other => other,
    }
}

/// Modifier requirements of a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub win: Side,
    pub ctrl: Side,
    pub alt: Side,
    pub shift: Side,
}

impl Mods {
    pub fn get(&self, f: ModFamily) -> Side {
        match f {
            ModFamily::Win => self.win,
            ModFamily::Ctrl => self.ctrl,
            ModFamily::Alt => self.alt,
            ModFamily::Shift => self.shift,
        }
    }

    pub fn set(&mut self, f: ModFamily, s: Side) {
        match f {
            ModFamily::Win => self.win = s,
            ModFamily::Ctrl => self.ctrl = s,
            ModFamily::Alt => self.alt = s,
            ModFamily::Shift => self.shift = s,
        }
    }

    pub fn is_empty(&self) -> bool {
        ModFamily::ALL.iter().all(|&f| self.get(f) == Side::None)
    }

    /// Add a modifier key (generic or sided). Combining Left and Right gives Either.
    pub fn add(&mut self, vk: u32) {
        let Some(f) = mod_family(vk) else { return };
        let s = mod_side(vk);
        let cur = self.get(f);
        let new = match (cur, s) {
            (Side::None, s) => s,
            (a, b) if a == b => a,
            _ => Side::Either,
        };
        self.set(f, new);
    }

    /// Exact match against a set of held sided modifier keys: every required
    /// family is held on an acceptable side and no other modifier is held.
    pub fn matches_exactly(&self, held: impl Fn(u32) -> bool) -> bool {
        ModFamily::ALL.iter().all(|&f| {
            let (l, r) = f.sided();
            let (hl, hr) = (held(l), held(r));
            match self.get(f) {
                Side::None => !hl && !hr,
                Side::Left => hl && !hr,
                Side::Right => hr && !hl,
                Side::Either => hl || hr,
            }
        })
    }
}

/// A key combination: modifiers plus one non-modifier action key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub mods: Mods,
    /// The action key (never a modifier).
    pub key: u32,
}

impl Shortcut {
    pub fn new(mods: Mods, key: u32) -> Self {
        Self { mods, key }
    }

    /// Build from a list of keys (modifiers in any order plus exactly one
    /// non-modifier key).
    pub fn from_keys(keys: &[u32]) -> Result<Self, String> {
        let mut mods = Mods::default();
        let mut action = None;
        for &k in keys {
            if is_modifier(k) {
                mods.add(k);
            } else if action.replace(k).is_some() {
                return Err(
                    "a shortcut can only have one non-modifier key (chords are not supported)"
                        .into(),
                );
            }
        }
        let key = action.ok_or_else(|| "a shortcut needs a non-modifier action key".to_string())?;
        if mods.is_empty() {
            return Err(
                "a shortcut needs at least one modifier (use a key remap for single keys)".into(),
            );
        }
        Ok(Self { mods, key })
    }

    /// Modifier keys in canonical order (Win, Ctrl, Alt, Shift) as generic or
    /// sided VKs, followed by the action key.
    pub fn keys(&self) -> Vec<u32> {
        let mut v = Vec::new();
        for f in ModFamily::ALL {
            let (l, r) = f.sided();
            match self.mods.get(f) {
                Side::None => {}
                Side::Left => v.push(l),
                Side::Right => v.push(r),
                Side::Either => v.push(match f {
                    ModFamily::Win => VK_WIN_BOTH,
                    ModFamily::Ctrl => VK_CONTROL,
                    ModFamily::Alt => VK_MENU,
                    ModFamily::Shift => VK_SHIFT,
                }),
            }
        }
        v.push(self.key);
        v
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.keys().into_iter().map(key_name).collect();
        f.write_str(&names.join("+"))
    }
}

impl std::str::FromStr for Shortcut {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Shortcut::from_keys(&parse_combo(s)?)
    }
}

/// Split `"Ctrl+Shift+V"` into VKs. `"Ctrl++"` means Ctrl + the `=`/`+` key.
pub fn parse_combo(s: &str) -> Result<Vec<u32>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty key combination".into());
    }
    let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
    // "Ctrl++" splits into ["Ctrl", "", ""]: a trailing empty pair is the plus key.
    if s.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    } else if s == "+" {
        parts = vec!["+"];
    }
    parts
        .into_iter()
        .map(|p| parse_key(p).ok_or_else(|| format!("unknown key name '{p}' in '{s}'")))
        .collect()
}

/// A single key in config: a JSON number or a name / hex string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(pub u32);

impl Serialize for Key {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&key_name(self.0))
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Num(u32),
            Str(String),
        }
        match Repr::deserialize(d)? {
            Repr::Num(n) => Ok(Key(n)),
            Repr::Str(s) => parse_key(&s)
                .map(Key)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown key name '{s}'"))),
        }
    }
}

// (canonical name first, aliases after)
const NAMES: &[(u32, &[&str])] = &[
    (VK_BACK, &["Backspace", "Back"]),
    (VK_TAB, &["Tab"]),
    (VK_CLEAR, &["Clear"]),
    (VK_RETURN, &["Enter", "Return"]),
    (VK_PAUSE, &["Pause", "Break"]),
    (VK_CAPITAL, &["CapsLock", "Caps", "Capital"]),
    (VK_ESCAPE, &["Esc", "Escape"]),
    (VK_SPACE, &["Space", "Spacebar"]),
    (VK_PRIOR, &["PageUp", "PgUp", "Prior"]),
    (VK_NEXT, &["PageDown", "PgDn", "Next"]),
    (VK_END, &["End"]),
    (VK_HOME, &["Home"]),
    (VK_LEFT, &["Left", "LeftArrow", "ArrowLeft"]),
    (VK_UP, &["Up", "UpArrow", "ArrowUp"]),
    (VK_RIGHT, &["Right", "RightArrow", "ArrowRight"]),
    (VK_DOWN, &["Down", "DownArrow", "ArrowDown"]),
    (VK_SNAPSHOT, &["PrintScreen", "PrtSc", "Snapshot"]),
    (VK_INSERT, &["Insert", "Ins"]),
    (VK_DELETE, &["Delete", "Del", "ForwardDelete"]),
    (VK_HELP, &["Help"]),
    (VK_APPS, &["Menu", "Apps", "ContextMenu"]),
    (VK_SLEEP, &["Sleep"]),
    (VK_MULTIPLY, &["NumpadMultiply", "Multiply"]),
    (VK_ADD, &["NumpadAdd", "Add"]),
    (VK_SEPARATOR, &["NumpadSeparator", "Separator"]),
    (VK_SUBTRACT, &["NumpadSubtract", "Subtract"]),
    (VK_DECIMAL, &["NumpadDecimal", "Decimal"]),
    (VK_DIVIDE, &["NumpadDivide", "Divide"]),
    (VK_NUMLOCK, &["NumLock"]),
    (VK_SCROLL, &["ScrollLock", "Scroll"]),
    // Modifiers. Generic codes mean "either side".
    (
        VK_WIN_BOTH,
        if cfg!(target_os = "macos") {
            &["Cmd", "Command", "Win", "Windows", "Meta", "Super"]
        } else {
            &["Win", "Windows", "Cmd", "Command", "Meta", "Super"]
        },
    ),
    (
        VK_LWIN,
        if cfg!(target_os = "macos") {
            &["LCmd", "LeftCmd", "LCommand", "LWin", "LeftWin"]
        } else {
            &["LWin", "LeftWin", "LCmd", "LeftCmd", "LCommand"]
        },
    ),
    (
        VK_RWIN,
        if cfg!(target_os = "macos") {
            &["RCmd", "RightCmd", "RCommand", "RWin", "RightWin"]
        } else {
            &["RWin", "RightWin", "RCmd", "RightCmd", "RCommand"]
        },
    ),
    (VK_CONTROL, &["Ctrl", "Control"]),
    (VK_LCONTROL, &["LCtrl", "LeftCtrl", "LControl"]),
    (VK_RCONTROL, &["RCtrl", "RightCtrl", "RControl"]),
    (
        VK_MENU,
        if cfg!(target_os = "macos") {
            &["Option", "Opt", "Alt"]
        } else {
            &["Alt", "Option", "Opt"]
        },
    ),
    (
        VK_LMENU,
        if cfg!(target_os = "macos") {
            &["LOption", "LeftOption", "LOpt", "LAlt", "LeftAlt"]
        } else {
            &["LAlt", "LeftAlt", "LOption", "LeftOption", "LOpt"]
        },
    ),
    (
        VK_RMENU,
        if cfg!(target_os = "macos") {
            &[
                "ROption",
                "RightOption",
                "ROpt",
                "RAlt",
                "RightAlt",
                "AltGr",
            ]
        } else {
            &[
                "RAlt",
                "RightAlt",
                "AltGr",
                "ROption",
                "RightOption",
                "ROpt",
            ]
        },
    ),
    (VK_SHIFT, &["Shift"]),
    (VK_LSHIFT, &["LShift", "LeftShift"]),
    (VK_RSHIFT, &["RShift", "RightShift"]),
    // Media / browser
    (VK_BROWSER_BACK, &["BrowserBack"]),
    (VK_BROWSER_FORWARD, &["BrowserForward"]),
    (VK_BROWSER_REFRESH, &["BrowserRefresh"]),
    (VK_BROWSER_STOP, &["BrowserStop"]),
    (VK_BROWSER_SEARCH, &["BrowserSearch"]),
    (VK_BROWSER_FAVORITES, &["BrowserFavorites"]),
    (VK_BROWSER_HOME, &["BrowserHome"]),
    (VK_VOLUME_MUTE, &["VolumeMute", "Mute"]),
    (VK_VOLUME_DOWN, &["VolumeDown"]),
    (VK_VOLUME_UP, &["VolumeUp"]),
    (VK_MEDIA_NEXT_TRACK, &["MediaNext", "NextTrack"]),
    (
        VK_MEDIA_PREV_TRACK,
        &["MediaPrev", "MediaPrevious", "PrevTrack"],
    ),
    (VK_MEDIA_STOP, &["MediaStop"]),
    (VK_MEDIA_PLAY_PAUSE, &["MediaPlayPause", "PlayPause"]),
    (VK_LAUNCH_MAIL, &["LaunchMail"]),
    (VK_LAUNCH_MEDIA_SELECT, &["LaunchMedia"]),
    (VK_LAUNCH_APP1, &["LaunchApp1"]),
    (VK_LAUNCH_APP2, &["LaunchApp2"]),
    // OEM keys (US layout positions)
    (VK_OEM_1, &[";", "Semicolon"]),
    (VK_OEM_PLUS, &["=", "Equal", "Equals", "Plus", "+"]),
    (VK_OEM_COMMA, &[",", "Comma"]),
    (VK_OEM_MINUS, &["-", "Minus"]),
    (VK_OEM_PERIOD, &[".", "Period"]),
    (VK_OEM_2, &["/", "Slash"]),
    (VK_OEM_3, &["`", "Grave", "Backtick", "Backquote", "Tilde"]),
    (VK_OEM_4, &["[", "LeftBracket", "BracketLeft"]),
    (VK_OEM_5, &["\\", "Backslash"]),
    (VK_OEM_6, &["]", "RightBracket", "BracketRight"]),
    (VK_OEM_7, &["'", "Quote", "Apostrophe"]),
    (VK_OEM_102, &["IntlBackslash", "Oem102"]),
    (VK_DUMMY, &["Dummy"]),
    (VK_DISABLED, &["Disabled"]),
];

/// Parse a key name (case-insensitive), a hex code (`"0x41"`), or a decimal
/// code with a `#` prefix (`"#65"`).
///
/// Names: `A`..`Z`, `0`..`9`, `F1`..`F24`, `Numpad0`..`Numpad9`,
/// `NumpadMultiply/Add/Subtract/Decimal/Divide`, `Esc`, `Tab`, `CapsLock`,
/// `Space`, `Enter`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`,
/// `PageDown`, `Left`, `Right`, `Up`, `Down`, `PrintScreen`, `ScrollLock`,
/// `Pause`, `NumLock`, `Menu`, modifiers `Ctrl`/`Shift`/`Alt`(=`Option`)/
/// `Win`(=`Cmd`) (either side) and their `L`/`R` sided forms (`LCtrl`, `RCmd`,
/// `ROption`, ...), punctuation (`;` `=` `,` `-` `.` `/` `` ` `` `[` `\` `]`
/// `'` or `Semicolon`, `Equal`, `Comma`, ...), media keys (`VolumeUp`,
/// `MediaPlayPause`, ...).
pub fn parse_key(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some(dec) = t.strip_prefix('#') {
        return dec.parse().ok();
    }
    let lower = t.to_ascii_lowercase();
    if t.len() == 1 {
        let c = t.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(VK_F1 + n - 1);
        }
    }
    if let Some(n) = lower
        .strip_prefix("numpad")
        .or_else(|| lower.strip_prefix("num"))
        .and_then(|n| n.parse::<u32>().ok())
    {
        if n <= 9 {
            return Some(VK_NUMPAD0 + n);
        }
    }
    NAMES
        .iter()
        .find(|(_, names)| names.iter().any(|n| n.eq_ignore_ascii_case(t)))
        .map(|&(vk, _)| vk)
}

/// Canonical display name of a VK (`"0x.."` for unnamed codes).
pub fn key_name(vk: u32) -> String {
    if (VK_A..=VK_Z).contains(&vk) || (VK_0..=VK_9).contains(&vk) {
        return char::from_u32(vk).map(String::from).unwrap_or_default();
    }
    if (VK_F1..=VK_F24).contains(&vk) {
        return format!("F{}", vk - VK_F1 + 1);
    }
    if (VK_NUMPAD0..=VK_NUMPAD9).contains(&vk) {
        return format!("Numpad{}", vk - VK_NUMPAD0);
    }
    if let Some((_, names)) = NAMES.iter().find(|(v, _)| *v == vk) {
        return names[0].to_string();
    }
    format!("0x{vk:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for vk in 1..=0x104u32 {
            let name = key_name(vk);
            assert_eq!(parse_key(&name), Some(vk), "{vk:#x} -> {name}");
        }
    }

    #[test]
    fn parse_names() {
        assert_eq!(parse_key("capslock"), Some(VK_CAPITAL));
        assert_eq!(parse_key("ESC"), Some(VK_ESCAPE));
        assert_eq!(parse_key("a"), Some(VK_A));
        assert_eq!(parse_key("7"), Some(VK_7));
        assert_eq!(parse_key("F13"), Some(VK_F13));
        assert_eq!(parse_key("numpad5"), Some(VK_NUMPAD5));
        assert_eq!(parse_key("Cmd"), Some(VK_WIN_BOTH));
        assert_eq!(parse_key("option"), Some(VK_MENU));
        assert_eq!(parse_key("ROption"), Some(VK_RMENU));
        assert_eq!(parse_key("0x41"), Some(VK_A));
        assert_eq!(parse_key("#27"), Some(VK_ESCAPE));
        assert_eq!(parse_key("nope"), None);
        assert_eq!(parse_key("F25"), None);
    }

    #[test]
    fn shortcuts() {
        let s: Shortcut = "Ctrl+Shift+V".parse().unwrap();
        assert_eq!(s.mods.ctrl, Side::Either);
        assert_eq!(s.mods.shift, Side::Either);
        assert_eq!(s.mods.win, Side::None);
        assert_eq!(s.key, VK_V);
        let s: Shortcut = "lcmd + left".parse().unwrap();
        assert_eq!(s.mods.win, Side::Left);
        assert_eq!(s.key, VK_LEFT);
        let s: Shortcut = "Ctrl++".parse().unwrap();
        assert_eq!(s.key, VK_OEM_PLUS);
        assert!("Ctrl+Shift".parse::<Shortcut>().is_err());
        assert!("A+B".parse::<Shortcut>().is_err());
        assert!("Ctrl+K+D".parse::<Shortcut>().is_err());
        // LCtrl + RCtrl => either
        let s = Shortcut::from_keys(&[VK_LCONTROL, VK_RCONTROL, VK_A]).unwrap();
        assert_eq!(s.mods.ctrl, Side::Either);
        // display round trip
        let s: Shortcut = "Win+Ctrl+Alt+V".parse().unwrap();
        assert_eq!(s.to_string().parse::<Shortcut>().unwrap(), s);
    }

    #[test]
    fn exact_modifier_matching() {
        let s: Shortcut = "Ctrl+C".parse().unwrap();
        assert!(s.mods.matches_exactly(|k| k == VK_LCONTROL));
        assert!(s.mods.matches_exactly(|k| k == VK_RCONTROL));
        assert!(!s
            .mods
            .matches_exactly(|k| k == VK_LCONTROL || k == VK_LSHIFT));
        assert!(!s.mods.matches_exactly(|_| false));
        let s: Shortcut = "LCtrl+C".parse().unwrap();
        assert!(!s.mods.matches_exactly(|k| k == VK_RCONTROL));
    }
}
