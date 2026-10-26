//! Persistent configuration (settings, per-monitor applied layouts, custom
//! layouts, app zone history) plus import/export of FancyZones' own
//! `custom-layouts.json` / `applied-layouts.json` formats.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::layout::{AppliedLayout, CustomLayout, OverlappingZonesAlgorithm};

/// Modifier set for a hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub ctrl: bool,
    /// Option on macOS, Alt on Windows.
    pub alt: bool,
    pub shift: bool,
    /// Command on macOS, Win on Windows.
    pub meta: bool,
}

impl Modifiers {
    pub const fn new(ctrl: bool, alt: bool, shift: bool, meta: bool) -> Self {
        Self { ctrl, alt, shift, meta }
    }

    /// Default "snap" hotkey modifiers: Ctrl+Option on macOS (there is no
    /// Win key), Win on Windows (FancyZones' "override Windows Snap").
    pub fn default_snap() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(true, true, false, false)
        } else {
            Self::new(false, false, false, true)
        }
    }

    /// Default "extend" hotkey modifiers: Ctrl+Option+Cmd on macOS,
    /// Win+Ctrl+Alt on Windows.
    pub fn default_extend() -> Self {
        Self::new(true, true, false, true)
    }
}

fn d_snap() -> Modifiers {
    Modifiers::default_snap()
}
fn d_extend() -> Modifiers {
    Modifiers::default_extend()
}

/// FancyZones `Settings` subset that applies to this port. Defaults match
/// FancyZones except `override_snap_hotkeys` (on by default here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hold Shift while dragging to show zones (if `false`, zones show while
    /// dragging and Shift *hides* them).
    pub shift_drag: bool,
    /// Pressing the secondary (right) mouse button while dragging toggles zone activation.
    pub mouse_switch: bool,
    /// Keyboard snapping hotkeys (Win+Arrow / Ctrl+Option+Arrow).
    pub override_snap_hotkeys: bool,
    pub snap_modifiers: Modifiers,
    pub extend_modifiers: Modifiers,
    /// `false`: Left/Right cycle by zone index; `true`: all four arrows move spatially.
    pub move_windows_based_on_position: bool,
    pub move_window_across_monitors: bool,
    /// Treat all monitors as one big work area.
    pub span_zones_across_monitors: bool,
    pub show_zones_on_all_monitors: bool,
    pub show_zone_number: bool,
    /// Restore a window's original size when it is dragged out of its zone.
    pub restore_size: bool,
    /// Move newly opened windows of an app to the zone it last used.
    pub app_last_zone_move_windows: bool,
    pub zone_color: String,
    pub zone_border_color: String,
    pub zone_highlight_color: String,
    pub zone_number_color: String,
    /// 0..=100
    pub zone_highlight_opacity: u8,
    pub overlapping_zones_algorithm: OverlappingZonesAlgorithm,
    /// Applied to monitors that have no entry in `applied_layouts`.
    pub default_layout: AppliedLayout,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shift_drag: true,
            mouse_switch: false,
            override_snap_hotkeys: true,
            snap_modifiers: d_snap(),
            extend_modifiers: d_extend(),
            move_windows_based_on_position: false,
            move_window_across_monitors: false,
            span_zones_across_monitors: false,
            show_zones_on_all_monitors: false,
            show_zone_number: true,
            restore_size: false,
            app_last_zone_move_windows: false,
            zone_color: "#AACDFF".into(),
            zone_border_color: "#FFFFFF".into(),
            zone_highlight_color: "#008CFF".into(),
            zone_number_color: "#000000".into(),
            zone_highlight_opacity: 50,
            overlapping_zones_algorithm: OverlappingZonesAlgorithm::Smallest,
            default_layout: AppliedLayout::default(),
        }
    }
}

/// "Last zone" memory for an application (FancyZones `app-zone-history`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppZoneHistoryEntry {
    pub monitor: String,
    #[serde(rename = "zoneset-uuid")]
    pub layout_uuid: String,
    #[serde(rename = "layout-type")]
    pub layout_kind: crate::layout::LayoutType,
    #[serde(rename = "zone-index-set")]
    pub zones: Vec<usize>,
}

pub const CONFIG_VERSION: u32 = 1;

fn d_version() -> u32 {
    CONFIG_VERSION
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "d_version")]
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    /// Monitor id -> applied layout.
    #[serde(default)]
    pub applied_layouts: BTreeMap<String, AppliedLayout>,
    #[serde(default)]
    pub custom_layouts: Vec<CustomLayout>,
    /// App id (executable path / bundle path) -> last zone.
    #[serde(default)]
    pub app_zone_history: BTreeMap<String, AppZoneHistoryEntry>,
}

impl Config {
    /// `<config dir>/PowerMac/fancyzones.json`
    /// (`~/Library/Application Support/PowerMac/fancyzones.json` on macOS,
    /// `%APPDATA%\PowerMac\fancyzones.json` on Windows).
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("PowerMac").join("fancyzones.json"))
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&s).with_context(|| format!("parsing {}", path.display()))
    }

    /// Loads the default config file, or returns defaults if it does not exist.
    pub fn load_or_default() -> Result<Self> {
        match Self::default_path() {
            Some(p) if p.exists() => Self::load_from(&p),
            _ => Ok(Self::default()),
        }
    }

    /// Atomic write (temp file + rename).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json()?).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    pub fn save(&self) -> Result<PathBuf> {
        let p = Self::default_path().context("no config directory on this platform")?;
        self.save_to(&p)?;
        Ok(p)
    }

    /// Applied layout for a monitor (falls back to `settings.default_layout`).
    pub fn layout_for(&self, monitor_id: &str) -> AppliedLayout {
        self.applied_layouts
            .get(monitor_id)
            .cloned()
            .unwrap_or_else(|| self.settings.default_layout.clone())
    }

    /// Insert or replace a custom layout (matched by uuid).
    pub fn upsert_custom_layout(&mut self, layout: CustomLayout) {
        if let Some(e) = self.custom_layouts.iter_mut().find(|c| crate::layout::guid_eq(&c.uuid, &layout.uuid)) {
            *e = layout;
        } else {
            self.custom_layouts.push(layout);
        }
    }

    /// Merge FancyZones' `custom-layouts.json` contents.
    pub fn import_fancyzones_custom_layouts(&mut self, json: &str) -> Result<usize> {
        let layouts = parse_fancyzones_custom_layouts(json)?;
        let n = layouts.len();
        for l in layouts {
            self.upsert_custom_layout(l);
        }
        Ok(n)
    }

    /// Merge FancyZones' `applied-layouts.json`. Windows monitor ids rarely
    /// match the ids on another machine, so `map_monitor` can translate a
    /// FancyZones device to one of our monitor ids (return `None` to keep
    /// the FancyZones monitor string as-is).
    pub fn import_fancyzones_applied_layouts(
        &mut self,
        json: &str,
        mut map_monitor: impl FnMut(&FzDevice) -> Option<String>,
    ) -> Result<usize> {
        let entries = parse_fancyzones_applied_layouts(json)?;
        let n = entries.len();
        for (dev, layout) in entries {
            let key = map_monitor(&dev).unwrap_or_else(|| dev.monitor.clone());
            self.applied_layouts.insert(key, layout);
        }
        Ok(n)
    }

    pub fn export_fancyzones_custom_layouts(&self) -> Result<String> {
        #[derive(Serialize)]
        struct File<'a> {
            #[serde(rename = "custom-layouts")]
            custom_layouts: &'a [CustomLayout],
        }
        Ok(serde_json::to_string_pretty(&File { custom_layouts: &self.custom_layouts })?)
    }

    pub fn export_fancyzones_applied_layouts(&self) -> Result<String> {
        let entries: Vec<FzAppliedEntry> = self
            .applied_layouts
            .iter()
            .enumerate()
            .map(|(i, (m, l))| FzAppliedEntry {
                device: Some(FzDevice {
                    monitor: m.clone(),
                    monitor_instance: String::new(),
                    monitor_number: i as i32 + 1,
                    serial_number: String::new(),
                    virtual_desktop: crate::layout::NULL_GUID.into(),
                }),
                device_id: None,
                applied_layout: l.clone(),
            })
            .collect();
        Ok(serde_json::to_string_pretty(&FzAppliedFile { applied_layouts: entries })?)
    }
}

/// `device` object of an applied-layouts.json entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FzDevice {
    pub monitor: String,
    #[serde(rename = "monitor-instance", default)]
    pub monitor_instance: String,
    #[serde(rename = "monitor-number", default)]
    pub monitor_number: i32,
    #[serde(rename = "serial-number", default)]
    pub serial_number: String,
    #[serde(rename = "virtual-desktop", default)]
    pub virtual_desktop: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FzAppliedEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<FzDevice>,
    /// Legacy format: `"device-id": "<monitor>_<w>_<h>_<vd>"`.
    #[serde(rename = "device-id", default, skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    #[serde(rename = "applied-layout")]
    applied_layout: AppliedLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FzAppliedFile {
    #[serde(rename = "applied-layouts")]
    applied_layouts: Vec<FzAppliedEntry>,
}

/// Parse FancyZones' `custom-layouts.json`. Invalid entries are skipped
/// (like FancyZones), so a single broken layout does not lose the others.
pub fn parse_fancyzones_custom_layouts(json: &str) -> Result<Vec<CustomLayout>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v
        .get("custom-layouts")
        .and_then(|a| a.as_array())
        .context("missing 'custom-layouts' array")?;
    let mut out = Vec::new();
    for item in arr {
        match serde_json::from_value::<CustomLayout>(item.clone()) {
            Ok(l) => match &l.info {
                crate::layout::CustomLayoutInfo::Grid(g) if g.validate().is_err() => {
                    log::warn!("skipping invalid grid layout {}", l.uuid)
                }
                _ => out.push(l),
            },
            Err(e) => log::warn!("skipping custom layout: {e}"),
        }
    }
    Ok(out)
}

/// Parse FancyZones' `applied-layouts.json` (both the current `device`
/// object format and the legacy `device-id` string format).
pub fn parse_fancyzones_applied_layouts(json: &str) -> Result<Vec<(FzDevice, AppliedLayout)>> {
    let f: FzAppliedFile = serde_json::from_str(json)?;
    let mut out = Vec::new();
    for e in f.applied_layouts {
        let dev = match (e.device, e.device_id) {
            (Some(d), _) => d,
            (None, Some(id)) => FzDevice {
                monitor: id.split('_').next().unwrap_or(&id).to_string(),
                monitor_instance: String::new(),
                monitor_number: 0,
                serial_number: String::new(),
                virtual_desktop: String::new(),
            },
            (None, None) => continue,
        };
        out.push((dev, e.applied_layout));
    }
    Ok(out)
}

/// Parse `#RRGGBB` / `#AARRGGBB` into (r, g, b).
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    let h = match h.len() {
        6 => h,
        8 => &h[2..],
        _ => return None,
    };
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CustomLayoutInfo, Layout, LayoutType};
    use crate::geometry::Rect;

    const FZ_CUSTOM: &str = r#"{
      "custom-layouts": [
        {
          "uuid": "{D9D4F04A-9F5E-4D3B-8E54-5C1E3B5E1B11}",
          "name": "Custom grid",
          "type": "grid",
          "info": {
            "rows": 2, "columns": 3,
            "rows-percentage": [5000, 5000],
            "columns-percentage": [3333, 3333, 3334],
            "cell-child-map": [[0, 1, 2], [0, 3, 2]],
            "show-spacing": true, "spacing": 10, "sensitivity-radius": 25
          }
        },
        {
          "uuid": "{E2B2C3F6-1111-2222-3333-444455556666}",
          "name": "Canvas",
          "type": "canvas",
          "info": {
            "ref-width": 1920, "ref-height": 1080,
            "zones": [ {"X": 0, "Y": 0, "width": 960, "height": 1080}, {"X": 960, "Y": 0, "width": 960, "height": 540} ],
            "sensitivity-radius": 20
          }
        },
        { "uuid": "{BROKEN}", "name": "broken", "type": "grid", "info": { "rows": 2, "columns": 1, "rows-percentage": [10000], "columns-percentage": [10000], "cell-child-map": [[0]] } }
      ]
    }"#;

    const FZ_APPLIED: &str = r#"{
      "applied-layouts": [
        {
          "device": { "monitor": "LEN4123", "monitor-instance": "4&123&0&UID1", "monitor-number": 1, "serial-number": "", "virtual-desktop": "{00000000-0000-0000-0000-000000000000}" },
          "applied-layout": { "uuid": "{00000000-0000-0000-0000-000000000000}", "type": "priority-grid", "show-spacing": true, "spacing": 16, "zone-count": 3, "sensitivity-radius": 20 }
        },
        {
          "device-id": "DELL_1920_1080_{GUID}",
          "applied-layout": { "uuid": "{D9D4F04A-9F5E-4D3B-8E54-5C1E3B5E1B11}", "type": "custom", "show-spacing": false, "spacing": 0, "zone-count": 4 }
        }
      ]
    }"#;

    #[test]
    fn import_custom_layouts() {
        let l = parse_fancyzones_custom_layouts(FZ_CUSTOM).unwrap();
        assert_eq!(l.len(), 2);
        match &l[0].info {
            CustomLayoutInfo::Grid(g) => {
                assert_eq!(g.rows, 2);
                assert_eq!(g.spacing, 10);
                assert_eq!(g.sensitivity_radius, 25);
                assert_eq!(g.zone_count(), 4);
            }
            _ => panic!("expected grid"),
        }
        match &l[1].info {
            CustomLayoutInfo::Canvas(c) => assert_eq!(c.zones[1].x, 960),
            _ => panic!("expected canvas"),
        }
        // round-trip through our serializer keeps the FancyZones format
        let v = serde_json::to_value(&l[1]).unwrap();
        assert_eq!(v["type"], "canvas");
        assert_eq!(v["info"]["ref-width"], 1920);
        assert_eq!(v["info"]["zones"][0]["X"], 0);
    }

    #[test]
    fn import_applied_layouts_both_formats() {
        let e = parse_fancyzones_applied_layouts(FZ_APPLIED).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].0.monitor, "LEN4123");
        assert_eq!(e[0].1.kind, LayoutType::PriorityGrid);
        assert_eq!(e[1].0.monitor, "DELL");
        assert_eq!(e[1].1.kind, LayoutType::Custom);
        assert_eq!(e[1].1.sensitivity_radius, 20); // default when missing
    }

    #[test]
    fn config_import_and_compute() {
        let mut c = Config::default();
        assert_eq!(c.import_fancyzones_custom_layouts(FZ_CUSTOM).unwrap(), 2);
        c.import_fancyzones_applied_layouts(FZ_APPLIED, |d| (d.monitor == "DELL").then(|| "mac-display-1".to_string()))
            .unwrap();
        let ap = c.layout_for("mac-display-1");
        assert_eq!(ap.kind, LayoutType::Custom);
        let l = Layout::new(&ap, Rect::new(0, 0, 1920, 1080), &c.custom_layouts).unwrap();
        assert_eq!(l.zones.len(), 4);
        assert_eq!(l.zones[0], Rect::new(0, 0, 639, 1080));
        // unknown monitor -> default layout
        assert_eq!(c.layout_for("other").kind, LayoutType::PriorityGrid);
        // re-import replaces by uuid instead of duplicating
        c.import_fancyzones_custom_layouts(FZ_CUSTOM).unwrap();
        assert_eq!(c.custom_layouts.len(), 2);
    }

    #[test]
    fn config_roundtrip_and_file() {
        let mut c = Config::default();
        c.applied_layouts.insert("m1".into(), AppliedLayout::template(LayoutType::Columns, 2));
        c.settings.shift_drag = false;
        c.app_zone_history.insert(
            "/Applications/Safari.app".into(),
            AppZoneHistoryEntry { monitor: "m1".into(), layout_uuid: "x".into(), layout_kind: LayoutType::Columns, zones: vec![1] },
        );
        let s = c.to_json().unwrap();
        assert_eq!(Config::from_json(&s).unwrap(), c);
        // partial config uses defaults
        let p = Config::from_json(r#"{"settings": {"shift_drag": false}}"#).unwrap();
        assert!(!p.settings.shift_drag);
        assert_eq!(p.settings.zone_highlight_color, "#008CFF");

        let dir = std::env::temp_dir().join(format!("fz-test-{}", std::process::id()));
        let path = dir.join("sub").join("fancyzones.json");
        c.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), c);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_formats_reimport() {
        let mut c = Config::default();
        c.import_fancyzones_custom_layouts(FZ_CUSTOM).unwrap();
        c.applied_layouts.insert("m1".into(), AppliedLayout::template(LayoutType::Grid, 6));
        let custom = c.export_fancyzones_custom_layouts().unwrap();
        assert_eq!(parse_fancyzones_custom_layouts(&custom).unwrap(), c.custom_layouts);
        let applied = c.export_fancyzones_applied_layouts().unwrap();
        let e = parse_fancyzones_applied_layouts(&applied).unwrap();
        assert_eq!(e[0].0.monitor, "m1");
        assert_eq!(e[0].1.zone_count, 6);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_hex_color("#008CFF"), Some((0, 0x8c, 0xff)));
        assert_eq!(parse_hex_color("#FF008CFF"), Some((0, 0x8c, 0xff)));
        assert_eq!(parse_hex_color("nope"), None);
    }

    #[test]
    fn default_path_location() {
        if let Some(p) = Config::default_path() {
            assert!(p.ends_with("PowerMac/fancyzones.json") || p.ends_with("PowerMac\\fancyzones.json"));
        }
    }
}
