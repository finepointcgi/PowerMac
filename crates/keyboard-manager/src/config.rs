//! Configuration (serde, camelCase, every field optional) and the PowerToys
//! `default.json` importer. See the crate docs for the JSON format.

use serde::{Deserialize, Serialize};

use crate::keys::{key_name, parse_combo, Key, Shortcut, VK_DISABLED};

/// Top-level Keyboard Manager configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch for all remaps (paste-as-plain-text has its own switch).
    pub enabled: bool,
    /// Single key remaps: key -> key / shortcut / disabled / text / ...
    pub remap_keys: Vec<KeyRemap>,
    /// Shortcut remaps, optionally scoped to one application.
    pub remap_shortcuts: Vec<ShortcutRemap>,
    /// "Paste as plain text" (from PowerToys Advanced Paste).
    pub paste_plain_text: PastePlainText,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            remap_keys: Vec::new(),
            remap_shortcuts: Vec::new(),
            paste_plain_text: PastePlainText::default(),
        }
    }
}

/// `{"from": "CapsLock", "to": "Esc"}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRemap {
    pub from: Key,
    pub to: Target,
}

/// `{"from": "Ctrl+Left", "to": "Option+Left", "app": "com.apple.Terminal"}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutRemap {
    #[serde(with = "shortcut_serde")]
    pub from: Shortcut,
    pub to: Target,
    /// Only active while this app is in front: a macOS bundle id
    /// (`com.google.Chrome`) or app name, or a Windows process name
    /// (`chrome.exe`). Case-insensitive; `.exe` is optional.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "targetApp")]
    pub app: Option<String>,
}

/// "Paste as plain text" settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PastePlainText {
    pub enabled: bool,
    /// Defaults to Cmd+Option+Shift+V on macOS, Win+Ctrl+Alt+V on Windows.
    #[serde(with = "shortcut_serde")]
    pub hotkey: Shortcut,
    /// Put the original (rich) clipboard contents back after pasting.
    pub restore_clipboard: bool,
    /// How long to wait after the paste before restoring, in milliseconds.
    pub restore_delay_ms: u64,
}

impl Default for PastePlainText {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: default_paste_hotkey(),
            restore_clipboard: false,
            restore_delay_ms: 500,
        }
    }
}

/// Cmd+Option+Shift+V on macOS, Win+Ctrl+Alt+V elsewhere (PowerToys' default).
pub fn default_paste_hotkey() -> Shortcut {
    let s = if cfg!(target_os = "macos") {
        "Cmd+Option+Shift+V"
    } else {
        "Win+Ctrl+Alt+V"
    };
    s.parse().expect("valid default hotkey")
}

/// A program to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgram {
    /// Executable path. On macOS a `.app` bundle is opened with `open -a`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

/// What a key or shortcut is remapped to.
///
/// JSON forms:
/// - `"Esc"`, `27`, `"0x1B"` - a single key (may be a modifier, e.g. `"LCtrl"`)
/// - `"Cmd+C"`, `["Ctrl", "C"]` - a shortcut
/// - `"disabled"` or `null` - do nothing
/// - `{"text": "hello"}` - type a string
/// - `{"run": {"path": "/Applications/Safari.app", "args": []}}` - start a program
/// - `{"openUri": "https://example.com"}` - open a URL / file / folder
/// - `"pastePlainText"` - paste the clipboard as plain text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TargetRepr", into = "TargetRepr")]
pub enum Target {
    Disabled,
    Key(u32),
    Shortcut(Shortcut),
    Text(String),
    Run(RunProgram),
    OpenUri(String),
    PastePlainText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum TargetRepr {
    Null(()),
    Num(u32),
    Str(String),
    Keys(Vec<Key>),
    Obj(TargetObj),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetObj {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<RunProgram>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    open_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keys: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disabled: Option<bool>,
}

impl Target {
    /// Parse `"Esc"` / `"Ctrl+C"` / `"disabled"` / `"pastePlainText"`.
    pub fn parse(s: &str) -> Result<Target, String> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("disabled")
            || t.eq_ignore_ascii_case("disable")
            || t.eq_ignore_ascii_case("none")
        {
            return Ok(Target::Disabled);
        }
        if t.eq_ignore_ascii_case("pastePlainText") || t.eq_ignore_ascii_case("paste-plain-text") {
            return Ok(Target::PastePlainText);
        }
        Target::from_keys(&parse_combo(t)?)
    }

    fn from_keys(keys: &[u32]) -> Result<Target, String> {
        match keys {
            [] => Err("empty key list".into()),
            [k] if *k == VK_DISABLED => Ok(Target::Disabled),
            [k] => Ok(Target::Key(*k)),
            ks => Shortcut::from_keys(ks).map(Target::Shortcut),
        }
    }
}

impl TryFrom<TargetRepr> for Target {
    type Error = String;
    fn try_from(r: TargetRepr) -> Result<Self, String> {
        match r {
            TargetRepr::Null(()) => Ok(Target::Disabled),
            TargetRepr::Num(n) => Target::from_keys(&[n]),
            TargetRepr::Str(s) => Target::parse(&s),
            TargetRepr::Keys(ks) => Target::from_keys(&ks.iter().map(|k| k.0).collect::<Vec<_>>()),
            TargetRepr::Obj(o) => {
                let n = o.text.is_some() as u8
                    + o.run.is_some() as u8
                    + o.open_uri.is_some() as u8
                    + o.keys.is_some() as u8
                    + o.disabled.is_some() as u8;
                if n != 1 {
                    return Err(
                        "target object needs exactly one of text/run/openUri/keys/disabled".into(),
                    );
                }
                if let Some(t) = o.text {
                    Ok(Target::Text(t))
                } else if let Some(r) = o.run {
                    Ok(Target::Run(r))
                } else if let Some(u) = o.open_uri {
                    Ok(Target::OpenUri(u))
                } else if let Some(k) = o.keys {
                    Target::parse(&k)
                } else {
                    Ok(Target::Disabled)
                }
            }
        }
    }
}

impl From<Target> for TargetRepr {
    fn from(t: Target) -> Self {
        match t {
            Target::Disabled => TargetRepr::Str("disabled".into()),
            Target::Key(k) => TargetRepr::Str(key_name(k)),
            Target::Shortcut(s) => TargetRepr::Str(s.to_string()),
            Target::Text(text) => TargetRepr::Obj(TargetObj {
                text: Some(text),
                ..Default::default()
            }),
            Target::Run(r) => TargetRepr::Obj(TargetObj {
                run: Some(r),
                ..Default::default()
            }),
            Target::OpenUri(u) => TargetRepr::Obj(TargetObj {
                open_uri: Some(u),
                ..Default::default()
            }),
            Target::PastePlainText => TargetRepr::Str("pastePlainText".into()),
        }
    }
}

/// Shortcuts serialize as `"Ctrl+Shift+V"` and also accept a key array.
pub(crate) mod shortcut_serde {
    use super::*;

    pub fn serialize<S: serde::Serializer>(s: &Shortcut, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&s.to_string())
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Shortcut, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Keys(Vec<Key>),
        }
        let r = match Repr::deserialize(d)? {
            Repr::Str(s) => s.parse::<Shortcut>(),
            Repr::Keys(ks) => Shortcut::from_keys(&ks.iter().map(|k| k.0).collect::<Vec<_>>()),
        };
        r.map_err(serde::de::Error::custom)
    }
}

impl Config {
    /// Parse a config from JSON text.
    pub fn from_json(s: &str) -> anyhow::Result<Config> {
        Ok(serde_json::from_str(s)?)
    }

    /// Serialize as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    /// Problems that would make a remap be ignored at runtime.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, r) in self.remap_keys.iter().enumerate() {
            if r.from.0 == 0 || r.from.0 > 0xFF && r.from.0 != crate::keys::VK_WIN_BOTH {
                out.push(format!(
                    "remapKeys[{i}]: invalid source key {:#x}",
                    r.from.0
                ));
            }
            if let Target::Key(k) = r.to {
                if k == r.from.0 {
                    out.push(format!(
                        "remapKeys[{i}]: {} is remapped to itself",
                        key_name(k)
                    ));
                }
            }
        }
        for (i, a) in self.remap_keys.iter().enumerate() {
            for b in &self.remap_keys[i + 1..] {
                if overlaps(a.from.0, b.from.0) {
                    out.push(format!(
                        "remapKeys: {} is remapped more than once (the first one wins)",
                        key_name(a.from.0)
                    ));
                }
            }
        }
        for (i, r) in self.remap_shortcuts.iter().enumerate() {
            if let Target::Key(k) = r.to {
                if k == crate::keys::VK_DUMMY {
                    out.push(format!("remapShortcuts[{i}]: target key is invalid"));
                }
            }
            if r.app.as_deref().is_some_and(|a| a.trim().is_empty()) {
                out.push(format!("remapShortcuts[{i}]: empty app name"));
            }
        }
        out
    }

    /// Import a PowerToys Keyboard Manager config (`%LOCALAPPDATA%\Microsoft\
    /// PowerToys\Keyboard Manager\default.json`). Returns the config plus
    /// warnings for entries that could not be imported (chords, "alone"
    /// remaps, malformed entries). The paste-as-plain-text section keeps its
    /// default.
    pub fn from_powertoys_json(json: &str) -> anyhow::Result<(Config, Vec<String>)> {
        crate::powertoys::import(json)
    }
}

fn overlaps(a: u32, b: u32) -> bool {
    use crate::keys::{mod_family, mod_side, Side};
    if a == b {
        return true;
    }
    match (mod_family(a), mod_family(b)) {
        (Some(fa), Some(fb)) if fa == fb => {
            mod_side(a) == Side::Either || mod_side(b) == Side::Either
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_input::vk::*;

    #[test]
    fn default_config() {
        let c = Config::default();
        assert!(c.enabled);
        assert!(c.remap_keys.is_empty() && c.remap_shortcuts.is_empty());
        assert!(c.paste_plain_text.enabled);
        assert_eq!(c.paste_plain_text.hotkey.key, VK_V);
        // empty object == default
        assert_eq!(Config::from_json("{}").unwrap(), c);
        // round trip
        assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
    }

    #[test]
    fn parse_hand_written() {
        let json = r#"{
          "remapKeys": [
            {"from": "CapsLock", "to": "Esc"},
            {"from": "LOption", "to": "LCmd"},
            {"from": 44, "to": "Ctrl+Alt+Delete"},
            {"from": "Insert", "to": "disabled"},
            {"from": "F13", "to": {"text": "¯\\_(ツ)_/¯"}},
            {"from": "F14", "to": null}
          ],
          "remapShortcuts": [
            {"from": "Ctrl+Left", "to": "Option+Left"},
            {"from": ["Ctrl", "Q"], "to": "disabled", "app": "com.apple.Terminal"},
            {"from": "Ctrl+Alt+T", "to": {"run": {"path": "/System/Applications/Utilities/Terminal.app"}}},
            {"from": "Ctrl+Alt+G", "to": {"openUri": "https://github.com"}},
            {"from": "Ctrl+Shift+D", "to": {"text": "2026-01-01"}},
            {"from": "Ctrl+Shift+P", "to": "pastePlainText"},
            {"from": "Ctrl+Shift+K", "to": {"keys": "Cmd+K"}}
          ],
          "pastePlainText": {"hotkey": "Ctrl+Shift+V", "restoreClipboard": true}
        }"#
        .to_string();
        let c = Config::from_json(&json).unwrap();
        assert_eq!(
            c.remap_keys[0],
            KeyRemap {
                from: Key(VK_CAPITAL),
                to: Target::Key(VK_ESCAPE)
            }
        );
        assert_eq!(c.remap_keys[1].to, Target::Key(VK_LWIN));
        assert!(matches!(c.remap_keys[2].to, Target::Shortcut(s) if s.key == VK_DELETE));
        assert_eq!(c.remap_keys[3].to, Target::Disabled);
        assert_eq!(c.remap_keys[4].to, Target::Text("¯\\_(ツ)_/¯".into()));
        assert_eq!(c.remap_keys[5].to, Target::Disabled);
        assert_eq!(
            c.remap_shortcuts[1].app.as_deref(),
            Some("com.apple.Terminal")
        );
        assert!(matches!(c.remap_shortcuts[2].to, Target::Run(_)));
        assert_eq!(
            c.remap_shortcuts[3].to,
            Target::OpenUri("https://github.com".into())
        );
        assert_eq!(c.remap_shortcuts[5].to, Target::PastePlainText);
        assert!(matches!(c.remap_shortcuts[6].to, Target::Shortcut(_)));
        assert!(c.paste_plain_text.enabled);
        assert!(c.paste_plain_text.restore_clipboard);
        assert_eq!(c.paste_plain_text.restore_delay_ms, 500);
        assert!(c.validate().is_empty(), "{:?}", c.validate());
        // round trip
        assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
    }

    #[test]
    fn bad_entries_are_errors() {
        assert!(Config::from_json(r#"{"remapKeys":[{"from":"Nope","to":"Esc"}]}"#).is_err());
        assert!(Config::from_json(r#"{"remapShortcuts":[{"from":"Ctrl","to":"Esc"}]}"#).is_err());
        assert!(Config::from_json(
            r#"{"remapShortcuts":[{"from":"Ctrl+A","to":{"text":"a","openUri":"b"}}]}"#
        )
        .is_err());
    }

    #[test]
    fn validate_reports_duplicates() {
        let c = Config::from_json(
            r#"{"remapKeys":[{"from":"Ctrl","to":"A"},{"from":"LCtrl","to":"B"},{"from":"A","to":"A"}]}"#,
        )
        .unwrap();
        let w = c.validate();
        assert_eq!(w.len(), 2, "{w:?}");
    }
}
