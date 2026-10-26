//! Key codes used by [`crate::config::Hotkey`]: hotkeys are stored as
//! Windows virtual-key codes (like PowerToys' `HotkeyObject::code`) on every
//! platform; the macOS backend translates them to `kVK_*` key codes.

/// Windows virtual-key codes (the subset FancyZones hotkeys use).
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const ESCAPE: u16 = 0x1B;
    pub const SPACE: u16 = 0x20;
    pub const PRIOR: u16 = 0x21; // Page Up
    pub const NEXT: u16 = 0x22; // Page Down
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
    pub const DELETE: u16 = 0x2E;
    pub const KEY_0: u16 = 0x30;
    pub const KEY_9: u16 = 0x39;
    pub const KEY_A: u16 = 0x41;
    pub const KEY_X: u16 = 0x58;
    pub const KEY_Z: u16 = 0x5A;
    pub const NUMPAD0: u16 = 0x60;
    pub const NUMPAD9: u16 = 0x69;
    pub const F1: u16 = 0x70;
    pub const F12: u16 = 0x7B;
    pub const OEM_1: u16 = 0xBA; // ;
    pub const OEM_PLUS: u16 = 0xBB; // =
    pub const OEM_COMMA: u16 = 0xBC;
    pub const OEM_MINUS: u16 = 0xBD;
    pub const OEM_PERIOD: u16 = 0xBE;
    pub const OEM_2: u16 = 0xBF; // /
    pub const OEM_3: u16 = 0xC0; // `
    pub const OEM_4: u16 = 0xDB; // [
    pub const OEM_5: u16 = 0xDC; // \
    pub const OEM_6: u16 = 0xDD; // ]
    pub const OEM_7: u16 = 0xDE; // '
}

/// Display name of a virtual key (also accepted by [`vk_from_name`]).
pub fn vk_name(code: u16) -> String {
    use vk::*;
    let s = match code {
        BACK => "Backspace",
        TAB => "Tab",
        RETURN => "Enter",
        ESCAPE => "Escape",
        SPACE => "Space",
        PRIOR => "PageUp",
        NEXT => "PageDown",
        END => "End",
        HOME => "Home",
        LEFT => "Left",
        UP => "Up",
        RIGHT => "Right",
        DOWN => "Down",
        DELETE => "Delete",
        OEM_1 => "Semicolon",
        OEM_PLUS => "Equal",
        OEM_COMMA => "Comma",
        OEM_MINUS => "Minus",
        OEM_PERIOD => "Period",
        OEM_2 => "Slash",
        OEM_3 => "Grave",
        OEM_4 => "LeftBracket",
        OEM_5 => "Backslash",
        OEM_6 => "RightBracket",
        OEM_7 => "Quote",
        KEY_0..=KEY_9 | KEY_A..=KEY_Z => return (code as u8 as char).to_string(),
        NUMPAD0..=NUMPAD9 => return format!("Numpad{}", code - NUMPAD0),
        F1..=F12 => return format!("F{}", code - F1 + 1),
        _ => return format!("VK{code:#04X}"),
    };
    s.to_string()
}

/// Inverse of [`vk_name`] (case-insensitive; also accepts the characters
/// themselves, e.g. "`", "[", "]").
pub fn vk_from_name(name: &str) -> Option<u16> {
    use vk::*;
    let n = name.trim();
    if n.chars().count() == 1 {
        let c = n.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u16);
        }
        return Some(match c {
            '`' | '~' => OEM_3,
            '[' => OEM_4,
            ']' => OEM_6,
            '\\' => OEM_5,
            ';' => OEM_1,
            '\'' => OEM_7,
            ',' => OEM_COMMA,
            '.' => OEM_PERIOD,
            '/' => OEM_2,
            '-' => OEM_MINUS,
            '=' => OEM_PLUS,
            _ => return None,
        });
    }
    let l = n.to_ascii_lowercase();
    let v = match l.as_str() {
        "backspace" => BACK,
        "tab" => TAB,
        "enter" | "return" => RETURN,
        "escape" | "esc" => ESCAPE,
        "space" => SPACE,
        "pageup" | "pgup" | "prior" => PRIOR,
        "pagedown" | "pgdn" | "next" => NEXT,
        "end" => END,
        "home" => HOME,
        "left" => LEFT,
        "up" => UP,
        "right" => RIGHT,
        "down" => DOWN,
        "delete" | "del" => DELETE,
        "semicolon" => OEM_1,
        "equal" | "plus" => OEM_PLUS,
        "comma" => OEM_COMMA,
        "minus" => OEM_MINUS,
        "period" => OEM_PERIOD,
        "slash" => OEM_2,
        "grave" | "backtick" | "tilde" => OEM_3,
        "leftbracket" => OEM_4,
        "backslash" => OEM_5,
        "rightbracket" => OEM_6,
        "quote" => OEM_7,
        _ => {
            if let Some(d) = l.strip_prefix("numpad") {
                let d: u16 = d.parse().ok()?;
                return (d <= 9).then_some(NUMPAD0 + d);
            }
            if let Some(f) = l.strip_prefix('f') {
                let f: u16 = f.parse().ok()?;
                return (1..=12).contains(&f).then_some(F1 + f - 1);
            }
            if let Some(h) = l.strip_prefix("vk0x") {
                return u16::from_str_radix(h, 16).ok();
            }
            return None;
        }
    };
    Some(v)
}

/// Windows virtual key -> macOS `kVK_*` key code (ANSI layout positions).
pub fn vk_to_mac_keycode(code: u16) -> Option<u16> {
    use vk::*;
    const LETTERS: [u16; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F, 0x23, 0x0C, 0x0F,
        0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
    const KEYPAD: [u16; 10] = [0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C];
    const FKEYS: [u16; 12] = [0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F];
    Some(match code {
        KEY_A..=KEY_Z => LETTERS[(code - KEY_A) as usize],
        KEY_0..=KEY_9 => DIGITS[(code - KEY_0) as usize],
        NUMPAD0..=NUMPAD9 => KEYPAD[(code - NUMPAD0) as usize],
        F1..=F12 => FKEYS[(code - F1) as usize],
        BACK => 0x33,
        TAB => 0x30,
        RETURN => 0x24,
        ESCAPE => 0x35,
        SPACE => 0x31,
        PRIOR => 0x74,
        NEXT => 0x79,
        END => 0x77,
        HOME => 0x73,
        LEFT => 0x7B,
        RIGHT => 0x7C,
        DOWN => 0x7D,
        UP => 0x7E,
        DELETE => 0x75,
        OEM_1 => 0x29,
        OEM_PLUS => 0x18,
        OEM_COMMA => 0x2B,
        OEM_MINUS => 0x1B,
        OEM_PERIOD => 0x2F,
        OEM_2 => 0x2C,
        OEM_3 => 0x32,
        OEM_4 => 0x21,
        OEM_5 => 0x2A,
        OEM_6 => 0x1E,
        OEM_7 => 0x27,
        _ => return None,
    })
}

/// macOS key code of a digit key (top row or keypad) -> digit.
pub fn mac_keycode_digit(code: u16) -> Option<u8> {
    (0..10u16).find(|&d| vk_to_mac_keycode(vk::KEY_0 + d) == Some(code) || vk_to_mac_keycode(vk::NUMPAD0 + d) == Some(code)).map(|d| d as u8)
}

/// Windows virtual key of a digit key (top row or numpad) -> digit.
pub fn vk_digit(code: u16) -> Option<u8> {
    match code {
        vk::KEY_0..=vk::KEY_9 => Some((code - vk::KEY_0) as u8),
        vk::NUMPAD0..=vk::NUMPAD9 => Some((code - vk::NUMPAD0) as u8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for code in [vk::OEM_3, vk::NEXT, vk::PRIOR, vk::KEY_X, vk::KEY_0, vk::F1, vk::F12, vk::NUMPAD0 + 7, vk::OEM_4, 0xE9] {
            assert_eq!(vk_from_name(&vk_name(code)), Some(code), "{}", vk_name(code));
        }
        assert_eq!(vk_from_name("`"), Some(vk::OEM_3));
        assert_eq!(vk_from_name("x"), Some(vk::KEY_X));
        assert_eq!(vk_from_name("pgdn"), Some(vk::NEXT));
        assert_eq!(vk_from_name("nope"), None);
    }

    #[test]
    fn mac_mapping() {
        assert_eq!(vk_to_mac_keycode(vk::KEY_X), Some(0x07));
        assert_eq!(vk_to_mac_keycode(vk::OEM_3), Some(0x32));
        assert_eq!(vk_to_mac_keycode(vk::OEM_6), Some(0x1E));
        assert_eq!(vk_to_mac_keycode(vk::NEXT), Some(0x79));
        assert_eq!(mac_keycode_digit(0x1D), Some(0));
        assert_eq!(mac_keycode_digit(0x19), Some(9));
        assert_eq!(mac_keycode_digit(0x5C), Some(9));
        assert_eq!(mac_keycode_digit(0x00), None);
        assert_eq!(vk_digit(0x35), Some(5));
        assert_eq!(vk_digit(vk::NUMPAD9), Some(9));
    }
}
