//! Configuration (serde camelCase; every field optional).

use serde::{Deserialize, Serialize};

/// Text expander settings, stored in the app config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub snippets: Vec<Snippet>,
    /// Backspace right after an expansion puts the typed trigger back.
    pub undo_with_backspace: bool,
    /// Replacements longer than this many characters (or containing
    /// `{clipboard}` text over it) are pasted through the clipboard
    /// (saved and restored) instead of typed.
    pub paste_threshold: usize,
    /// How long after pasting the previous clipboard is put back, in ms.
    pub clipboard_restore_delay_ms: u64,
    /// Apps where nothing expands (bundle ids / app names on macOS, exe
    /// names on Windows; case-insensitive). Password managers by default.
    pub excluded_apps: Vec<String>,
    /// Also skip terminals (Terminal, iTerm, Warp, Windows Terminal, ...).
    pub exclude_terminals: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            snippets: default_snippets(),
            undo_with_backspace: true,
            paste_threshold: 200,
            clipboard_restore_delay_ms: 400,
            excluded_apps: default_excluded_apps(),
            exclude_terminals: false,
        }
    }
}

/// One abbreviation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// What to type, e.g. `";addr"`.
    pub trigger: String,
    /// What it becomes. Variables: `{date}`, `{date:%d.%m.%Y}`, `{time}`,
    /// `{time:%H:%M:%S}`, `{clipboard}`, `{cursor}`, `{uuid}`,
    /// `{random:6}` (1..=6), `{random:10-99}`; `{{` and `}}` are literal
    /// braces.
    pub replacement: String,
    /// Match the trigger's case exactly (default). When false, any case
    /// matches and the case is carried over: `;Addr` capitalizes the first
    /// letter, `;ADDR` upper-cases the replacement.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub case_sensitive: bool,
    /// Expand only as a whole word: when a space, Return, Tab or
    /// punctuation follows the trigger (which is kept after the
    /// replacement), and only if the trigger starts a word.
    #[serde(default, skip_serializing_if = "is_false")]
    pub word_boundary: bool,
    /// Only in these apps (empty: everywhere).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    /// Always paste this one through the clipboard.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paste: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Free-form note shown in the settings UI.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}

impl Snippet {
    pub fn new(trigger: &str, replacement: &str) -> Snippet {
        Snippet {
            trigger: trigger.into(),
            replacement: replacement.into(),
            case_sensitive: true,
            word_boundary: false,
            apps: Vec::new(),
            paste: false,
            enabled: true,
            label: String::new(),
        }
    }
}

/// The examples a fresh config starts with.
pub fn default_snippets() -> Vec<Snippet> {
    vec![
        Snippet::new(";date", "{date:%Y-%m-%d}"),
        Snippet::new(";time", "{time:%H:%M}"),
        Snippet::new(";now", "{date:%Y-%m-%d} {time:%H:%M}"),
        Snippet::new(";shrug", "¯\\_(ツ)_/¯"),
        Snippet::new(";uuid", "{uuid}"),
    ]
}

/// Password managers and credential prompts.
pub fn default_excluded_apps() -> Vec<String> {
    [
        "com.1password.1password",
        "com.agilebits.onepassword7",
        "com.bitwarden.desktop",
        "com.lastpass.LastPass",
        "org.keepassxc.keepassxc",
        "com.dashlane.dashlanephonefinal",
        "com.apple.keychainaccess",
        "com.apple.Passwords",
        "1Password.exe",
        "Bitwarden.exe",
        "KeePass.exe",
        "KeePassXC.exe",
        "CredentialUIBroker.exe",
        "consent.exe",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Terminals, skipped when `excludeTerminals` is on.
pub const TERMINALS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "io.alacritty",
    "com.mitchellh.ghostty",
    "co.zeit.hyper",
    "WindowsTerminal.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "conhost.exe",
    "mintty.exe",
    "alacritty.exe",
    "wezterm-gui.exe",
];

impl Config {
    pub fn from_json(s: &str) -> anyhow::Result<Config> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    /// Problems worth showing in the settings UI.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, s) in self.snippets.iter().enumerate() {
            if s.trigger.trim().is_empty() {
                out.push(format!("snippet {}: empty trigger", i + 1));
            } else if s.trigger.chars().count() > crate::engine::MAX_TRIGGER {
                out.push(format!(
                    "snippet {} ({}): trigger longer than {} characters",
                    i + 1,
                    s.trigger,
                    crate::engine::MAX_TRIGGER
                ));
            }
            if s.trigger.chars().any(|c| c.is_control()) {
                out.push(format!(
                    "snippet {} ({}): trigger contains control characters",
                    i + 1,
                    s.trigger
                ));
            }
            for (j, t) in self.snippets.iter().enumerate().skip(i + 1) {
                if s.enabled
                    && t.enabled
                    && s.trigger == t.trigger
                    && s.apps == t.apps
                    && !s.trigger.is_empty()
                {
                    out.push(format!(
                        "snippets {} and {}: same trigger {:?} (the first wins)",
                        i + 1,
                        j + 1,
                        s.trigger
                    ));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let c = Config::default();
        assert!(!c.enabled);
        assert!(c.snippets.iter().any(|s| s.trigger == ";date"));
        assert!(c.snippets.iter().any(|s| s.replacement == "¯\\_(ツ)_/¯"));
        assert_eq!(Config::from_json("{}").unwrap(), c);
        assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
        assert!(c.validate().is_empty(), "{:?}", c.validate());
        // compact snippet JSON
        let j = serde_json::to_string(&Snippet::new(";a", "b")).unwrap();
        assert_eq!(j, r#"{"trigger":";a","replacement":"b"}"#);
    }

    #[test]
    fn parse_hand_written() {
        let c = Config::from_json(
            r#"{"enabled": true, "snippets": [
                {"trigger": "addr", "replacement": "1 Infinite Loop", "wordBoundary": true,
                 "caseSensitive": false, "apps": ["com.apple.mail"]}
            ]}"#,
        )
        .unwrap();
        assert!(c.enabled);
        let s = &c.snippets[0];
        assert!(s.word_boundary && !s.case_sensitive && s.enabled);
        assert_eq!(s.apps, vec!["com.apple.mail"]);
        assert!(c.undo_with_backspace);
    }

    #[test]
    fn validate_flags_problems() {
        let mut c = Config {
            snippets: vec![
                Snippet::new("", "x"),
                Snippet::new(";a", "1"),
                Snippet::new(";a", "2"),
            ],
            ..Default::default()
        };
        assert_eq!(c.validate().len(), 2);
        c.snippets.clear();
        assert!(c.validate().is_empty());
    }
}
