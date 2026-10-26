//! Persistent configuration (settings, per-monitor applied layouts, custom
//! layouts, app zone history) plus import/export of FancyZones' own
//! `custom-layouts.json` / `applied-layouts.json` formats.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::edge_snap::{BottomEdgeAction, EdgeSnapConfig, TopEdgeAction};
use crate::keys::vk;
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

/// A hotkey: modifiers + a Windows virtual-key code (see [`crate::keys`];
/// the macOS backend translates the code to a `kVK_*` key code).
///
/// Serialized as `{"modifiers": {...}, "code": 192}`; a string such as
/// `"Ctrl+Option+Grave"` / `"Win+Shift+\u{60}"` is accepted when loading (see
/// [`Hotkey::from_str`](std::str::FromStr)). Modifier names: `Ctrl`/`Control`,
/// `Alt`/`Option`/`Opt`, `Shift`, `Win`/`Cmd`/`Command`/`Meta`/`Super`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    /// Windows virtual-key code (`VK_*`), e.g. `0xC0` for the backtick key.
    pub code: u16,
}

impl Hotkey {
    pub const fn new(modifiers: Modifiers, code: u16) -> Self {
        Self { modifiers, code }
    }

    /// `true` if `m` + `code` (a VK code) is exactly this hotkey.
    pub fn matches(&self, m: Modifiers, code: u16) -> bool {
        self.code != 0 && self.code == code && self.modifiers == m
    }

    /// Editor: Win+Shift+` (PowerToys) / Ctrl+Option+` on macOS (Cmd+Shift+`
    /// is the system's "previous window of the app").
    pub fn default_editor() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(Modifiers::new(true, true, false, false), vk::OEM_3)
        } else {
            Self::new(Modifiers::new(false, false, true, true), vk::OEM_3)
        }
    }

    /// Next window in zone: Win+PgDn / Ctrl+Option+] on macOS (laptops have no PgDn key).
    pub fn default_next_tab() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(Modifiers::new(true, true, false, false), vk::OEM_6)
        } else {
            Self::new(Modifiers::new(false, false, false, true), vk::NEXT)
        }
    }

    /// Previous window in zone: Win+PgUp / Ctrl+Option+[ on macOS.
    pub fn default_prev_tab() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(Modifiers::new(true, true, false, false), vk::OEM_4)
        } else {
            Self::new(Modifiers::new(false, false, false, true), vk::PRIOR)
        }
    }

    /// Monitor rotation chord (hold, then Left/Right): Alt+X / Ctrl+Option+X
    /// on macOS (Option+X types a character).
    pub fn default_monitor_rotation() -> Self {
        if cfg!(target_os = "macos") {
            Self::new(Modifiers::new(true, true, false, false), vk::KEY_X)
        } else {
            Self::new(Modifiers::new(false, true, false, false), vk::KEY_X)
        }
    }
}

impl std::fmt::Display for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mac = cfg!(target_os = "macos");
        let m = &self.modifiers;
        let mut parts: Vec<&str> = Vec::new();
        if m.ctrl {
            parts.push("Ctrl");
        }
        if m.alt {
            parts.push(if mac { "Option" } else { "Alt" });
        }
        if m.shift {
            parts.push("Shift");
        }
        if m.meta {
            parts.push(if mac { "Cmd" } else { "Win" });
        }
        let key = crate::keys::vk_name(self.code);
        parts.push(&key);
        f.write_str(&parts.join("+"))
    }
}

impl std::str::FromStr for Hotkey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut m = Modifiers::default();
        let mut code = None;
        // "+" itself can be the key ("Ctrl++"): split carefully.
        let s = s.trim();
        let (mods, key) = match s.strip_suffix("++") {
            Some(rest) => (rest, "+"),
            None => match s.rfind('+') {
                Some(i) => (&s[..i], &s[i + 1..]),
                None => ("", s),
            },
        };
        for p in mods.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match p.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => m.ctrl = true,
                "alt" | "option" | "opt" => m.alt = true,
                "shift" => m.shift = true,
                "win" | "cmd" | "command" | "meta" | "super" => m.meta = true,
                other => return Err(format!("unknown modifier '{other}'")),
            }
        }
        if key == "+" {
            code = Some(vk::OEM_PLUS);
        } else if !key.is_empty() {
            code = crate::keys::vk_from_name(key);
        }
        let code = code.ok_or_else(|| format!("unknown key '{key}'"))?;
        Ok(Hotkey { modifiers: m, code })
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Obj {
            modifiers: Modifiers,
            code: u16,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Obj(Obj),
        }
        match Repr::deserialize(d)? {
            Repr::Str(s) => s.parse().map_err(serde::de::Error::custom),
            Repr::Obj(o) => Ok(Hotkey { modifiers: o.modifiers, code: o.code }),
        }
    }
}

/// `LayoutNameLabelPlacement` (where the layout name chip is drawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutNameLabelPlacement {
    TopLeft,
    #[default]
    TopCenter,
    TopRight,
    Center,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl LayoutNameLabelPlacement {
    pub const ALL: [Self; 7] =
        [Self::TopLeft, Self::TopCenter, Self::TopRight, Self::Center, Self::BottomLeft, Self::BottomCenter, Self::BottomRight];

    /// PowerToys' integer value (`fancyzones_layoutNameLabelPlacement`).
    pub fn from_index(i: i64) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }

    /// Top-left of a `w`x`h` chip inside `area`, `mx`/`my` from the edges.
    pub fn position(self, area: crate::geometry::Rect, w: i32, h: i32, mx: i32, my: i32) -> crate::geometry::Point {
        use LayoutNameLabelPlacement::*;
        let cx = area.left + (area.width() - w) / 2;
        let left = area.left + mx;
        let right = area.right - mx - w;
        let top = area.top + my;
        let bottom = area.bottom - my - h;
        let cy = area.top + (area.height() - h) / 2;
        let (x, y) = match self {
            TopLeft => (left, top),
            TopCenter => (cx, top),
            TopRight => (right, top),
            Center => (cx, cy),
            BottomLeft => (left, bottom),
            BottomCenter => (cx, bottom),
            BottomRight => (right, bottom),
        };
        crate::geometry::Point::new(x, y)
    }
}

fn d_snap() -> Modifiers {
    Modifiers::default_snap()
}
fn d_extend() -> Modifiers {
    Modifiers::default_extend()
}

/// FancyZones `Settings` (see PowerToys `Settings.h`). Defaults match
/// FancyZones except `override_snap_hotkeys` (on by default here) and the
/// macOS hotkeys (see [`Hotkey`]'s `default_*` constructors / the crate docs).
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
    /// Restore a window's original size when it is dragged out of its zone
    /// (or out of an edge snap).
    pub restore_size: bool,
    /// Rectangle-style snapping: drag a window to a screen edge/corner (with
    /// zones not showing) to snap it to a half, quarter, third or maximize.
    /// Default on macOS; off on Windows, whose built-in Snap already does this.
    pub edge_snap_enabled: bool,
    /// Distance (px/points) from a monitor edge that triggers an edge snap.
    pub edge_snap_margin: i32,
    /// Distance from a corner (along either edge) that snaps to a quarter.
    pub edge_snap_corner_size: i32,
    pub edge_snap_top: TopEdgeAction,
    pub edge_snap_bottom: BottomEdgeAction,
    /// Gap around and between edge-snapped windows.
    pub edge_snap_gap: i32,
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

    // ---- mouse
    /// Middle click while dragging toggles multi-zone spanning (like holding
    /// Ctrl). When off, the middle button acts like the secondary button.
    pub mouse_middle_click_spanning_multiple_zones: bool,
    /// Mouse wheel while dragging (zones shown) cycles the layouts of the
    /// monitor under the cursor (layouts with quick-switch keys, or all
    /// custom layouts when fewer than two have keys).
    pub mouse_wheel_layout_switch: bool,
    /// Make the dragged window semi-transparent while zones are shown
    /// (Windows only; macOS has no API to change another app's window alpha).
    pub make_dragged_window_transparent: bool,

    // ---- window behaviour
    /// Move windows back into their zones when displays or work areas change.
    pub display_change_move_windows: bool,
    /// Flash the zones when a monitor's layout changes.
    pub zone_set_change_flash_zones: bool,
    /// Move zoned windows into the zone with the same index when a monitor's layout changes.
    pub zone_set_change_move_windows: bool,
    /// New windows without zone history open on the monitor under the cursor.
    pub open_window_on_active_monitor: bool,
    /// Ctrl+Win+Alt+0..9 (Ctrl+Option+Cmd+0..9 on macOS) applies the layout
    /// bound to that key in `Config::layout_hotkeys`.
    pub quick_layout_switch: bool,
    pub flash_zones_on_quick_switch: bool,
    /// Snap windows that have a (visible) owner — dialogs (macOS `AXDialog`).
    pub allow_snap_child_windows: bool,
    /// Snap popup windows without a caption/thick frame (macOS floating panels).
    pub allow_snap_popup_windows: bool,
    /// Windows 11: square corners on snapped windows (no-op elsewhere).
    pub disable_round_corners: bool,
    /// Apps never snapped or tracked: Windows exe names (`notepad.exe`, or a
    /// path suffix / prefix of the file name as in PowerToys, or a window-title
    /// substring); macOS bundle ids (`com.apple.Safari`), app names (`Safari`)
    /// or bundle file names (`Safari.app`). Case-insensitive.
    pub excluded_apps: Vec<String>,
    /// Keep a separate layout per virtual desktop (Windows) / Space (macOS,
    /// via the private `CGSManagedDisplayGetCurrentSpace`; falls back to one
    /// layout per monitor if unavailable).
    pub layouts_per_virtual_desktop: bool,

    // ---- layout name label (shown on quick / wheel layout switch)
    pub layout_name_label_enabled: bool,
    pub layout_name_label_placement: LayoutNameLabelPlacement,
    pub layout_name_label_text_color: String,
    pub layout_name_label_background_color: String,
    /// Points / DIPs.
    pub layout_name_label_font_size: i32,
    pub layout_name_label_padding: i32,
    pub layout_name_label_duration_ms: u32,

    // ---- hotkeys
    /// Fires `StartOptions::on_open_editor`.
    pub editor_hotkey: Hotkey,
    /// Cycle focus between windows snapped into the same zone(s).
    pub window_switching: bool,
    pub next_tab_hotkey: Hotkey,
    pub prev_tab_hotkey: Hotkey,
    /// Hold `monitor_rotation_hotkey` and press Left/Right to move all windows
    /// to the previous/next monitor.
    pub monitor_rotation: bool,
    pub monitor_rotation_hotkey: Hotkey,
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
            edge_snap_enabled: cfg!(not(windows)),
            edge_snap_margin: 5,
            edge_snap_corner_size: 20,
            edge_snap_top: TopEdgeAction::Maximize,
            edge_snap_bottom: BottomEdgeAction::BottomHalf,
            edge_snap_gap: 0,
            app_last_zone_move_windows: false,
            zone_color: "#AACDFF".into(),
            zone_border_color: "#FFFFFF".into(),
            zone_highlight_color: "#008CFF".into(),
            zone_number_color: "#000000".into(),
            zone_highlight_opacity: 50,
            overlapping_zones_algorithm: OverlappingZonesAlgorithm::Smallest,
            default_layout: AppliedLayout::default(),
            mouse_middle_click_spanning_multiple_zones: false,
            mouse_wheel_layout_switch: false,
            make_dragged_window_transparent: true,
            display_change_move_windows: true,
            zone_set_change_flash_zones: false,
            zone_set_change_move_windows: false,
            open_window_on_active_monitor: false,
            quick_layout_switch: true,
            flash_zones_on_quick_switch: true,
            allow_snap_child_windows: false,
            allow_snap_popup_windows: false,
            disable_round_corners: false,
            excluded_apps: Vec::new(),
            layouts_per_virtual_desktop: true,
            layout_name_label_enabled: true,
            layout_name_label_placement: LayoutNameLabelPlacement::TopCenter,
            layout_name_label_text_color: "#FFFFFF".into(),
            layout_name_label_background_color: "#262626".into(),
            layout_name_label_font_size: 36,
            layout_name_label_padding: 12,
            layout_name_label_duration_ms: 1500,
            editor_hotkey: Hotkey::default_editor(),
            window_switching: true,
            next_tab_hotkey: Hotkey::default_next_tab(),
            prev_tab_hotkey: Hotkey::default_prev_tab(),
            monitor_rotation: false,
            monitor_rotation_hotkey: Hotkey::default_monitor_rotation(),
        }
    }
}

impl Settings {
    pub fn edge_snap_config(&self) -> EdgeSnapConfig {
        EdgeSnapConfig {
            margin: self.edge_snap_margin,
            corner_size: self.edge_snap_corner_size,
            top: self.edge_snap_top,
            bottom: self.edge_snap_bottom,
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

/// Quick layout switch binding (FancyZones `layout-hotkeys.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutHotkey {
    /// 0..=9
    pub key: u8,
    /// Custom layout uuid.
    pub layout_uuid: String,
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
    /// Quick layout switch keys (FancyZones `layout-hotkeys.json`).
    #[serde(default)]
    pub layout_hotkeys: Vec<LayoutHotkey>,
    /// Customized template layouts (zone count / spacing / sensitivity per
    /// template type, FancyZones `layout-templates.json`); see [`Config::template`].
    #[serde(default)]
    pub layout_templates: Vec<AppliedLayout>,
    /// Virtual desktop / Space id -> monitor id -> layout. Used when
    /// `settings.layouts_per_virtual_desktop` is on; a desktop without an entry
    /// falls back to `applied_layouts` (which always holds the layout applied
    /// last, like FancyZones cloning the last used desktop's layout).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub virtual_desktop_layouts: BTreeMap<String, BTreeMap<String, AppliedLayout>>,
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

    /// Applied layout for a monitor on virtual desktop `vd` (honours
    /// `layouts_per_virtual_desktop`).
    pub fn layout_for_desktop(&self, monitor_id: &str, vd: Option<&str>) -> AppliedLayout {
        if let (true, Some(vd)) = (self.settings.layouts_per_virtual_desktop, vd) {
            if let Some(l) = self.virtual_desktop_layouts.get(vd).and_then(|m| m.get(monitor_id)) {
                return l.clone();
            }
        }
        self.layout_for(monitor_id)
    }

    /// Store `layout` for a monitor: always as the monitor's layout, and also
    /// for virtual desktop `vd` when per-desktop layouts are on.
    pub fn set_layout(&mut self, monitor_id: &str, vd: Option<&str>, layout: AppliedLayout) {
        if let (true, Some(vd)) = (self.settings.layouts_per_virtual_desktop, vd) {
            self.virtual_desktop_layouts.entry(vd.to_string()).or_default().insert(monitor_id.to_string(), layout.clone());
        }
        self.applied_layouts.insert(monitor_id.to_string(), layout);
    }

    /// Template layout of `kind` with the user's customizations from
    /// `layout_templates` (or FancyZones' defaults).
    pub fn template(&self, kind: crate::layout::LayoutType) -> AppliedLayout {
        self.layout_templates.iter().find(|t| t.kind == kind).cloned().map(|mut t| {
            t.uuid = crate::layout::NULL_GUID.into();
            t
        }).unwrap_or_else(|| {
            let n = match kind {
                crate::layout::LayoutType::Grid => 4,
                crate::layout::LayoutType::Blank => 0,
                _ => crate::layout::defaults::ZONE_COUNT,
            };
            AppliedLayout::template(kind, n)
        })
    }

    /// Custom layout uuid bound to quick-switch key `key`.
    pub fn layout_for_hotkey(&self, key: u8) -> Option<&str> {
        self.layout_hotkeys.iter().find(|h| h.key == key).map(|h| h.layout_uuid.as_str())
    }

    /// Bind (or with `None` unbind) a quick-switch key.
    pub fn set_layout_hotkey(&mut self, key: u8, layout_uuid: Option<String>) {
        self.layout_hotkeys.retain(|h| h.key != key);
        if let Some(u) = layout_uuid {
            self.layout_hotkeys.retain(|h| !crate::layout::guid_eq(&h.layout_uuid, &u));
            self.layout_hotkeys.push(LayoutHotkey { key, layout_uuid: u });
            self.layout_hotkeys.sort_by_key(|h| h.key);
        }
    }

    /// Merge FancyZones' `layout-hotkeys.json` (`{"layout-hotkeys": [{"key": 0, "layout-id": "{...}"}]}`).
    pub fn import_fancyzones_layout_hotkeys(&mut self, json: &str) -> Result<usize> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let arr = v.get("layout-hotkeys").and_then(|a| a.as_array()).context("missing 'layout-hotkeys' array")?;
        let mut n = 0;
        for item in arr {
            let key = item.get("key").and_then(|k| k.as_i64());
            let id = item.get("layout-id").and_then(|k| k.as_str());
            match (key, id) {
                (Some(k @ 0..=9), Some(id)) => {
                    self.set_layout_hotkey(k as u8, Some(id.to_string()));
                    n += 1;
                }
                _ => log::warn!("skipping layout hotkey {item}"),
            }
        }
        Ok(n)
    }

    pub fn export_fancyzones_layout_hotkeys(&self) -> Result<String> {
        let arr: Vec<serde_json::Value> = self
            .layout_hotkeys
            .iter()
            .map(|h| serde_json::json!({"key": h.key, "layout-id": h.layout_uuid}))
            .collect();
        Ok(serde_json::to_string_pretty(&serde_json::json!({ "layout-hotkeys": arr }))?)
    }

    /// Merge FancyZones' `layout-templates.json`
    /// (`{"layout-templates": [{"type": "columns", "zone-count": 3, ...}]}`).
    pub fn import_fancyzones_layout_templates(&mut self, json: &str) -> Result<usize> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let arr = v.get("layout-templates").and_then(|a| a.as_array()).context("missing 'layout-templates' array")?;
        let mut n = 0;
        for item in arr {
            match serde_json::from_value::<AppliedLayout>(item.clone()) {
                Ok(mut t) if t.kind != crate::layout::LayoutType::Custom => {
                    t.uuid = crate::layout::NULL_GUID.into();
                    self.layout_templates.retain(|x| x.kind != t.kind);
                    self.layout_templates.push(t);
                    n += 1;
                }
                Ok(_) => log::warn!("skipping custom entry in layout templates"),
                Err(e) => log::warn!("skipping layout template: {e}"),
            }
        }
        Ok(n)
    }

    pub fn export_fancyzones_layout_templates(&self) -> Result<String> {
        let arr: Vec<serde_json::Value> = self
            .layout_templates
            .iter()
            .map(|t| {
                serde_json::json!({"type": t.kind, "show-spacing": t.show_spacing, "spacing": t.spacing,
                    "zone-count": t.zone_count, "sensitivity-radius": t.sensitivity_radius})
            })
            .collect();
        Ok(serde_json::to_string_pretty(&serde_json::json!({ "layout-templates": arr }))?)
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

    #[test]
    fn new_settings_defaults_and_old_json() {
        let s = Config::from_json(r#"{"settings": {"shift_drag": false, "restore_size": true}}"#).unwrap().settings;
        assert!(s.display_change_move_windows && s.quick_layout_switch && s.flash_zones_on_quick_switch);
        assert!(s.make_dragged_window_transparent && s.window_switching && s.layout_name_label_enabled);
        assert!(!s.zone_set_change_move_windows && !s.zone_set_change_flash_zones && !s.monitor_rotation);
        assert!(!s.mouse_middle_click_spanning_multiple_zones && !s.mouse_wheel_layout_switch);
        assert!(!s.open_window_on_active_monitor && !s.allow_snap_child_windows && !s.allow_snap_popup_windows);
        assert!(!s.disable_round_corners && s.excluded_apps.is_empty() && s.layouts_per_virtual_desktop);
        assert_eq!(s.layout_name_label_placement, LayoutNameLabelPlacement::TopCenter);
        assert_eq!(s.layout_name_label_text_color, "#FFFFFF");
        assert_eq!(s.layout_name_label_background_color, "#262626");
        assert_eq!((s.layout_name_label_font_size, s.layout_name_label_padding, s.layout_name_label_duration_ms), (36, 12, 1500));
        assert_eq!(s.editor_hotkey, Hotkey::default_editor());
        assert_eq!(s.monitor_rotation_hotkey.code, vk::KEY_X);
        let c = Config::from_json("{}").unwrap();
        assert!(c.layout_hotkeys.is_empty() && c.layout_templates.is_empty() && c.virtual_desktop_layouts.is_empty());
        // and everything round-trips
        let mut c = Config::default();
        c.settings.excluded_apps = vec!["com.apple.Safari".into()];
        c.settings.layout_name_label_placement = LayoutNameLabelPlacement::BottomLeft;
        c.layout_hotkeys.push(LayoutHotkey { key: 1, layout_uuid: "{X}".into() });
        assert_eq!(Config::from_json(&c.to_json().unwrap()).unwrap(), c);
        let v: serde_json::Value = serde_json::from_str(&c.to_json().unwrap()).unwrap();
        assert_eq!(v["settings"]["layout_name_label_placement"], "bottom-left");
        assert_eq!(v["layout_hotkeys"][0]["layout_uuid"], "{X}");
        assert!(v.get("virtual_desktop_layouts").is_none());
    }

    #[test]
    fn hotkey_formats() {
        let h: Hotkey = "Win+Shift+`".parse().unwrap();
        assert_eq!(h, Hotkey::new(Modifiers::new(false, false, true, true), vk::OEM_3));
        let h: Hotkey = "ctrl+option+grave".parse().unwrap();
        assert_eq!(h, Hotkey::new(Modifiers::new(true, true, false, false), vk::OEM_3));
        assert_eq!("Cmd+PageDown".parse::<Hotkey>().unwrap().code, vk::NEXT);
        assert_eq!("Ctrl++".parse::<Hotkey>().unwrap().code, vk::OEM_PLUS);
        assert!("Hyper+X".parse::<Hotkey>().is_err());
        assert!("Ctrl+Nope".parse::<Hotkey>().is_err());
        let h = Hotkey::new(Modifiers::new(true, true, true, true), vk::KEY_X);
        assert_eq!(h.to_string().parse::<Hotkey>().unwrap(), h);
        // JSON: object form, string form
        let j = serde_json::to_value(h).unwrap();
        assert_eq!(j["code"], 0x58);
        assert_eq!(j["modifiers"]["meta"], true);
        assert_eq!(serde_json::from_value::<Hotkey>(j).unwrap(), h);
        let s: Settings = serde_json::from_str(r#"{"next_tab_hotkey": "Ctrl+Alt+]", "prev_tab_hotkey": {"code": 33}}"#).unwrap();
        assert_eq!(s.next_tab_hotkey, Hotkey::new(Modifiers::new(true, true, false, false), vk::OEM_6));
        assert_eq!(s.prev_tab_hotkey, Hotkey::new(Modifiers::default(), vk::PRIOR));
        assert!(serde_json::from_str::<Settings>(r#"{"editor_hotkey": "Ctrl+Bogus"}"#).is_err());
        assert!(h.matches(Modifiers::new(true, true, true, true), vk::KEY_X));
        assert!(!h.matches(Modifiers::new(true, true, true, false), vk::KEY_X));
        assert!(!Hotkey::default().matches(Modifiers::default(), 0));
    }

    #[test]
    fn import_layout_hotkeys_and_templates() {
        let mut c = Config::default();
        let hk = r#"{"layout-hotkeys": [{"key": 0, "layout-id": "{A}"}, {"key": 9, "layout-id": "{B}"}, {"key": 12, "layout-id": "{C}"}]}"#;
        assert_eq!(c.import_fancyzones_layout_hotkeys(hk).unwrap(), 2);
        assert_eq!(c.layout_for_hotkey(9), Some("{B}"));
        // rebinding a layout moves its key
        c.set_layout_hotkey(4, Some("{a}".into()));
        assert_eq!(c.layout_for_hotkey(0), None);
        assert_eq!(c.layout_for_hotkey(4), Some("{a}"));
        c.set_layout_hotkey(9, None);
        assert_eq!(c.layout_hotkeys.len(), 1);
        let mut d = Config::default();
        d.import_fancyzones_layout_hotkeys(&c.export_fancyzones_layout_hotkeys().unwrap()).unwrap();
        assert_eq!(d.layout_hotkeys, c.layout_hotkeys);

        let t = r#"{"layout-templates": [
            {"type": "blank", "show-spacing": false, "spacing": 0, "zone-count": 0, "sensitivity-radius": 0},
            {"type": "columns", "show-spacing": true, "spacing": 8, "zone-count": 4, "sensitivity-radius": 30},
            {"type": "custom"}, {"type": "bogus"}]}"#;
        assert_eq!(c.import_fancyzones_layout_templates(t).unwrap(), 2);
        let col = c.template(crate::layout::LayoutType::Columns);
        assert_eq!((col.zone_count, col.spacing, col.sensitivity_radius), (4, 8, 30));
        assert_eq!(c.template(crate::layout::LayoutType::Grid).zone_count, 4);
        assert_eq!(c.template(crate::layout::LayoutType::Rows).zone_count, 3);
        let mut d = Config::default();
        d.import_fancyzones_layout_templates(&c.export_fancyzones_layout_templates().unwrap()).unwrap();
        assert_eq!(d.layout_templates, c.layout_templates);
    }

    #[test]
    fn label_placement_index() {
        assert_eq!(LayoutNameLabelPlacement::from_index(0), LayoutNameLabelPlacement::TopLeft);
        assert_eq!(LayoutNameLabelPlacement::from_index(6), LayoutNameLabelPlacement::BottomRight);
        assert_eq!(LayoutNameLabelPlacement::from_index(42), LayoutNameLabelPlacement::TopCenter);
    }
}
