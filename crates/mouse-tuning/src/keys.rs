//! Key combinations for keystroke actions, e.g. `"Cmd+["`, `"Ctrl+Shift+T"`,
//! `"Alt+Left"`, `"F11"`. Keys are Windows virtual-key codes (pm-input's
//! convention on every platform); `Cmd`/`Win`/`Super`/`Meta` are the same key
//! (Command on macOS, the Windows key on Windows).

use pm_input::vk::*;

/// A parsed combination: modifiers (pressed in order) then one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Combo {
    pub modifiers: Vec<u32>,
    pub key: u32,
}

fn modifier(name: &str) -> Option<u32> {
    Some(match name {
        "cmd" | "command" | "win" | "windows" | "super" | "meta" | "⌘" => VK_LWIN,
        "ctrl" | "control" | "⌃" => VK_LCONTROL,
        "alt" | "option" | "opt" | "⌥" => VK_LMENU,
        "shift" | "⇧" => VK_LSHIFT,
        _ => return None,
    })
}

fn key(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    if name.chars().count() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
        return Some(match c {
            '[' => VK_OEM_4,
            ']' => VK_OEM_6,
            ';' => VK_OEM_1,
            '\'' => VK_OEM_7,
            ',' => VK_OEM_COMMA,
            '.' => VK_OEM_PERIOD,
            '/' => VK_OEM_2,
            '\\' => VK_OEM_5,
            '`' => VK_OEM_3,
            '-' => VK_OEM_MINUS,
            '=' => VK_OEM_PLUS,
            ' ' => VK_SPACE,
            _ => return None,
        });
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(VK_F1 + n - 1);
        }
    }
    Some(match lower.as_str() {
        "space" | "spacebar" => VK_SPACE,
        "tab" => VK_TAB,
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "backspace" => VK_BACK,
        "delete" | "del" | "forwarddelete" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "left" | "leftarrow" | "arrowleft" => VK_LEFT,
        "right" | "rightarrow" | "arrowright" => VK_RIGHT,
        "up" | "uparrow" | "arrowup" => VK_UP,
        "down" | "downarrow" | "arrowdown" => VK_DOWN,
        "leftbracket" | "bracketleft" => VK_OEM_4,
        "rightbracket" | "bracketright" => VK_OEM_6,
        "semicolon" => VK_OEM_1,
        "quote" | "apostrophe" => VK_OEM_7,
        "comma" => VK_OEM_COMMA,
        "period" | "dot" => VK_OEM_PERIOD,
        "slash" => VK_OEM_2,
        "backslash" => VK_OEM_5,
        "grave" | "backtick" | "backquote" => VK_OEM_3,
        "minus" | "dash" => VK_OEM_MINUS,
        "equal" | "equals" | "plus" => VK_OEM_PLUS,
        "volumeup" => VK_VOLUME_UP,
        "volumedown" => VK_VOLUME_DOWN,
        "mute" | "volumemute" => VK_VOLUME_MUTE,
        "playpause" | "mediaplaypause" => VK_MEDIA_PLAY_PAUSE,
        "nexttrack" | "medianexttrack" => VK_MEDIA_NEXT_TRACK,
        "prevtrack" | "previoustrack" | "mediaprevtrack" => VK_MEDIA_PREV_TRACK,
        "browserback" => VK_BROWSER_BACK,
        "browserforward" => VK_BROWSER_FORWARD,
        _ => return None,
    })
}

/// Parse `"Mod+Mod+Key"` (case-insensitive; `"Cmd++"` is Cmd + `=`).
pub fn parse_combo(s: &str) -> Result<Combo, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty key combination".into());
    }
    let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if s.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("=");
    } else if s == "+" {
        parts = vec!["="];
    }
    let (last, mods) = parts.split_last().ok_or("empty key combination")?;
    let mut modifiers = Vec::new();
    for m in mods {
        let vk = modifier(&m.to_ascii_lowercase()).ok_or_else(|| format!("unknown modifier '{m}' in '{s}'"))?;
        if !modifiers.contains(&vk) {
            modifiers.push(vk);
        }
    }
    let key = key(last).ok_or_else(|| format!("unknown key '{last}' in '{s}'"))?;
    Ok(Combo { modifiers, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combos() {
        assert_eq!(parse_combo("Cmd+[").unwrap(), Combo { modifiers: vec![VK_LWIN], key: VK_OEM_4 });
        assert_eq!(
            parse_combo("ctrl + shift + t").unwrap(),
            Combo { modifiers: vec![VK_LCONTROL, VK_LSHIFT], key: b'T' as u32 }
        );
        assert_eq!(parse_combo("Option+Left").unwrap().modifiers, vec![VK_LMENU]);
        assert_eq!(parse_combo("F11").unwrap(), Combo { modifiers: vec![], key: VK_F11 });
        assert_eq!(parse_combo("Cmd++").unwrap(), Combo { modifiers: vec![VK_LWIN], key: VK_OEM_PLUS });
        assert_eq!(parse_combo("Win+D").unwrap().key, b'D' as u32);
        assert_eq!(parse_combo("Cmd+Shift+Cmd+1").unwrap().modifiers, vec![VK_LWIN, VK_LSHIFT]);
        assert!(parse_combo("").is_err());
        assert!(parse_combo("Hyper+X").is_err());
        assert!(parse_combo("Cmd+Nope").is_err());
    }
}
