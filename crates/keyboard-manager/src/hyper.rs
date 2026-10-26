//! Caps Lock → Hyper key.
//!
//! Modes ([`HyperMode`]):
//!
//! | mode | Caps Lock held | Caps Lock tapped alone |
//! |---|---|---|
//! | `off` (default) | normal Caps Lock | normal Caps Lock |
//! | `hyper` | Control+Option+Shift+Command (Ctrl+Alt+Shift+Win on Windows) | `tapAction` |
//! | `control` | Control | `tapAction` |
//! | `escape` | Escape (auto-repeats) | Escape |
//!
//! `tapAction` ([`TapAction`]) is `escape` (default), `none` or `capsLock`
//! (toggle Caps Lock, so it is still reachable). A press counts as a tap when
//! no other key (or mouse button, on macOS) was used while it was held and
//! it was released within `tapTimeoutMs` (default 200 ms).
//!
//! The modifiers are pressed *lazily*: only when another key (or a mouse
//! button / scroll on macOS) is used while Caps Lock is held, right before
//! that key. A tap therefore never sends a lone Ctrl/Cmd/Win press (no Start
//! menu, no "double-tap Ctrl" side effects), and Hyper+K arrives in every app
//! as Ctrl+Option+Shift+Cmd+K. Because the modifiers go through the remap
//! engine's logical layer, shortcut remaps such as
//! `{"from": "Ctrl+Alt+Shift+Cmd+T", "to": {"run": ...}}` fire on Hyper+T.
//!
//! **macOS:** Caps Lock only produces a flagsChanged toggle (no key-up), so
//! while the feature is on, the Caps Lock key is mapped to F18 for the login
//! session with `hidutil property --set '{"UserKeyMapping":[...]}'` (no root
//! needed; the mapping vanishes at logout/reboot). The previous
//! `UserKeyMapping` of each keyboard is read first and merged (other
//! mappings are kept, only the Caps Lock entry is replaced) and put back when
//! the feature is switched off or Keyboard Manager stops. The event tap then
//! treats F18 down/up as the Hyper key. Keyboards plugged in later are
//! mapped when their first Caps Lock press arrives. A marker file in the
//! temp dir lets the next start undo the mapping after a crash.
//!
//! **Windows:** the low-level hook swallows `VK_CAPITAL` and injects the
//! modifiers; Win/Alt releases are wrapped with the dummy key so the Start
//! menu doesn't open. Note that Ctrl+Alt+Shift+Win (+W/T/Y/O/P/D/L/X/N/Space)
//! are Office shortcuts on Windows 10/11.

use serde::{Deserialize, Serialize};

/// What the Caps Lock key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HyperMode {
    /// Leave Caps Lock alone.
    #[default]
    Off,
    /// Held: Control+Option+Shift+Command; tap: [`TapAction`].
    Hyper,
    /// Held: Control; tap: [`TapAction`].
    Control,
    /// Caps Lock is Escape.
    Escape,
}

/// What a quick tap of Caps Lock (alone) does in `hyper` / `control` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TapAction {
    #[default]
    Escape,
    None,
    /// Toggle Caps Lock.
    #[serde(alias = "caps_lock", alias = "capslock", alias = "CapsLock")]
    CapsLock,
}

/// `"hyperKey": {"mode": "hyper", "tapAction": "escape", "tapTimeoutMs": 200}`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HyperKeyConfig {
    pub mode: HyperMode,
    pub tap_action: TapAction,
    /// A press longer than this is never a tap.
    pub tap_timeout_ms: u64,
}

impl Default for HyperKeyConfig {
    fn default() -> Self {
        Self {
            mode: HyperMode::Off,
            tap_action: TapAction::Escape,
            tap_timeout_ms: 200,
        }
    }
}

impl HyperKeyConfig {
    pub fn is_on(&self) -> bool {
        self.mode != HyperMode::Off
    }
}

// ---------------------------------------------------------------------------
// hidutil UserKeyMapping (pure part: parsing, merging; unit-tested everywhere)
// ---------------------------------------------------------------------------

/// HID usage of Caps Lock (page 7, usage 0x39).
pub const HID_CAPS_LOCK: u64 = 0x7_0000_0039;
/// HID usage of F18 (page 7, usage 0x6D).
pub const HID_F18: u64 = 0x7_0000_006D;

/// One `UserKeyMapping` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyMapping {
    pub src: u64,
    pub dst: u64,
}

/// Our entry: Caps Lock → F18.
pub const CAPS_TO_F18: KeyMapping = KeyMapping {
    src: HID_CAPS_LOCK,
    dst: HID_F18,
};

/// The value of one service's `UserKeyMapping`: `None` = not set (`(null)`).
pub type MappingValue = Option<Vec<KeyMapping>>;

fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim().trim_end_matches(';').trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Parse an NSArray description of mapping dictionaries (`(null)`, `(\n)`,
/// `( { HIDKeyboardModifierMappingDst = 30064771181; ... } )`).
pub fn parse_mapping_value(s: &str) -> MappingValue {
    let t = s.trim();
    if t.is_empty() || t.starts_with("(null)") || t.starts_with("<null>") || t == "null" {
        return None;
    }
    let mut out = Vec::new();
    for block in t.split('{').skip(1) {
        let body = block.split('}').next().unwrap_or("");
        let (mut src, mut dst) = (None, None);
        for part in body.split(';') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            match k.trim().trim_matches('"') {
                "HIDKeyboardModifierMappingSrc" => src = parse_num(v),
                "HIDKeyboardModifierMappingDst" => dst = parse_num(v),
                _ => {}
            }
        }
        if let (Some(src), Some(dst)) = (src, dst) {
            out.push(KeyMapping { src, dst });
        }
    }
    Some(out)
}

/// Parse `hidutil property [--matching ...] --get UserKeyMapping`.
///
/// Newer macOS prints a table (`RegistryID  Key  Value`, one row per
/// service, values spanning several lines); older versions print just the
/// value. Returns `(registry id, value)` per service; the id is 0 for the
/// old format.
pub fn parse_get_output(text: &str) -> Vec<(u64, MappingValue)> {
    let mut rows: Vec<(u64, String)> = Vec::new();
    let mut table = false;
    for line in text.lines() {
        let l = line.trim_start();
        if l.starts_with("RegistryID") {
            table = true;
            continue;
        }
        if table {
            let mut it = l.splitn(3, char::is_whitespace);
            let id = it.next().unwrap_or("");
            let rest = it.collect::<Vec<_>>().join(" ");
            let rest = rest.trim_start();
            if !id.is_empty()
                && id.chars().all(|c| c.is_ascii_hexdigit())
                && rest.starts_with("UserKeyMapping")
            {
                let value = rest["UserKeyMapping".len()..].to_string();
                rows.push((u64::from_str_radix(id, 16).unwrap_or(0), value));
                continue;
            }
            if let Some(r) = rows.last_mut() {
                r.1.push('\n');
                r.1.push_str(line);
            }
        }
    }
    if !table {
        return vec![(0, parse_mapping_value(text))];
    }
    rows.into_iter()
        .map(|(id, v)| (id, parse_mapping_value(&v)))
        .collect()
}

/// Add `ours`, replacing any entry with the same source key; every other
/// entry is kept in order.
pub fn merge_mapping(existing: &MappingValue, ours: KeyMapping) -> Vec<KeyMapping> {
    let mut v: Vec<KeyMapping> = existing
        .iter()
        .flatten()
        .copied()
        .filter(|m| m.src != ours.src)
        .collect();
    v.push(ours);
    v
}

/// Undo [`merge_mapping`] on the *current* value (which may have been
/// changed by someone else meanwhile): drop `ours`, and put back the
/// original entry for the same source key, if there was one and nothing
/// else claimed that key since.
pub fn unmerge_mapping(
    current: &MappingValue,
    ours: KeyMapping,
    original: &MappingValue,
) -> Vec<KeyMapping> {
    let mut v: Vec<KeyMapping> = current
        .iter()
        .flatten()
        .copied()
        .filter(|m| *m != ours)
        .collect();
    if !v.iter().any(|m| m.src == ours.src) {
        if let Some(o) = original.iter().flatten().find(|m| m.src == ours.src) {
            // Put it back where it was relative to the other entries.
            let pos = original
                .iter()
                .flatten()
                .position(|m| m == o)
                .unwrap_or(v.len())
                .min(v.len());
            v.insert(pos, *o);
        }
    }
    v
}

/// Whether a value contains our entry.
pub fn has_mapping(v: &MappingValue, ours: KeyMapping) -> bool {
    v.iter().flatten().any(|m| *m == ours)
}

/// The JSON body for `hidutil property --set`.
pub fn set_json(v: &[KeyMapping]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|m| {
            format!(
                "{{\"HIDKeyboardModifierMappingSrc\":{},\"HIDKeyboardModifierMappingDst\":{}}}",
                m.src, m.dst
            )
        })
        .collect();
    format!("{{\"UserKeyMapping\":[{}]}}", items.join(","))
}

/// Which services a `hidutil` call targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HidTarget {
    /// `--matching keyboard` (every keyboard service).
    Keyboards,
    /// One device, by vendor / product / location id.
    Device {
        vendor: u64,
        product: u64,
        location: u64,
    },
}

impl HidTarget {
    pub fn matching(&self) -> String {
        match self {
            HidTarget::Keyboards => "keyboard".into(),
            HidTarget::Device {
                vendor,
                product,
                location,
            } => format!(
                "{{\"VendorID\":{vendor},\"ProductID\":{product},\"LocationID\":{location}}}"
            ),
        }
    }
}

/// Runs `hidutil` (a trait so the apply/restore logic is testable).
pub trait HidUtil {
    fn run(&self, args: &[String]) -> anyhow::Result<String>;
}

/// The real `/usr/bin/hidutil`.
pub struct SystemHidUtil;

impl HidUtil for SystemHidUtil {
    fn run(&self, args: &[String]) -> anyhow::Result<String> {
        let out = std::process::Command::new("/usr/bin/hidutil")
            .args(args)
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "hidutil {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Service {
    id: u64,
    target: HidTarget,
    value: MappingValue,
}

/// One saved original: the target and its value before we changed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub target: HidTarget,
    pub original: MappingValue,
}

/// Caps Lock → F18 applied with hidutil; [`restore`](Self::restore) puts the
/// previous mappings back.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidMapping {
    pub saved: Vec<Saved>,
}

fn get(h: &dyn HidUtil, t: &HidTarget) -> anyhow::Result<Vec<(u64, MappingValue)>> {
    let out = h.run(&[
        "property".into(),
        "--matching".into(),
        t.matching(),
        "--get".into(),
        "UserKeyMapping".into(),
    ])?;
    Ok(parse_get_output(&out))
}

fn set(h: &dyn HidUtil, t: &HidTarget, v: &[KeyMapping]) -> anyhow::Result<()> {
    h.run(&[
        "property".into(),
        "--matching".into(),
        t.matching(),
        "--set".into(),
        set_json(v),
    ])?;
    Ok(())
}

/// Keyboard services under `t` with their device ids and current values.
fn services(h: &dyn HidUtil, t: &HidTarget) -> anyhow::Result<Vec<Service>> {
    let values = get(h, t)?;
    let list = h.run(&[
        "list".into(),
        "--ndjson".into(),
        "--matching".into(),
        t.matching(),
    ])?;
    let mut out = Vec::new();
    for line in list.lines() {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if j.get("type").and_then(|v| v.as_str()) != Some("service") {
            continue;
        }
        let n = |k: &str| j.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let id = n("IORegistryEntryID");
        let value = values
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, v)| v.clone())
            .unwrap_or(None);
        out.push(Service {
            id,
            target: HidTarget::Device {
                vendor: n("VendorID"),
                product: n("ProductID"),
                location: n("LocationID"),
            },
            value,
        });
    }
    // `hidutil list` without services (unusual): fall back to the values.
    if out.is_empty() && !values.is_empty() && *t == HidTarget::Keyboards {
        if let Some((id, v)) = values.first() {
            out.push(Service {
                id: *id,
                target: HidTarget::Keyboards,
                value: v.clone(),
            });
        }
    }
    Ok(out)
}

fn uniform(svcs: &[Service]) -> Option<&MappingValue> {
    let first = &svcs.first()?.value;
    svcs.iter().all(|s| s.value == *first).then_some(first)
}

impl HidMapping {
    /// Map Caps Lock to F18 on every keyboard, remembering what was there.
    pub fn apply(h: &dyn HidUtil) -> anyhow::Result<HidMapping> {
        let svcs = services(h, &HidTarget::Keyboards)?;
        if svcs.is_empty() {
            anyhow::bail!("hidutil found no keyboard");
        }
        let mut m = HidMapping::default();
        if let Some(v) = uniform(&svcs) {
            set(h, &HidTarget::Keyboards, &merge_mapping(v, CAPS_TO_F18))?;
            m.saved.push(Saved {
                target: HidTarget::Keyboards,
                original: v.clone(),
            });
        } else {
            for s in &svcs {
                if m.saved.iter().any(|x| x.target == s.target) {
                    continue;
                }
                set(h, &s.target, &merge_mapping(&s.value, CAPS_TO_F18))?;
                m.saved.push(Saved {
                    target: s.target.clone(),
                    original: s.value.clone(),
                });
            }
        }
        Ok(m)
    }

    /// Map keyboards that don't have the mapping yet (plugged in later).
    /// Returns how many were mapped.
    pub fn reapply(&mut self, h: &dyn HidUtil) -> anyhow::Result<usize> {
        let svcs = services(h, &HidTarget::Keyboards)?;
        let mut n = 0;
        for s in svcs {
            if has_mapping(&s.value, CAPS_TO_F18) || s.target == HidTarget::Keyboards {
                continue;
            }
            set(h, &s.target, &merge_mapping(&s.value, CAPS_TO_F18))?;
            if !self.saved.iter().any(|x| x.target == s.target) {
                self.saved.push(Saved {
                    target: s.target.clone(),
                    original: s.value.clone(),
                });
            }
            n += 1;
        }
        Ok(n)
    }

    /// Put the previous mappings back (later saves first). Changes others
    /// made meanwhile are kept; only our entry is removed.
    pub fn restore(&self, h: &dyn HidUtil) -> anyhow::Result<()> {
        let mut first_err = None;
        for s in self.saved.iter().rev() {
            let r = (|| -> anyhow::Result<()> {
                let svcs = services(h, &s.target)?;
                if let Some(cur) = uniform(&svcs) {
                    if has_mapping(cur, CAPS_TO_F18) {
                        set(
                            h,
                            &s.target,
                            &unmerge_mapping(cur, CAPS_TO_F18, &s.original),
                        )?;
                    }
                } else {
                    for svc in &svcs {
                        if has_mapping(&svc.value, CAPS_TO_F18) {
                            set(
                                h,
                                &svc.target,
                                &unmerge_mapping(&svc.value, CAPS_TO_F18, &s.original),
                            )?;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(e) = r {
                log::warn!("keyboard-manager: restoring Caps Lock mapping: {e:#}");
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// macOS lifecycle (with a crash marker)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub(crate) mod session {
    use super::*;
    use std::path::PathBuf;

    #[derive(Serialize, Deserialize)]
    struct Marker {
        pid: u32,
        mapping: HidMapping,
    }

    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn getuid() -> u32;
    }

    fn marker_path() -> PathBuf {
        std::env::temp_dir().join(format!("powermac-hyper-key-{}.json", unsafe { getuid() }))
    }

    fn alive(pid: u32) -> bool {
        pid == std::process::id() || unsafe { kill(pid as i32, 0) == 0 }
    }

    fn write_marker(m: &HidMapping) {
        let marker = Marker {
            pid: std::process::id(),
            mapping: m.clone(),
        };
        if let Ok(s) = serde_json::to_string(&marker) {
            if let Err(e) = std::fs::write(marker_path(), s) {
                log::debug!("keyboard-manager: hyper marker: {e}");
            }
        }
    }

    /// Undo a mapping left behind by a crashed run (its marker names a dead
    /// process).
    pub fn recover() {
        let p = marker_path();
        let Ok(s) = std::fs::read_to_string(&p) else {
            return;
        };
        match serde_json::from_str::<Marker>(&s) {
            Ok(m) if !alive(m.pid) => {
                log::info!("keyboard-manager: undoing the Caps Lock mapping of a previous run");
                if m.mapping.restore(&SystemHidUtil).is_ok() {
                    let _ = std::fs::remove_file(&p);
                }
            }
            Ok(_) => {}
            Err(_) => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    /// Caps Lock → F18 for as long as this lives.
    pub struct Session {
        mapping: HidMapping,
    }

    impl Session {
        pub fn start() -> anyhow::Result<Session> {
            recover();
            let mapping = HidMapping::apply(&SystemHidUtil)?;
            write_marker(&mapping);
            log::info!("keyboard-manager: Caps Lock mapped to F18 (hyper key)");
            Ok(Session { mapping })
        }

        pub fn reapply(&mut self) {
            match self.mapping.reapply(&SystemHidUtil) {
                Ok(0) => {}
                Ok(n) => {
                    log::info!("keyboard-manager: mapped Caps Lock on {n} new keyboard(s)");
                    write_marker(&self.mapping);
                }
                Err(e) => log::warn!("keyboard-manager: {e:#}"),
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            match self.mapping.restore(&SystemHidUtil) {
                Ok(()) => {
                    let _ = std::fs::remove_file(marker_path());
                    log::info!("keyboard-manager: Caps Lock mapping restored");
                }
                Err(e) => log::warn!("keyboard-manager: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const TABLE: &str = "RegistryID  Key                   Value
100000930   UserKeyMapping   (null)
100000cad   UserKeyMapping   (
        {
        HIDKeyboardModifierMappingDst = 30064771300;
        HIDKeyboardModifierMappingSrc = 30064771299;
    },
        {
        HIDKeyboardModifierMappingDst = 30064771296;
        HIDKeyboardModifierMappingSrc = 30064771129;
    }
)
100000cae   UserKeyMapping   (
)
";

    #[test]
    fn parses_table_and_plain_output() {
        let rows = parse_get_output(TABLE);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (0x100000930, None));
        assert_eq!(
            rows[1],
            (
                0x100000cad,
                Some(vec![
                    KeyMapping {
                        src: 0x7000000E3,
                        dst: 0x7000000E4
                    },
                    KeyMapping {
                        src: HID_CAPS_LOCK,
                        dst: 0x7000000E0
                    },
                ])
            )
        );
        assert_eq!(rows[2], (0x100000cae, Some(vec![])));
        // old format: just the value
        assert_eq!(parse_get_output("(null)\n"), vec![(0, None)]);
        let plain = "(\n    {\n    HIDKeyboardModifierMappingDst = 0x70000006D;\n    HIDKeyboardModifierMappingSrc = 0x700000039;\n}\n)";
        assert_eq!(parse_get_output(plain), vec![(0, Some(vec![CAPS_TO_F18]))]);
    }

    #[test]
    fn merge_keeps_other_mappings_and_replaces_caps() {
        let other = KeyMapping {
            src: 0x7000000E3,
            dst: 0x7000000E4,
        };
        let caps_ctrl = KeyMapping {
            src: HID_CAPS_LOCK,
            dst: 0x7000000E0,
        };
        assert_eq!(merge_mapping(&None, CAPS_TO_F18), vec![CAPS_TO_F18]);
        let orig = Some(vec![caps_ctrl, other]);
        let merged = merge_mapping(&orig, CAPS_TO_F18);
        assert_eq!(merged, vec![other, CAPS_TO_F18]);
        // restore: Caps → Ctrl comes back at its old position
        assert_eq!(
            unmerge_mapping(&Some(merged.clone()), CAPS_TO_F18, &orig),
            vec![caps_ctrl, other]
        );
        // someone added a mapping meanwhile: kept
        let extra = KeyMapping { src: 1, dst: 2 };
        let mut cur = merged.clone();
        cur.push(extra);
        assert_eq!(
            unmerge_mapping(&Some(cur), CAPS_TO_F18, &orig),
            vec![caps_ctrl, other, extra]
        );
        // someone remapped Caps meanwhile: theirs wins, no duplicate
        let theirs = KeyMapping {
            src: HID_CAPS_LOCK,
            dst: 5,
        };
        assert_eq!(
            unmerge_mapping(&Some(vec![other, theirs]), CAPS_TO_F18, &orig),
            vec![other, theirs]
        );
        // original was empty / null: back to empty
        assert_eq!(
            unmerge_mapping(&Some(vec![CAPS_TO_F18]), CAPS_TO_F18, &None),
            vec![]
        );
        // user had Caps → F18 themselves (Hammerspoon setup): it stays
        assert_eq!(
            unmerge_mapping(
                &Some(vec![CAPS_TO_F18]),
                CAPS_TO_F18,
                &Some(vec![CAPS_TO_F18])
            ),
            vec![CAPS_TO_F18]
        );
    }

    #[test]
    fn set_json_format() {
        assert_eq!(set_json(&[]), r#"{"UserKeyMapping":[]}"#);
        let j = set_json(&[CAPS_TO_F18]);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(
            v["UserKeyMapping"][0]["HIDKeyboardModifierMappingSrc"],
            0x700000039u64
        );
        assert_eq!(
            v["UserKeyMapping"][0]["HIDKeyboardModifierMappingDst"],
            0x70000006Du64
        );
    }

    /// (registry id, (vendor, product, location), value)
    type FakeSvc = (u64, (u64, u64, u64), MappingValue);

    /// Fake hidutil: services with ids, device ids and values.
    struct Fake {
        svcs: RefCell<Vec<FakeSvc>>,
        log: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(v: Vec<FakeSvc>) -> Self {
            Fake {
                svcs: RefCell::new(v),
                log: RefCell::default(),
            }
        }
        fn matches(m: &str, ids: (u64, u64, u64)) -> bool {
            m == "keyboard"
                || m == HidTarget::Device {
                    vendor: ids.0,
                    product: ids.1,
                    location: ids.2,
                }
                .matching()
        }
        fn values(&self) -> HashMap<u64, MappingValue> {
            self.svcs
                .borrow()
                .iter()
                .map(|(i, _, v)| (*i, v.clone()))
                .collect()
        }
    }

    fn describe(v: &MappingValue) -> String {
        match v {
            None => "(null)".into(),
            Some(v) => {
                let mut s = String::from("(\n");
                for m in v {
                    s += &format!(
                        "    {{\n    HIDKeyboardModifierMappingDst = {};\n    HIDKeyboardModifierMappingSrc = {};\n}},\n",
                        m.dst, m.src
                    );
                }
                s + ")"
            }
        }
    }

    impl HidUtil for Fake {
        fn run(&self, args: &[String]) -> anyhow::Result<String> {
            self.log.borrow_mut().push(args.join(" "));
            let m = &args[args.iter().position(|a| a == "--matching").unwrap() + 1];
            if args[0] == "list" {
                let mut s = String::new();
                for (id, ids, _) in self.svcs.borrow().iter() {
                    if Self::matches(m, *ids) {
                        s += &format!(
                            "{{\"type\":\"service\",\"IORegistryEntryID\":{id},\"VendorID\":{},\"ProductID\":{},\"LocationID\":{}}}\n",
                            ids.0, ids.1, ids.2
                        );
                    }
                }
                return Ok(s);
            }
            if args[3] == "--get" {
                let mut s = String::from("RegistryID  Key                   Value\n");
                for (id, ids, v) in self.svcs.borrow().iter() {
                    if Self::matches(m, *ids) {
                        s += &format!("{id:x}   UserKeyMapping   {}\n", describe(v));
                    }
                }
                return Ok(s);
            }
            let body: serde_json::Value = serde_json::from_str(&args[4])?;
            let new: Vec<KeyMapping> = body["UserKeyMapping"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| KeyMapping {
                    src: e["HIDKeyboardModifierMappingSrc"].as_u64().unwrap(),
                    dst: e["HIDKeyboardModifierMappingDst"].as_u64().unwrap(),
                })
                .collect();
            for (_, ids, v) in self.svcs.borrow_mut().iter_mut() {
                if Self::matches(m, *ids) {
                    *v = Some(new.clone());
                }
            }
            Ok(String::new())
        }
    }

    #[test]
    fn apply_and_restore_uniform() {
        let other = KeyMapping { src: 7, dst: 8 };
        let f = Fake::new(vec![
            (0x10, (1, 2, 3), Some(vec![other])),
            (0x11, (4, 5, 6), Some(vec![other])),
        ]);
        let m = HidMapping::apply(&f).unwrap();
        assert_eq!(m.saved.len(), 1);
        assert_eq!(m.saved[0].target, HidTarget::Keyboards);
        for v in f.values().values() {
            assert_eq!(v, &Some(vec![other, CAPS_TO_F18]));
        }
        m.restore(&f).unwrap();
        for v in f.values().values() {
            assert_eq!(v, &Some(vec![other]));
        }
    }

    #[test]
    fn apply_and_restore_per_device() {
        // Different per-keyboard mappings (e.g. System Settings' modifier
        // keys): each keyboard is mapped and restored on its own.
        let a = KeyMapping {
            src: HID_CAPS_LOCK,
            dst: 0x7000000E0,
        };
        let b = KeyMapping { src: 9, dst: 10 };
        let f = Fake::new(vec![
            (0x10, (1, 2, 3), Some(vec![a])),
            (0x11, (4, 5, 6), Some(vec![b])),
            (0x12, (7, 8, 9), None),
        ]);
        let mut m = HidMapping::apply(&f).unwrap();
        assert_eq!(m.saved.len(), 3);
        let v = f.values();
        assert_eq!(v[&0x10], Some(vec![CAPS_TO_F18]));
        assert_eq!(v[&0x11], Some(vec![b, CAPS_TO_F18]));
        assert_eq!(v[&0x12], Some(vec![CAPS_TO_F18]));
        // a keyboard plugged in later
        f.svcs.borrow_mut().push((0x13, (11, 12, 13), None));
        assert_eq!(m.reapply(&f).unwrap(), 1);
        assert_eq!(m.reapply(&f).unwrap(), 0);
        m.restore(&f).unwrap();
        let v = f.values();
        assert_eq!(v[&0x10], Some(vec![a]));
        assert_eq!(v[&0x11], Some(vec![b]));
        assert_eq!(v[&0x12], Some(vec![]));
        assert_eq!(v[&0x13], Some(vec![]));
    }

    #[test]
    fn config_json() {
        let c: HyperKeyConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, HyperKeyConfig::default());
        assert!(!c.is_on());
        let c: HyperKeyConfig =
            serde_json::from_str(r#"{"mode":"hyper","tapAction":"caps_lock","tapTimeoutMs":300}"#)
                .unwrap();
        assert_eq!(c.mode, HyperMode::Hyper);
        assert_eq!(c.tap_action, TapAction::CapsLock);
        assert_eq!(c.tap_timeout_ms, 300);
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"capsLock\""), "{s}");
        assert_eq!(serde_json::from_str::<HyperKeyConfig>(&s).unwrap(), c);
    }
}
