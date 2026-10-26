//! Shortcut clashes with the AltTab app (`com.lwouis.alt-tab-macos`), which
//! users often run next to (or instead of) this switcher.
//!
//! AltTab stores each shortcut as a "hold" part (modifier symbols such as
//! `⌘` or `⌥⇧`) and a key part (`⇥`, `` ` ``, a letter) in its defaults:
//! `holdShortcut` + `nextWindowShortcut` (defaults `⌥` + `⇥`), and
//! `holdShortcut2` + `nextWindowShortcut2` for the second shortcut.

use pm_input::vk;

use crate::config::{Config, Hotkey};

/// AltTab's bundle id.
pub const ALT_TAB_BUNDLE_ID: &str = "com.lwouis.alt-tab-macos";

/// AltTab's built-in defaults for shortcut 1.
pub const ALT_TAB_DEFAULT_HOLD: &str = "⌥";
pub const ALT_TAB_DEFAULT_KEY: &str = "⇥";

/// Turn AltTab's `hold` + `key` strings into a [`Hotkey`].
pub fn parse_alt_tab(hold: &str, key: &str) -> Option<Hotkey> {
    let mut h = Hotkey::none();
    for c in hold.chars() {
        match c {
            '⌘' => h.win = true,
            '⌥' => h.alt = true,
            '⌃' => h.ctrl = true,
            '⇧' => h.shift = true,
            c if c.is_whitespace() => {}
            _ => return None,
        }
    }
    // The key part may repeat modifiers ("⌥⇥"); only the last char is the key.
    let k = key.trim();
    let last = if k.eq_ignore_ascii_case("space") { ' ' } else { k.chars().last()? };
    for c in k.chars().take(k.chars().count().saturating_sub(1)) {
        match c {
            '⌘' => h.win = true,
            '⌥' => h.alt = true,
            '⌃' => h.ctrl = true,
            '⇧' => h.shift = true,
            _ => {}
        }
    }
    h.code = match last {
        '⇥' => vk::VK_TAB,
        '`' | '§' => vk::VK_OEM_3,
        ' ' | '␣' => vk::VK_SPACE,
        c if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u32,
        _ => return None,
    };
    (h.win || h.alt || h.ctrl).then_some(h)
}

/// The shortcuts this config reacts to (to compare with other apps').
pub fn active_hotkeys(cfg: &Config) -> Vec<Hotkey> {
    if !cfg.enabled {
        return vec![];
    }
    let mut v = vec![cfg.trigger, cfg.app_windows_trigger];
    if cfg.replaces_system_switcher() {
        v.push(Hotkey::system_switcher());
    }
    if cfg.replaces_cmd_backtick() {
        v.push(Hotkey::system_app_switcher());
    }
    v.retain(|h| h.is_set());
    v
}

/// Whether `other` (e.g. AltTab's shortcut) is one of ours. Shift is
/// ignored: both apps use it to go backwards.
pub fn clashes(cfg: &Config, other: &Hotkey) -> bool {
    active_hotkeys(cfg)
        .iter()
        .any(|h| h.code == other.code && h.win == other.win && h.alt == other.alt && h.ctrl == other.ctrl)
}

/// AltTab's shortcuts as configured on this Mac (its defaults when unset).
/// Empty on other platforms.
pub fn alt_tab_hotkeys() -> Vec<Hotkey> {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::apps::alt_tab_hotkeys()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alt_tab_shortcuts() {
        assert_eq!(parse_alt_tab("⌥", "⇥"), Some(Hotkey::new(false, false, true, false, vk::VK_TAB)));
        assert_eq!(parse_alt_tab("⌘", "⇥"), Some(Hotkey::system_switcher()));
        assert_eq!(parse_alt_tab("⌥", "`"), Some(Hotkey::new(false, false, true, false, vk::VK_OEM_3)));
        assert_eq!(parse_alt_tab("⌃⌥", "⇧⇥"), Some(Hotkey::new(false, true, true, true, vk::VK_TAB)));
        assert_eq!(parse_alt_tab("⌘", "q"), Some(Hotkey::new(true, false, false, false, 0x51)));
        assert_eq!(parse_alt_tab("", "⇥"), None, "needs a held modifier");
        assert_eq!(parse_alt_tab("x", "⇥"), None);
        assert_eq!(parse_alt_tab("⌥", ""), None);
    }

    #[test]
    fn clash_detection_follows_the_config() {
        let alt_tab_cmd = parse_alt_tab("⌘", "⇥").unwrap();
        let alt_tab_opt = parse_alt_tab("⌥", "⇥").unwrap();
        let mut c = Config::default();
        // Default: Option+Tab clashes with AltTab's default, Cmd+Tab doesn't.
        assert!(clashes(&c, &alt_tab_opt));
        assert!(!clashes(&c, &alt_tab_cmd));
        c.replace_cmd_tab = true;
        assert!(clashes(&c, &alt_tab_cmd));
        // Shift doesn't matter.
        let mut shifted = alt_tab_cmd;
        shifted.shift = true;
        assert!(clashes(&c, &shifted));
        c.trigger = Hotkey::none();
        c.app_windows_trigger = Hotkey::none();
        assert!(!clashes(&c, &alt_tab_opt));
        c.enabled = false;
        assert!(!clashes(&c, &alt_tab_cmd));
        assert!(active_hotkeys(&c).is_empty());
    }
}
