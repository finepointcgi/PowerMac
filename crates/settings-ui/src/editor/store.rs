//! Reading and writing `fancyzones.json` for the editor.
//!
//! Every change re-reads the file (`fancyzones::Config::load_from`), applies
//! just that change and writes it back (`Config::save_to`), so whatever the
//! running app wrote in the meantime (window history, layouts applied from
//! the tray or a hotkey) is kept.

use std::path::Path;

use anyhow::Result;
use fancyzones::layout::{guid_eq, CustomLayout, LayoutType};
use fancyzones::{AppliedLayout, Config};

pub use fancyzones::LayoutHotkey;

/// The settings page's id for "default for other monitors".
pub const DEFAULT_TARGET: &str = "";

/// Load `path` (defaults if it doesn't exist).
pub fn load(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load_from(path)
    } else {
        Ok(Config::default())
    }
}

/// Re-read `path`, apply `f`, write it back and return the new state.
/// A file that exists but can't be parsed is never overwritten.
pub fn modify(path: &Path, f: impl FnOnce(&mut Config)) -> Result<Config> {
    let mut c = load(path)?;
    f(&mut c);
    c.save_to(path)?;
    Ok(c)
}

// ---------------------------------------------------------------------------
// Edits (pure; applied inside `modify`)

/// The layout applied to `target` (a monitor id, or [`DEFAULT_TARGET`]).
pub fn applied_for(c: &Config, target: &str) -> AppliedLayout {
    if target == DEFAULT_TARGET {
        c.settings.default_layout.clone()
    } else {
        c.layout_for(target)
    }
}

pub fn apply_layout(c: &mut Config, target: &str, layout: AppliedLayout) {
    if target == DEFAULT_TARGET {
        c.settings.default_layout = layout;
    } else {
        c.set_layout(target, None, layout);
    }
}

/// Remember a template's zone count / spacing / distance (`layout_templates`).
pub fn set_template(c: &mut Config, layout: AppliedLayout) {
    c.layout_templates.retain(|t| t.kind != layout.kind);
    c.layout_templates.push(layout);
}

fn refers_to(l: &AppliedLayout, uuid: &str) -> bool {
    l.kind == LayoutType::Custom && guid_eq(&l.uuid, uuid)
}

/// Every applied layout in the file (per monitor, per virtual desktop, default).
fn all_applied(c: &mut Config) -> impl Iterator<Item = &mut AppliedLayout> {
    c.applied_layouts
        .values_mut()
        .chain(c.virtual_desktop_layouts.values_mut().flat_map(|m| m.values_mut()))
        .chain(std::iter::once(&mut c.settings.default_layout))
}

/// Insert or replace a custom layout; monitors using it get its new zone
/// count / spacing / sensitivity (what the FancyZones editor does on save).
pub fn upsert_custom(c: &mut Config, layout: CustomLayout) {
    let applied = AppliedLayout::custom(&layout);
    for l in all_applied(c) {
        if refers_to(l, &layout.uuid) {
            *l = applied.clone();
        }
    }
    c.upsert_custom_layout(layout);
}

/// Delete a custom layout: monitors using it fall back to the default
/// layout, and its hotkey is freed.
pub fn delete_custom(c: &mut Config, uuid: &str) {
    c.custom_layouts.retain(|l| !guid_eq(&l.uuid, uuid));
    c.applied_layouts.retain(|_, l| !refers_to(l, uuid));
    for m in c.virtual_desktop_layouts.values_mut() {
        m.retain(|_, l| !refers_to(l, uuid));
    }
    if refers_to(&c.settings.default_layout, uuid) {
        c.settings.default_layout = AppliedLayout::default();
    }
    c.layout_hotkeys.retain(|h| !guid_eq(&h.layout_uuid, uuid));
}

pub fn rename_custom(c: &mut Config, uuid: &str, name: &str) {
    if let Some(l) = c.custom_layouts.iter_mut().find(|l| guid_eq(&l.uuid, uuid)) {
        l.name = name.to_string();
    }
}

/// A copy of a custom layout under a new uuid ("<name> (copy)"); returns it.
pub fn duplicate_custom(c: &mut Config, uuid: &str) -> Option<CustomLayout> {
    let src = c.custom_layouts.iter().find(|l| guid_eq(&l.uuid, uuid))?;
    let mut copy = src.clone();
    copy.uuid = new_uuid();
    copy.name = unique_name(&c.custom_layouts, &format!("{} (copy)", src.name));
    c.custom_layouts.push(copy.clone());
    Some(copy)
}

pub fn hotkey_for(hotkeys: &[LayoutHotkey], uuid: &str) -> Option<u8> {
    hotkeys.iter().find(|h| guid_eq(&h.layout_uuid, uuid)).map(|h| h.key)
}

/// Give layout `uuid` hotkey `key` (taking it from any other layout), or none.
pub fn set_hotkey(c: &mut Config, uuid: &str, key: Option<u8>) {
    match key.filter(|k| *k <= 9) {
        Some(k) => c.set_layout_hotkey(k, Some(uuid.to_string())),
        None => c.layout_hotkeys.retain(|h| !guid_eq(&h.layout_uuid, uuid)),
    }
}

/// `name`, or `name 2`, `name 3`… if it is taken.
pub fn unique_name(custom: &[CustomLayout], name: &str) -> String {
    let taken = |n: &str| custom.iter().any(|c| c.name == n);
    if !taken(name) {
        return name.to_string();
    }
    (2..).map(|i| format!("{name} {i}")).find(|n| !taken(n)).unwrap_or_else(|| name.to_string())
}

/// A random `{XXXXXXXX-XXXX-4XXX-YXXX-XXXXXXXXXXXX}` GUID (FancyZones style).
pub fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let word = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.finish()
    };
    let (a, b) = (word(1), word(2));
    let b = (b & !(0b11 << 62)) | (0b10 << 62); // RFC 4122 variant
    let a = (a & !0xF000) | 0x4000; // version 4
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        a >> 32,
        (a >> 16) & 0xFFFF,
        a & 0xFFFF,
        (b >> 48) & 0xFFFF,
        b & 0xFFFF_FFFF_FFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancyzones::layout::{CanvasLayoutInfo, CustomLayoutInfo, GridLayoutInfo};

    fn grid(uuid: &str, name: &str) -> CustomLayout {
        CustomLayout {
            uuid: uuid.into(),
            name: name.into(),
            info: CustomLayoutInfo::Grid(GridLayoutInfo::new(vec![10000], vec![5000, 5000], vec![vec![0, 1]])),
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("pm-editor-store-{}-{name}", std::process::id())).join("fancyzones.json")
    }

    fn hk(key: u8, uuid: &str) -> LayoutHotkey {
        LayoutHotkey { key, layout_uuid: uuid.into() }
    }

    #[test]
    fn uuids_are_guids_and_unique() {
        let a = new_uuid();
        let b = new_uuid();
        assert_ne!(a, b);
        assert_eq!(a.len(), 38);
        assert!(a.starts_with('{') && a.ends_with('}'));
        assert_eq!(&a[15..16], "4");
        assert!("89AB".contains(&a[20..21]), "{a}");
        assert_eq!(a.matches('-').count(), 4);
    }

    #[test]
    fn hotkeys_are_exclusive() {
        let mut c = Config::default();
        set_hotkey(&mut c, "{A}", Some(1));
        set_hotkey(&mut c, "{B}", Some(2));
        assert_eq!(hotkey_for(&c.layout_hotkeys, "{a}"), Some(1));
        // Taking key 1 for B frees A's key and B's old one.
        set_hotkey(&mut c, "{B}", Some(1));
        assert_eq!(c.layout_hotkeys, vec![hk(1, "{B}")]);
        set_hotkey(&mut c, "{B}", None);
        assert!(c.layout_hotkeys.is_empty());
        set_hotkey(&mut c, "{C}", Some(12));
        assert!(c.layout_hotkeys.is_empty());
    }

    #[test]
    fn modify_keeps_what_others_wrote() {
        let path = tmp("keep");
        let mut c = Config::default();
        c.app_zone_history.insert(
            "/Applications/Safari.app".into(),
            fancyzones::config::AppZoneHistoryEntry { monitor: "m1".into(), layout_uuid: "x".into(), layout_kind: LayoutType::Grid, zones: vec![0] },
        );
        c.applied_layouts.insert("m2".into(), AppliedLayout::template(LayoutType::Rows, 2));
        c.save_to(&path).unwrap();

        let s = modify(&path, |s| {
            upsert_custom(s, grid("{A}", "Two"));
            set_hotkey(s, "{A}", Some(3));
            apply_layout(s, "m1", AppliedLayout::template(LayoutType::Columns, 4));
            set_template(s, AppliedLayout::template(LayoutType::Columns, 4));
        })
        .unwrap();
        assert_eq!(s.custom_layouts.len(), 1);
        let back = Config::load_from(&path).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.app_zone_history.len(), 1, "history kept");
        assert_eq!(back.applied_layouts["m2"].kind, LayoutType::Rows);
        assert_eq!(back.applied_layouts["m1"].zone_count, 4);
        assert_eq!(back.layout_hotkeys, vec![hk(3, "{A}")]);
        assert_eq!(back.template(LayoutType::Columns).zone_count, 4);
        // The hotkeys are stored in the typed field's JSON form.
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["layout_hotkeys"], serde_json::json!([{"key": 3, "layout_uuid": "{A}"}]));
        // Changes made by the running app in between are kept by the next edit.
        let mut disk = back.clone();
        disk.app_zone_history.clear();
        disk.applied_layouts.insert("m3".into(), AppliedLayout::template(LayoutType::Grid, 6));
        disk.save_to(&path).unwrap();
        let s = modify(&path, |s| rename_custom(s, "{A}", "Renamed")).unwrap();
        assert!(s.app_zone_history.is_empty());
        assert_eq!(s.applied_layouts["m3"].zone_count, 6);
        assert_eq!(s.custom_layouts[0].name, "Renamed");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn broken_file_is_not_overwritten() {
        let path = tmp("broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(modify(&path, |s| apply_layout(s, "m", AppliedLayout::default())).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
        // A missing file starts from defaults.
        assert_eq!(load(&tmp("missing")).unwrap(), Config::default());
    }

    #[test]
    fn upsert_updates_monitors_using_the_layout() {
        let mut s = Config::default();
        let mut l = grid("{A}", "x");
        upsert_custom(&mut s, l.clone());
        apply_layout(&mut s, "m1", AppliedLayout::custom(&l));
        apply_layout(&mut s, DEFAULT_TARGET, AppliedLayout::custom(&l));
        apply_layout(&mut s, "m2", AppliedLayout::template(LayoutType::Grid, 4));
        s.virtual_desktop_layouts.entry("vd".into()).or_default().insert("m1".into(), AppliedLayout::custom(&l));
        l.info = CustomLayoutInfo::Grid(GridLayoutInfo::new(vec![10000], vec![3333, 3333, 3334], vec![vec![0, 1, 2]]));
        upsert_custom(&mut s, l.clone());
        assert_eq!(s.custom_layouts.len(), 1);
        assert_eq!(s.applied_layouts["m1"].zone_count, 3);
        assert_eq!(s.settings.default_layout.zone_count, 3);
        assert_eq!(s.virtual_desktop_layouts["vd"]["m1"].zone_count, 3);
        assert_eq!(s.applied_layouts["m2"].zone_count, 4);
        assert_eq!(applied_for(&s, "m1").uuid, "{A}");
        assert_eq!(applied_for(&s, "unknown").uuid, "{A}");
    }

    #[test]
    fn delete_rename_duplicate() {
        let mut s = Config::default();
        upsert_custom(&mut s, grid("{A}", "Mine"));
        let canvas = CustomLayout {
            uuid: "{B}".into(),
            name: "Canvas".into(),
            info: CustomLayoutInfo::Canvas(CanvasLayoutInfo { ref_width: 100, ref_height: 100, zones: vec![], sensitivity_radius: 20 }),
        };
        upsert_custom(&mut s, canvas);
        let a = s.custom_layouts[0].clone();
        apply_layout(&mut s, "m1", AppliedLayout::custom(&a));
        apply_layout(&mut s, DEFAULT_TARGET, AppliedLayout::custom(&a));
        s.virtual_desktop_layouts.entry("vd".into()).or_default().insert("m1".into(), AppliedLayout::custom(&a));
        set_hotkey(&mut s, "{A}", Some(0));
        set_hotkey(&mut s, "{B}", Some(1));

        let copy = duplicate_custom(&mut s, "{a}").unwrap();
        assert_eq!(copy.name, "Mine (copy)");
        assert_ne!(copy.uuid, "{A}");
        assert_eq!(copy.info, a.info);
        assert_eq!(duplicate_custom(&mut s, "{A}").unwrap().name, "Mine (copy) 2");
        assert!(duplicate_custom(&mut s, "{nope}").is_none());

        rename_custom(&mut s, "{A}", "Renamed");
        assert_eq!(s.custom_layouts[0].name, "Renamed");

        delete_custom(&mut s, "{A}");
        assert!(s.custom_layouts.iter().all(|c| c.uuid != "{A}"));
        assert!(!s.applied_layouts.contains_key("m1"));
        assert!(s.virtual_desktop_layouts["vd"].is_empty());
        assert_eq!(s.settings.default_layout, AppliedLayout::default());
        assert_eq!(s.layout_hotkeys, vec![hk(1, "{B}")]);
        assert_eq!(s.custom_layouts.len(), 3);
    }
}
