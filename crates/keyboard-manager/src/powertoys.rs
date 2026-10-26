//! Importer for PowerToys Keyboard Manager's `default.json`.
//!
//! Shape (all keys are decimal VK codes in strings, shortcuts are `;`-joined):
//! ```json
//! { "remapKeys": { "inProcess": [ {"originalKeys": "20", "newRemapKeys": "27"} ] },
//!   "remapKeysToText": { "inProcess": [ {"originalKeys": "112", "unicodeText": "hi"} ] },
//!   "remapShortcuts": {
//!     "global": [ {"originalKeys": "17;67", "newRemapKeys": "91;67", "exactMatch": false,
//!                  "operationType": 0} ],
//!     "appSpecific": [ {"originalKeys": "17;87", "newRemapKeys": "256", "targetApp": "msedge.exe"} ] },
//!   "remapShortcutsToText": { "global": [ {"originalKeys": "17;18;68", "unicodeText": "x"} ],
//!                             "appSpecific": [] } }
//! ```
//! `operationType` 1 = run program (`runProgramFilePath`, `runProgramArgs`,
//! `runProgramStartInDir`), 2 = open URI (`openUri`).

use serde_json::Value;

use crate::config::{Config, KeyRemap, RunProgram, ShortcutRemap, Target};
use crate::keys::{key_name, Key, Shortcut, VK_DISABLED};

fn parse_vks(s: &str) -> Result<Vec<u32>, String> {
    s.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            p.trim()
                .parse::<u32>()
                .map_err(|_| format!("bad key code '{p}'"))
        })
        .collect()
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn entries<'a>(root: &'a Value, section: &str, list: &str) -> Vec<&'a Value> {
    root.get(section)
        .and_then(|s| s.get(list))
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// Split a Windows-style argument string, honouring double quotes.
pub(crate) fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

fn target_from_keys(s: &str) -> Result<Target, String> {
    let ks = parse_vks(s)?;
    match ks.as_slice() {
        [] => Err("empty target".into()),
        [k] if *k == VK_DISABLED => Ok(Target::Disabled),
        [k] => Ok(Target::Key(*k)),
        _ => Shortcut::from_keys(&ks).map(Target::Shortcut),
    }
}

fn shortcut_target(e: &Value) -> Result<Target, String> {
    match e.get("operationType").and_then(Value::as_u64).unwrap_or(0) {
        1 => {
            let path = str_field(e, "runProgramFilePath")
                .unwrap_or_default()
                .to_string();
            if path.is_empty() {
                return Err("run program without a path".into());
            }
            let args = split_args(str_field(e, "runProgramArgs").unwrap_or_default());
            let wd = str_field(e, "runProgramStartInDir")
                .filter(|s| !s.is_empty())
                .map(String::from);
            Ok(Target::Run(RunProgram {
                path,
                args,
                working_dir: wd,
            }))
        }
        2 => {
            let uri = str_field(e, "openUri").unwrap_or_default();
            if uri.is_empty() {
                return Err("open URI without a URI".into());
            }
            Ok(Target::OpenUri(uri.to_string()))
        }
        _ => match str_field(e, "newRemapKeys").filter(|s| !s.is_empty()) {
            Some(k) => target_from_keys(k),
            None => Ok(Target::Text(
                str_field(e, "unicodeText").unwrap_or_default().to_string(),
            )),
        },
    }
}

pub(crate) fn import(json: &str) -> anyhow::Result<(Config, Vec<String>)> {
    let root: Value = serde_json::from_str(json)?;
    let mut cfg = Config::default();
    let mut warn = Vec::new();

    for e in entries(&root, "remapKeys", "inProcess") {
        let orig = str_field(e, "originalKeys").unwrap_or_default();
        if str_field(e, "condition") == Some("alone") {
            warn.push(format!(
                "remapKeys {orig}: 'alone' (tap) remaps are not supported, skipped"
            ));
            continue;
        }
        let r = parse_vks(orig).and_then(|ks| match ks.as_slice() {
            [k] => Ok(*k),
            _ => Err("expected one source key".into()),
        });
        let t = target_from_keys(str_field(e, "newRemapKeys").unwrap_or_default());
        match (r, t) {
            (Ok(k), Ok(t)) => cfg.remap_keys.push(KeyRemap {
                from: Key(k),
                to: t,
            }),
            (Err(err), _) | (_, Err(err)) => warn.push(format!("remapKeys {orig}: {err}")),
        }
    }
    for e in entries(&root, "remapKeysToText", "inProcess") {
        let orig = str_field(e, "originalKeys").unwrap_or_default();
        match parse_vks(orig).as_deref() {
            Ok([k]) => cfg.remap_keys.push(KeyRemap {
                from: Key(*k),
                to: Target::Text(str_field(e, "unicodeText").unwrap_or_default().to_string()),
            }),
            _ => warn.push(format!("remapKeysToText {orig}: expected one source key")),
        }
    }
    for section in ["remapShortcuts", "remapShortcutsToText"] {
        for list in ["global", "appSpecific"] {
            for e in entries(&root, section, list) {
                let orig = str_field(e, "originalKeys").unwrap_or_default();
                let from = parse_vks(orig).and_then(|ks| Shortcut::from_keys(&ks));
                let to = shortcut_target(e);
                let app = if list == "appSpecific" {
                    str_field(e, "targetApp").map(String::from)
                } else {
                    None
                };
                match (from, to) {
                    (Ok(from), Ok(to)) => cfg.remap_shortcuts.push(ShortcutRemap { from, to, app }),
                    (Err(err), _) | (_, Err(err)) => {
                        let names: Vec<String> = parse_vks(orig)
                            .unwrap_or_default()
                            .into_iter()
                            .map(key_name)
                            .collect();
                        warn.push(format!(
                            "{section}.{list} {}: {err}, skipped",
                            names.join("+")
                        ))
                    }
                }
            }
        }
    }
    Ok((cfg, warn))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_input::vk::*;

    #[test]
    fn import_powertoys_default_json() {
        let json = r#"{
          "remapKeys": {"inProcess": [
            {"originalKeys": "20", "newRemapKeys": "27"},
            {"originalKeys": "45", "newRemapKeys": "256"},
            {"originalKeys": "112", "newRemapKeys": "17;16;84"},
            {"originalKeys": "163", "newRemapKeys": "27", "condition": "alone"}
          ]},
          "remapKeysToText": {"inProcess": [{"originalKeys": "113", "unicodeText": "hello"}]},
          "remapShortcuts": {
            "global": [
              {"originalKeys": "17;37", "exactMatch": false, "operationType": 0, "newRemapKeys": "164;37"},
              {"originalKeys": "91;18;84", "operationType": 1, "runProgramFilePath": "C:\\Windows\\notepad.exe",
               "runProgramArgs": "\"C:\\a b.txt\" -x", "runProgramStartInDir": "", "newRemapKeys": ""},
              {"originalKeys": "91;18;71", "operationType": 2, "openUri": "https://github.com", "newRemapKeys": ""},
              {"originalKeys": "17;75;68", "newRemapKeys": "65"}
            ],
            "appSpecific": [
              {"originalKeys": "17;87", "newRemapKeys": "256", "targetApp": "msedge.exe"}
            ]
          },
          "remapShortcutsToText": {
            "global": [{"originalKeys": "17;18;68", "unicodeText": "2026"}],
            "appSpecific": []
          }
        }"#;
        let (c, warn) = Config::from_powertoys_json(json).unwrap();
        assert_eq!(warn.len(), 2, "{warn:?}"); // alone + chord
        assert_eq!(c.remap_keys.len(), 4);
        assert_eq!(c.remap_keys[0].to, Target::Key(VK_ESCAPE));
        assert_eq!(c.remap_keys[1].to, Target::Disabled);
        assert_eq!(
            c.remap_keys[2].to,
            Target::Shortcut("Ctrl+Shift+T".parse().unwrap())
        );
        assert_eq!(c.remap_keys[3].to, Target::Text("hello".into()));
        assert_eq!(c.remap_shortcuts.len(), 5);
        assert_eq!(c.remap_shortcuts[0].from, "Ctrl+Left".parse().unwrap());
        assert_eq!(
            c.remap_shortcuts[0].to,
            Target::Shortcut("LAlt+Left".parse().unwrap())
        );
        match &c.remap_shortcuts[1].to {
            Target::Run(r) => {
                assert_eq!(r.path, "C:\\Windows\\notepad.exe");
                assert_eq!(r.args, vec!["C:\\a b.txt".to_string(), "-x".into()]);
                assert_eq!(r.working_dir, None);
            }
            t => panic!("{t:?}"),
        }
        assert_eq!(
            c.remap_shortcuts[2].to,
            Target::OpenUri("https://github.com".into())
        );
        assert_eq!(c.remap_shortcuts[3].app.as_deref(), Some("msedge.exe"));
        assert_eq!(c.remap_shortcuts[3].to, Target::Disabled);
        assert_eq!(c.remap_shortcuts[4].to, Target::Text("2026".into()));
        // The imported config serializes to our format and back.
        assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
    }

    #[test]
    fn args() {
        assert_eq!(split_args(r#"a "b c"  d"#), vec!["a", "b c", "d"]);
        assert_eq!(split_args(r#""""#), vec![""]);
        assert!(split_args("  ").is_empty());
    }
}
