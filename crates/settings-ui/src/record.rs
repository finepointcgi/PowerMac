//! "Record shortcut": turn the next key press in the window into a [`Combo`].

use eframe::egui::{self, Key, Modifiers};

use crate::model::Combo;

/// Windows VK code for an egui key (US layout positions).
pub fn vk_for_key(k: Key) -> Option<u32> {
    use Key::*;
    let vk = match k {
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        Escape => 0x1B,
        Tab => 0x09,
        Backspace => 0x08,
        Enter => 0x0D,
        Space => 0x20,
        Insert => 0x2D,
        Delete => 0x2E,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        Comma => 0xBC,
        Minus => 0xBD,
        Period => 0xBE,
        Plus | Equals => 0xBB,
        Semicolon | Colon => 0xBA,
        Slash | Questionmark => 0xBF,
        Backtick => 0xC0,
        OpenBracket | OpenCurlyBracket => 0xDB,
        Backslash | Pipe => 0xDC,
        CloseBracket | CloseCurlyBracket => 0xDD,
        Quote => 0xDE,
        Num0 => 0x30,
        Num1 => 0x31,
        Num2 => 0x32,
        Num3 => 0x33,
        Num4 => 0x34,
        Num5 => 0x35,
        Num6 => 0x36,
        Num7 => 0x37,
        Num8 => 0x38,
        Num9 => 0x39,
        A => 0x41,
        B => 0x42,
        C => 0x43,
        D => 0x44,
        E => 0x45,
        F => 0x46,
        G => 0x47,
        H => 0x48,
        I => 0x49,
        J => 0x4A,
        K => 0x4B,
        L => 0x4C,
        M => 0x4D,
        N => 0x4E,
        O => 0x4F,
        P => 0x50,
        Q => 0x51,
        R => 0x52,
        S => 0x53,
        T => 0x54,
        U => 0x55,
        V => 0x56,
        W => 0x57,
        X => 0x58,
        Y => 0x59,
        Z => 0x5A,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7A,
        F12 => 0x7B,
        F13 => 0x7C,
        F14 => 0x7D,
        F15 => 0x7E,
        F16 => 0x7F,
        F17 => 0x80,
        F18 => 0x81,
        F19 => 0x82,
        F20 => 0x83,
        _ => return None,
    };
    Some(vk)
}

/// The combo for a key press. egui reports Cmd as `mac_cmd` on macOS; the
/// Windows key isn't reported, so Win shortcuts must be typed.
pub fn combo(k: Key, m: Modifiers) -> Option<Combo> {
    Some(Combo { win: m.mac_cmd, ctrl: m.ctrl, alt: m.alt, shift: m.shift, vk: vk_for_key(k)? })
}

/// What the recorder saw this frame.
pub enum Recorded {
    Combo(Combo),
    Cancel,
}

/// Consume this frame's key presses. Esc alone cancels.
pub fn take(ctx: &egui::Context) -> Option<Recorded> {
    ctx.input_mut(|i| {
        let mods = i.modifiers;
        let mut out = None;
        for e in &i.events {
            let r = match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if *key == Key::Escape && modifiers.is_none() {
                        Some(Recorded::Cancel)
                    } else {
                        combo(*key, *modifiers).map(Recorded::Combo)
                    }
                }
                // Cmd/Ctrl+C/X/V arrive as clipboard events instead of key presses.
                egui::Event::Copy => combo(Key::C, mods).map(Recorded::Combo),
                egui::Event::Cut => combo(Key::X, mods).map(Recorded::Combo),
                egui::Event::Paste(_) => combo(Key::V, mods).map(Recorded::Combo),
                _ => None,
            };
            if r.is_some() {
                out = r;
                break;
            }
        }
        if out.is_some() {
            i.events.clear();
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_vk() {
        assert_eq!(vk_for_key(Key::A), Some(0x41));
        assert_eq!(vk_for_key(Key::Num7), Some(0x37));
        assert_eq!(vk_for_key(Key::F13), Some(0x7C));
        assert_eq!(vk_for_key(Key::ArrowLeft), Some(0x25));
        let c = combo(Key::H, Modifiers { shift: true, mac_cmd: true, command: true, ..Default::default() }).unwrap();
        assert!(c.win && c.shift && !c.ctrl && c.vk == 0x48);
        // every mapped key has a name the key parser reads back
        for k in Key::ALL {
            if let Some(vk) = vk_for_key(*k) {
                let name = keyboard_manager::keys::key_name(vk);
                assert_eq!(keyboard_manager::keys::parse_key(&name), Some(vk), "{k:?}");
            }
        }
    }
}
