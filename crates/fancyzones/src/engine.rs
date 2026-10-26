//! Platform independent FancyZones engine: work areas, window zone
//! assignment, keyboard snapping, drag sessions and "last zone" history.
//! The OS specific parts are behind the [`WindowSystem`] trait.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{AppZoneHistoryEntry, Config, LayoutNameLabelPlacement};
use crate::edge_snap::{self, EdgeAction};
use crate::filters::{self, AppIdentity, WindowKind};
use crate::geometry::{Point, Rect};
use crate::layout::{AppliedLayout, Layout, LayoutType};
use crate::rotation;
use crate::navigation::{self, Direction, ExtendState, SnapTarget};

/// Opaque, stable-while-alive identifier of a top-level window.
pub type WindowKey = u64;

/// Id used for the virtual work area when `span_zones_across_monitors` is on.
pub const SPAN_MONITOR_ID: &str = "span-all-monitors";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    /// Stable id used as the key of `Config::applied_layouts`.
    pub id: String,
    pub name: String,
    /// Full monitor bounds (global, top-left origin).
    pub frame: Rect,
    /// Usable area (excludes menu bar / Dock / taskbar).
    pub work_area: Rect,
    /// Backing scale factor / DPI scale (informational).
    pub scale: f64,
    pub primary: bool,
}

/// OS window management backend. Only the first five methods are required;
/// the others have conservative defaults (features needing them are then
/// inactive).
pub trait WindowSystem {
    type Window;
    fn monitors(&self) -> Result<Vec<Monitor>>;
    fn focused_window(&self) -> Result<Option<Self::Window>>;
    fn window_key(&self, w: &Self::Window) -> WindowKey;
    /// Visible frame of the window (global coordinates).
    fn window_rect(&self, w: &Self::Window) -> Result<Rect>;
    /// Move/resize so that the *visible* frame becomes `r`.
    fn set_window_rect(&self, w: &Self::Window, r: Rect) -> Result<()>;
    /// Identifier of the owning application (path / bundle), for "last zone".
    fn window_app_id(&self, _w: &Self::Window) -> Option<String> {
        None
    }
    /// App identity for `excluded_apps` matching.
    fn window_identity(&self, w: &Self::Window) -> AppIdentity {
        AppIdentity::from_path(self.window_app_id(w))
    }
    /// Window classification for the child/popup filters.
    fn window_kind(&self, _w: &Self::Window) -> WindowKind {
        WindowKind::Standard
    }
    /// Look a window up again by its key (`None` if it no longer exists).
    fn window_from_key(&self, _key: WindowKey) -> Option<Self::Window> {
        None
    }
    /// Activate / focus a window.
    fn focus_window(&self, _w: &Self::Window) -> Result<()> {
        anyhow::bail!("focusing windows is not supported by this window system")
    }
    /// All top-level windows on the current virtual desktop (for monitor rotation).
    fn all_windows(&self) -> Result<Vec<Self::Window>> {
        Ok(vec![])
    }
    /// Current virtual desktop / Space id per monitor id (empty if unknown).
    fn virtual_desktops(&self, _monitors: &[Monitor]) -> HashMap<String, String> {
        HashMap::new()
    }
    /// Called after a window was snapped (`true`) or unsnapped (`false`)
    /// when `disable_round_corners` is on (Windows 11 corner preference).
    fn set_snapped_style(&self, _w: &Self::Window, _snapped: bool) {}
}

/// A monitor with its instantiated layout.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkArea {
    pub monitor: Monitor,
    pub applied: AppliedLayout,
    pub layout: Layout,
    /// Frames of the physical monitors this work area covers (one entry, or
    /// all monitors when zones span across monitors).
    pub physical_frames: Vec<Rect>,
}

/// Where a window is currently snapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub monitor_id: String,
    pub layout_uuid: String,
    pub layout_kind: LayoutType,
    pub zones: Vec<usize>,
    /// Insertion order, for cycling between windows of the same zone(s).
    pub order: u64,
}

/// Layout name label to draw (PowerToys `ZonesOverlay::ShowLayoutName`).
#[derive(Debug, Clone, PartialEq)]
pub struct LabelState {
    /// Work area the label is placed in (global coordinates).
    pub area: Rect,
    pub text: String,
    pub placement: LayoutNameLabelPlacement,
    pub text_color: (u8, u8, u8),
    pub background_color: (u8, u8, u8),
    pub font_size: i32,
    pub padding: i32,
    pub duration_ms: u32,
}

impl LabelState {
    /// Top-left of a `w`x`h` chip, with PowerToys' margins (2% of the work
    /// area width from the sides, 8% of its height from the top/bottom).
    pub fn chip_origin(&self, w: i32, h: i32) -> Point {
        let mx = (self.area.width() as f64 * 0.02).round() as i32;
        let my = (self.area.height() as f64 * 0.08).round() as i32;
        self.placement.position(self.area, w, h, mx, my)
    }
}

/// How long flashed zones stay visible (`FlashZonesDurationMillis`).
pub const FLASH_ZONES_MS: u64 = 700;

/// Visual feedback after a layout switch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SwitchFeedback {
    /// Flash these zones (all work areas) for [`FLASH_ZONES_MS`].
    pub flash: Option<OverlayState>,
    pub label: Option<LabelState>,
}

/// Highlight information for the overlay of one monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayMonitor {
    pub frame: Rect,
    pub zones: Vec<Rect>,
    pub highlighted: Vec<usize>,
}

/// What the overlay should draw (empty `monitors` = hide).
///
/// An edge-snap preview is drawn as a single highlighted "zone" on its
/// monitor (so the platform overlays need no special case); `edge_preview`
/// tells consumers that this is what's shown.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OverlayState {
    pub monitors: Vec<OverlayMonitor>,
    pub edge_preview: Option<Rect>,
    pub show_numbers: bool,
    pub zone_color: (u8, u8, u8),
    pub border_color: (u8, u8, u8),
    pub highlight_color: (u8, u8, u8),
    pub number_color: (u8, u8, u8),
    pub opacity: f64,
}

/// Modifier/mouse state sampled during a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DragInput {
    pub cursor: Point,
    pub shift: bool,
    /// Select-many-zones key (Ctrl) held.
    pub ctrl: bool,
}

/// An active window drag (`WindowMouseSnap` + `HighlightedZones`).
#[derive(Debug, Clone, PartialEq)]
pub struct DragSession {
    pub window: WindowKey,
    pub start_rect: Rect,
    pub work_area: Option<usize>,
    pub highlighted: Vec<usize>,
    initial_highlight: Vec<usize>,
    secondary_toggle: bool,
    /// Middle-click multi-zone spanning toggle.
    multi_toggle: bool,
    pub zones_active: bool,
    /// Pending edge snap (cursor at a screen edge, zones not showing).
    pub edge: Option<(EdgeAction, Rect)>,
}

/// Outcome of [`Engine::drag_end`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragResult {
    /// Snapped into these zones.
    Zones(Vec<usize>),
    /// Snapped to a screen-edge target.
    Edge(EdgeAction, Rect),
    /// Plain move (unsnapped if it was snapped).
    Moved,
}

pub struct Engine {
    pub config: Config,
    assignments: HashMap<WindowKey, Assignment>,
    /// Size before the window was first snapped (for `restore_size`).
    pre_snap_size: HashMap<WindowKey, (i32, i32)>,
    /// Windows currently placed by an edge snap.
    edge_snapped: HashSet<WindowKey>,
    extend: Option<ExtendState>,
    pub drag: Option<DragSession>,
    next_order: u64,
    /// Monitor id -> current virtual desktop / Space id.
    virtual_desktops: HashMap<String, String>,
    /// Windows already handled by [`Engine::on_window_created`].
    seen_new: HashSet<WindowKey>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            assignments: HashMap::new(),
            pre_snap_size: HashMap::new(),
            edge_snapped: HashSet::new(),
            extend: None,
            drag: None,
            next_order: 0,
            virtual_desktops: HashMap::new(),
            seen_new: HashSet::new(),
        }
    }

    /// Set the current virtual desktop per monitor (platforms call this via
    /// [`Engine::areas`]); the span work area uses the primary monitor's.
    pub fn set_virtual_desktops(&mut self, mut map: HashMap<String, String>, monitors: &[Monitor]) {
        let primary = monitors.iter().find(|m| m.primary).or(monitors.first());
        if let Some(vd) = primary.and_then(|m| map.get(&m.id)).cloned() {
            map.insert(SPAN_MONITOR_ID.into(), vd);
        }
        self.virtual_desktops = map;
    }

    pub fn virtual_desktop(&self, monitor_id: &str) -> Option<&str> {
        self.virtual_desktops.get(monitor_id).map(String::as_str)
    }

    /// Monitors + current virtual desktops from `ws`, instantiated as work areas.
    pub fn areas<WS: WindowSystem>(&mut self, ws: &WS) -> Result<Vec<WorkArea>> {
        let monitors = ws.monitors()?;
        let vds = ws.virtual_desktops(&monitors);
        self.set_virtual_desktops(vds, &monitors);
        Ok(self.work_areas(&monitors))
    }

    /// `FancyZonesWindowProcessing::IsProcessableManually` + exclusion list.
    pub fn is_processable<WS: WindowSystem>(&self, ws: &WS, w: &WS::Window) -> bool {
        let kind = ws.window_kind(w);
        if kind == WindowKind::Never {
            return false;
        }
        filters::is_processable(&self.config.settings, kind, &ws.window_identity(w))
    }

    /// Work areas in FancyZones monitor order.
    pub fn work_areas(&self, monitors: &[Monitor]) -> Vec<WorkArea> {
        let s = &self.config.settings;
        if s.span_zones_across_monitors && !monitors.is_empty() {
            let wa = monitors.iter().map(|m| m.work_area).reduce(|a, b| a.union(&b)).unwrap();
            let fr = monitors.iter().map(|m| m.frame).reduce(|a, b| a.union(&b)).unwrap();
            let m = Monitor {
                id: SPAN_MONITOR_ID.into(),
                name: "All monitors".into(),
                frame: fr,
                work_area: wa,
                scale: 1.0,
                primary: true,
            };
            let frames = monitors.iter().map(|m| m.frame).collect();
            return vec![self.make_work_area(m, frames)];
        }
        let rects: Vec<Rect> = monitors.iter().map(|m| m.work_area).collect();
        navigation::order_monitors(&rects)
            .into_iter()
            .map(|i| self.make_work_area(monitors[i].clone(), vec![monitors[i].frame]))
            .collect()
    }

    fn make_work_area(&self, monitor: Monitor, physical_frames: Vec<Rect>) -> WorkArea {
        let applied = self.config.layout_for_desktop(&monitor.id, self.virtual_desktop(&monitor.id));
        let layout = match Layout::new(&applied, monitor.work_area, &self.config.custom_layouts) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("layout for monitor {} failed: {e}", monitor.id);
                Layout::blank(monitor.work_area)
            }
        };
        WorkArea { monitor, applied, layout, physical_frames }
    }

    /// Store `layout` for a monitor (and its current virtual desktop). Call
    /// [`Engine::on_layout_changed`] afterwards to move / unassign windows.
    pub fn apply_layout(&mut self, monitor_id: &str, layout: AppliedLayout) {
        let vd = self.virtual_desktop(monitor_id).map(str::to_string);
        self.config.set_layout(monitor_id, vd.as_deref(), layout);
    }

    /// The layout of `monitor_id` changed (`RefreshLayouts`): with
    /// `zone_set_change_move_windows` windows snapped there move into the
    /// zones with the same indices of the new layout, otherwise they are
    /// unassigned. Returns the number of windows moved.
    pub fn on_layout_changed<WS: WindowSystem>(&mut self, ws: &WS, monitor_id: &str) -> Result<usize> {
        let areas = self.areas(ws)?;
        let dragged = self.drag.as_ref().map(|d| d.window);
        let keys: Vec<WindowKey> = self
            .assignments
            .iter()
            .filter(|(k, a)| a.monitor_id == monitor_id && Some(**k) != dragged)
            .map(|(k, _)| *k)
            .collect();
        let wa = areas.iter().position(|a| a.monitor.id == monitor_id);
        let move_windows = self.config.settings.zone_set_change_move_windows;
        let mut moved = 0;
        for key in keys {
            let (Some(wa), true) = (wa, move_windows) else {
                self.assignments.remove(&key);
                continue;
            };
            let n = areas[wa].layout.zones.len();
            let zones: Vec<usize> = self.assignments[&key].zones.iter().copied().filter(|&z| z < n).collect();
            if zones.is_empty() {
                self.assignments.remove(&key);
                continue;
            }
            let Some(w) = ws.window_from_key(key) else {
                self.forget_window(key);
                continue;
            };
            match self.place_window(ws, &w, &areas, wa, &zones, false) {
                Ok(true) => moved += 1,
                Ok(false) => {}
                Err(e) => {
                    log::debug!("fancyzones: moving window after layout change failed: {e:#}");
                    self.assignments.remove(&key);
                }
            }
        }
        Ok(moved)
    }

    /// Apply a layout chosen by a quick switch (hotkey / wheel) or the API and
    /// produce the flash + name label feedback (`FancyZones::SwitchLayout`).
    pub fn switch_layout<WS: WindowSystem>(&mut self, ws: &WS, monitor_id: &str, layout: AppliedLayout) -> Result<SwitchFeedback> {
        self.apply_layout(monitor_id, layout.clone());
        self.on_layout_changed(ws, monitor_id)?;
        let areas = self.areas(ws)?;
        let s = &self.config.settings;
        let flash = (s.flash_zones_on_quick_switch && self.drag.is_none()).then(|| self.flash_overlay(&areas));
        let label = match (layout.kind, areas.iter().find(|a| a.monitor.id == monitor_id)) {
            (LayoutType::Custom, Some(a)) => crate::layout::find_custom(&self.config.custom_layouts, &layout.uuid)
                .and_then(|c| self.label_state(a.monitor.work_area, &c.name)),
            _ => None,
        };
        Ok(SwitchFeedback { flash, label })
    }

    /// Quick layout switch (Ctrl+Win+Alt+`key`): apply the layout bound to
    /// `key` on the work area under `cursor`.
    pub fn apply_quick_layout<WS: WindowSystem>(&mut self, ws: &WS, key: u8, cursor: Point) -> Result<Option<SwitchFeedback>> {
        if !self.config.settings.quick_layout_switch {
            return Ok(None);
        }
        let Some(layout) = self.quick_layout(key) else { return Ok(None) };
        let areas = self.areas(ws)?;
        let Some(wa) = Self::work_area_for_point(&areas, cursor) else { return Ok(None) };
        let id = areas[wa].monitor.id.clone();
        self.switch_layout(ws, &id, layout).map(Some)
    }

    /// The applied layout bound to quick-switch key `key` (custom layouts only).
    pub fn quick_layout(&self, key: u8) -> Option<AppliedLayout> {
        let uuid = self.config.layout_for_hotkey(key)?;
        crate::layout::find_custom(&self.config.custom_layouts, uuid).map(AppliedLayout::custom)
    }

    /// Layout ids the mouse wheel cycles through (`WheelCycleLayoutIds`).
    pub fn wheel_cycle_ids(&self) -> Vec<String> {
        let custom = &self.config.custom_layouts;
        let mut hk: Vec<_> = self.config.layout_hotkeys.iter().collect();
        hk.sort_by_key(|h| h.key);
        let ids: Vec<String> = hk
            .into_iter()
            .filter(|h| crate::layout::find_custom(custom, &h.layout_uuid).is_some())
            .map(|h| h.layout_uuid.clone())
            .collect();
        if ids.len() >= 2 {
            ids
        } else {
            custom.iter().map(|c| c.uuid.clone()).collect()
        }
    }

    /// The layout the wheel would switch `monitor_id` to (`NextWheelLayout`).
    pub fn next_wheel_layout(&self, monitor_id: &str, reverse: bool) -> Option<AppliedLayout> {
        let ids = self.wheel_cycle_ids();
        let cur = self.config.layout_for_desktop(monitor_id, self.virtual_desktop(monitor_id));
        let next = rotation::pick_next_layout_id(&ids, Some(&cur.uuid), reverse)?;
        crate::layout::find_custom(&self.config.custom_layouts, next).map(AppliedLayout::custom)
    }

    /// Mouse wheel during a drag with zones shown: cycle the layout of the
    /// work area under the cursor (wheel up = backwards). The caller must
    /// re-instantiate its work areas and call [`Engine::drag_invalidate`].
    pub fn drag_wheel<WS: WindowSystem>(&mut self, ws: &WS, areas: &[WorkArea], up: bool) -> Result<Option<SwitchFeedback>> {
        if !self.config.settings.mouse_wheel_layout_switch {
            return Ok(None);
        }
        let Some(d) = &self.drag else { return Ok(None) };
        let (true, Some(wa)) = (d.zones_active, d.work_area) else { return Ok(None) };
        let Some(area) = areas.get(wa) else { return Ok(None) };
        let id = area.monitor.id.clone();
        let Some(layout) = self.next_wheel_layout(&id, up) else { return Ok(None) };
        self.switch_layout(ws, &id, layout).map(Some)
    }

    /// Overlay showing all zones of all work areas (`WorkArea::FlashZones`).
    pub fn flash_overlay(&self, areas: &[WorkArea]) -> OverlayState {
        let mut st = self.overlay_base();
        for a in areas {
            Self::push_area(&mut st, a, vec![]);
        }
        st
    }

    /// Layout name label for `area`, if enabled.
    pub fn label_state(&self, area: Rect, text: &str) -> Option<LabelState> {
        let s = &self.config.settings;
        if !s.layout_name_label_enabled || text.is_empty() {
            return None;
        }
        let c = |h: &str, d: (u8, u8, u8)| crate::config::parse_hex_color(h).unwrap_or(d);
        Some(LabelState {
            area,
            text: text.to_string(),
            placement: s.layout_name_label_placement,
            text_color: c(&s.layout_name_label_text_color, (0xFF, 0xFF, 0xFF)),
            background_color: c(&s.layout_name_label_background_color, (38, 38, 38)),
            font_size: s.layout_name_label_font_size.clamp(6, 200),
            padding: s.layout_name_label_padding.clamp(0, 200),
            duration_ms: s.layout_name_label_duration_ms.max(300),
        })
    }

    /// Displays or work areas changed: with `display_change_move_windows`,
    /// move every assigned window back into its zones (on its monitor, or on
    /// the monitor it is now on if that one has the same layout).
    pub fn on_display_change<WS: WindowSystem>(&mut self, ws: &WS) -> Result<usize> {
        let areas = self.areas(ws)?;
        if !self.config.settings.display_change_move_windows {
            return Ok(0);
        }
        let dragged = self.drag.as_ref().map(|d| d.window);
        let keys: Vec<WindowKey> = self.assignments.keys().copied().filter(|k| Some(*k) != dragged).collect();
        let mut moved = 0;
        for key in keys {
            let Some(w) = ws.window_from_key(key) else {
                self.forget_window(key);
                continue;
            };
            let a = self.assignments[&key].clone();
            let matches = |area: &WorkArea| {
                area.layout.uuid == a.layout_uuid
                    && area.layout.kind == a.layout_kind
                    && a.zones.iter().all(|&z| z < area.layout.zones.len())
            };
            let target = match areas.iter().position(|x| x.monitor.id == a.monitor_id) {
                Some(i) if matches(&areas[i]) => Some(i),
                Some(_) => None,
                None => ws.window_rect(&w).ok().and_then(|r| Self::work_area_for_rect(&areas, r)).filter(|&i| matches(&areas[i])),
            };
            match target {
                Some(i) => match self.place_window(ws, &w, &areas, i, &a.zones, false) {
                    Ok(true) => moved += 1,
                    Ok(false) => {}
                    Err(e) => log::debug!("fancyzones: re-snapping after display change failed: {e:#}"),
                },
                None => {
                    self.assignments.remove(&key);
                }
            }
        }
        Ok(moved)
    }

    /// A new window appeared (`FancyZones::WindowCreated`): move it to its
    /// app's last zone (`app_last_zone_move_windows`) or onto the monitor
    /// under the cursor (`open_window_on_active_monitor`). Each window is
    /// handled once. Returns `true` if the window was moved.
    pub fn on_window_created<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window, cursor: Option<Point>) -> Result<bool> {
        let s = &self.config.settings;
        let (last_zone, active_monitor) = (s.app_last_zone_move_windows, s.open_window_on_active_monitor);
        if !last_zone && !active_monitor {
            return Ok(false);
        }
        let key = ws.window_key(w);
        if self.seen_new.len() > 50_000 {
            self.seen_new.clear(); // windows are never "destroyed" on macOS; keep this bounded
        }
        if self.assignments.contains_key(&key) || !self.seen_new.insert(key) {
            return Ok(false);
        }
        if !self.is_processable(ws, w) {
            return Ok(false);
        }
        if last_zone && self.restore_last_zone(ws, w)? {
            return Ok(true);
        }
        let Some(cursor) = cursor.filter(|_| active_monitor) else { return Ok(false) };
        let monitors = ws.monitors()?;
        let rect = ws.window_rect(w)?;
        let active = monitors.iter().position(|m| m.frame.contains(cursor));
        let origin = monitors
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.frame.intersection(&rect).map(|x| (i, x.area())))
            .max_by_key(|&(_, a)| a)
            .map(|(i, _)| i)
            .or_else(|| monitors.iter().position(|m| m.primary));
        match (active, origin) {
            (Some(a), Some(o)) if a != o => {
                let r = rotation::fit_on_screen(rect, monitors[o].work_area, monitors[a].work_area);
                ws.set_window_rect(w, r)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Focus the next (previous) window snapped into the same zones as the
    /// focused window (`LayoutAssignedWindows::CycleWindows`).
    pub fn cycle_windows<WS: WindowSystem>(&mut self, ws: &WS, reverse: bool) -> Result<bool> {
        let Some(w) = ws.focused_window()? else { return Ok(false) };
        let key = ws.window_key(&w);
        let Some(cur) = self.assignments.get(&key).cloned() else { return Ok(false) };
        loop {
            let mut group: Vec<(u64, WindowKey)> = self
                .assignments
                .iter()
                .filter(|(_, a)| {
                    a.monitor_id == cur.monitor_id && a.layout_uuid == cur.layout_uuid && a.layout_kind == cur.layout_kind && a.zones == cur.zones
                })
                .map(|(k, a)| (a.order, *k))
                .collect();
            group.sort_unstable();
            if group.len() < 2 {
                return Ok(false);
            }
            let i = group.iter().position(|&(_, k)| k == key).unwrap_or(0);
            let n = group.len();
            let next = group[if reverse { (i + n - 1) % n } else { (i + 1) % n }].1;
            match ws.window_from_key(next) {
                Some(nw) => {
                    ws.focus_window(&nw)?;
                    return Ok(true);
                }
                // closed: dismiss and try again
                None => self.forget_window(next),
            }
        }
    }

    /// Monitor rotation: move every processable window to the next
    /// (previous) monitor in FancyZones order, scaled proportionally
    /// (`RotateWindowsAcrossMonitors`). Snapped windows are unsnapped.
    pub fn rotate_windows<WS: WindowSystem>(&mut self, ws: &WS, reverse: bool) -> Result<usize> {
        let monitors = ws.monitors()?;
        if monitors.len() < 2 {
            return Ok(0);
        }
        let rects: Vec<Rect> = monitors.iter().map(|m| m.work_area).collect();
        let ordered: Vec<&Monitor> = navigation::order_monitors(&rects).into_iter().map(|i| &monitors[i]).collect();
        let mut moved = 0;
        for w in ws.all_windows()? {
            if !self.is_processable(ws, &w) {
                continue;
            }
            let Ok(rect) = ws.window_rect(&w) else { continue };
            if rect.is_empty() {
                continue;
            }
            let src = ordered
                .iter()
                .enumerate()
                .filter_map(|(i, m)| m.frame.intersection(&rect).map(|x| (i, x.area())))
                .max_by_key(|&(_, a)| a)
                .map(|(i, _)| i);
            let Some(src) = src else { continue };
            let dst = rotation::rotated_monitor_index(src, ordered.len(), reverse);
            let target = rotation::map_rect_between_work_areas(rect, ordered[src].work_area, ordered[dst].work_area);
            let key = ws.window_key(&w);
            self.assignments.remove(&key);
            self.edge_snapped.remove(&key);
            match ws.set_window_rect(&w, target) {
                Ok(()) => moved += 1,
                Err(e) => log::debug!("fancyzones: rotating window failed: {e:#}"),
            }
        }
        Ok(moved)
    }

    /// Preview for monitor rotation: the outlines of all processable windows.
    pub fn rotation_preview<WS: WindowSystem>(&self, ws: &WS) -> Result<OverlayState> {
        let monitors = ws.monitors()?;
        let mut st = self.overlay_base();
        st.show_numbers = false;
        let windows: Vec<Rect> = ws
            .all_windows()?
            .iter()
            .filter(|w| self.is_processable(ws, w))
            .filter_map(|w| ws.window_rect(w).ok())
            .collect();
        for m in &monitors {
            let zones: Vec<Rect> = windows.iter().filter(|r| m.frame.contains(r.center())).copied().collect();
            st.monitors.push(OverlayMonitor { frame: m.frame, zones, highlighted: vec![] });
        }
        Ok(st)
    }

    pub fn assignment(&self, w: WindowKey) -> Option<&Assignment> {
        self.assignments.get(&w)
    }

    pub fn forget_window(&mut self, w: WindowKey) {
        self.assignments.remove(&w);
        self.pre_snap_size.remove(&w);
        self.edge_snapped.remove(&w);
        self.seen_new.remove(&w);
    }

    /// Windows currently assigned to zones.
    pub fn assigned_windows(&self) -> impl Iterator<Item = (WindowKey, &Assignment)> {
        self.assignments.iter().map(|(k, a)| (*k, a))
    }

    /// Index of the work area containing most of `r` (or nearest center).
    pub fn work_area_for_rect(areas: &[WorkArea], r: Rect) -> Option<usize> {
        let best = areas
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a.monitor.frame.intersection(&r).map(|x| (i, x.area())))
            .max_by_key(|&(_, a)| a);
        if let Some((i, _)) = best {
            return Some(i);
        }
        Self::work_area_for_point(areas, r.center())
    }

    pub fn work_area_for_point(areas: &[WorkArea], p: Point) -> Option<usize> {
        if let Some(i) = areas.iter().position(|a| a.monitor.frame.contains(p)) {
            return Some(i);
        }
        areas.iter().enumerate().min_by_key(|(_, a)| a.monitor.frame.distance_sq(p)).map(|(i, _)| i)
    }

    /// The window's current assignment translated to a work-area index,
    /// if it still looks valid (window center inside the snapped rect).
    fn current_target(&self, areas: &[WorkArea], key: WindowKey, rect: Rect) -> Option<SnapTarget> {
        let a = self.assignments.get(&key)?;
        let wa = areas.iter().position(|w| w.monitor.id == a.monitor_id)?;
        let l = &areas[wa].layout;
        if l.uuid != a.layout_uuid || l.kind != a.layout_kind || a.zones.iter().any(|&z| z >= l.zones.len()) {
            return None;
        }
        let zr = l.combined_zones_rect(&a.zones)?;
        let slack = 40;
        let grown = Rect::new(zr.left - slack, zr.top - slack, zr.right + slack, zr.bottom + slack);
        grown.contains(rect.center()).then(|| SnapTarget { work_area: wa, zones: a.zones.clone() })
    }

    /// Move `w` into `zones` of `areas[wa]` and record the assignment.
    pub fn snap_window<WS: WindowSystem>(
        &mut self,
        ws: &WS,
        w: &WS::Window,
        areas: &[WorkArea],
        wa: usize,
        zones: &[usize],
    ) -> Result<bool> {
        self.place_window(ws, w, areas, wa, zones, true)
    }

    fn place_window<WS: WindowSystem>(
        &mut self,
        ws: &WS,
        w: &WS::Window,
        areas: &[WorkArea],
        wa: usize,
        zones: &[usize],
        record_history: bool,
    ) -> Result<bool> {
        let area = &areas[wa];
        let Some(rect) = area.layout.combined_zones_rect(zones) else {
            return Ok(false);
        };
        let key = ws.window_key(w);
        self.remember_pre_snap_size(ws, w, key);
        ws.set_window_rect(w, rect)?;
        self.edge_snapped.remove(&key);
        if self.config.settings.disable_round_corners {
            ws.set_snapped_style(w, true);
        }
        let mut zs = zones.to_vec();
        zs.sort_unstable();
        zs.dedup();
        // Keep the position within the zone when re-snapping to the same zones.
        let order = match self.assignments.get(&key) {
            Some(a) if a.zones == zs && a.monitor_id == area.monitor.id => a.order,
            _ => {
                self.next_order += 1;
                self.next_order
            }
        };
        let asg = Assignment {
            monitor_id: area.monitor.id.clone(),
            layout_uuid: area.layout.uuid.clone(),
            layout_kind: area.layout.kind,
            zones: zs.clone(),
            order,
        };
        if let Some(app) = ws.window_app_id(w).filter(|_| record_history) {
            self.config.app_zone_history.insert(
                app,
                AppZoneHistoryEntry {
                    monitor: asg.monitor_id.clone(),
                    layout_uuid: asg.layout_uuid.clone(),
                    layout_kind: asg.layout_kind,
                    zones: zs,
                },
            );
        }
        self.assignments.insert(key, asg);
        Ok(true)
    }

    fn remember_pre_snap_size<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window, key: WindowKey) {
        if !self.assignments.contains_key(&key) && !self.edge_snapped.contains(&key) {
            if let Ok(cur) = ws.window_rect(w) {
                self.pre_snap_size.entry(key).or_insert((cur.width(), cur.height()));
            }
        }
    }

    /// Move `w` to an edge-snap target rect.
    pub fn edge_snap_window<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window, rect: Rect) -> Result<()> {
        let key = ws.window_key(w);
        self.remember_pre_snap_size(ws, w, key);
        ws.set_window_rect(w, rect)?;
        self.assignments.remove(&key);
        self.edge_snapped.insert(key);
        Ok(())
    }

    pub fn is_edge_snapped(&self, w: WindowKey) -> bool {
        self.edge_snapped.contains(&w)
    }

    /// Unsnap (e.g. dragged out of all zones or away from an edge snap);
    /// restores the pre-snap size if configured.
    pub fn unsnap_window<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window) -> Result<()> {
        let key = ws.window_key(w);
        let was_zone = self.assignments.remove(&key).is_some();
        let was_edge = self.edge_snapped.remove(&key);
        ws.set_snapped_style(w, false);
        if was_zone || was_edge {
            let size = self.pre_snap_size.remove(&key);
            if let (true, Some((sw, sh))) = (self.config.settings.restore_size, size) {
                let r = ws.window_rect(w)?;
                ws.set_window_rect(w, Rect::from_xywh(r.left, r.top, sw, sh))?;
            }
        }
        Ok(())
    }

    /// Keyboard snap (Win+Arrow / Ctrl+Option+Arrow) of the focused window.
    pub fn snap_focused<WS: WindowSystem>(&mut self, ws: &WS, dir: Direction) -> Result<bool> {
        let Some(w) = ws.focused_window()? else {
            return Ok(false);
        };
        self.snap_window_direction(ws, &w, dir)
    }

    pub fn snap_window_direction<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window, dir: Direction) -> Result<bool> {
        self.extend = None;
        if !self.is_processable(ws, w) {
            return Ok(false);
        }
        let areas = self.areas(ws)?;
        let rect = ws.window_rect(w)?;
        let key = ws.window_key(w);
        let Some(cur) = Self::work_area_for_rect(&areas, rect) else {
            return Ok(false);
        };
        let current = self.current_target(&areas, key, rect);
        let layouts: Vec<&Layout> = areas.iter().map(|a| &a.layout).collect();
        let s = &self.config.settings;
        let target = if s.move_windows_based_on_position {
            navigation::snap_by_position(&layouts, cur, rect, current.as_ref(), dir, s.move_window_across_monitors)
        } else {
            navigation::snap_by_index(&layouts, cur, current.as_ref(), dir, s.move_window_across_monitors)
        };
        match target {
            Some(t) => self.snap_window(ws, w, &areas, t.work_area, &t.zones),
            None => Ok(false),
        }
    }

    /// Keyboard extend (Win+Ctrl+Alt+Arrow / Ctrl+Option+Cmd+Arrow).
    pub fn extend_focused<WS: WindowSystem>(&mut self, ws: &WS, dir: Direction) -> Result<bool> {
        let Some(w) = ws.focused_window()? else {
            return Ok(false);
        };
        if !self.is_processable(ws, &w) {
            return Ok(false);
        }
        let areas = self.areas(ws)?;
        let rect = ws.window_rect(&w)?;
        let key = ws.window_key(&w);
        let Some(cur) = Self::work_area_for_rect(&areas, rect) else {
            return Ok(false);
        };
        let applied = self.current_target(&areas, key, rect).filter(|t| t.work_area == cur).map(|t| t.zones).unwrap_or_default();
        let mut st = self.extend.take();
        let r = navigation::extend(&areas[cur].layout, key, rect, &applied, &mut st, dir);
        let res = match r {
            Some(z) => self.snap_window(ws, &w, &areas, cur, &z),
            None => Ok(false),
        };
        self.extend = st;
        res
    }

    /// Snap the focused window into zone `zone` of `monitor_id` (or the
    /// monitor it is on).
    pub fn snap_focused_to_zone<WS: WindowSystem>(&mut self, ws: &WS, monitor_id: Option<&str>, zone: usize) -> Result<bool> {
        let Some(w) = ws.focused_window()? else {
            anyhow::bail!("no focused window");
        };
        if !self.is_processable(ws, &w) {
            anyhow::bail!("the focused window is excluded or not snappable");
        }
        let areas = self.areas(ws)?;
        let wa = match monitor_id {
            Some(id) => areas.iter().position(|a| a.monitor.id == id),
            None => Self::work_area_for_rect(&areas, ws.window_rect(&w)?),
        };
        let Some(wa) = wa else { anyhow::bail!("monitor not found") };
        if zone >= areas[wa].layout.zones.len() {
            anyhow::bail!("zone {zone} out of range (layout has {} zones)", areas[wa].layout.zones.len());
        }
        self.snap_window(ws, &w, &areas, wa, &[zone])
    }

    /// Move a window to its application's last zone, if known and the layout
    /// still matches (FancyZones "appLastZone_moveWindows").
    pub fn restore_last_zone<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window) -> Result<bool> {
        let Some(app) = ws.window_app_id(w) else { return Ok(false) };
        let Some(h) = self.config.app_zone_history.get(&app).cloned() else { return Ok(false) };
        if !self.is_processable(ws, w) {
            return Ok(false);
        }
        let areas = self.areas(ws)?;
        let Some(wa) = areas.iter().position(|a| a.monitor.id == h.monitor) else { return Ok(false) };
        let l = &areas[wa].layout;
        if l.uuid != h.layout_uuid || l.kind != h.layout_kind || h.zones.iter().any(|&z| z >= l.zones.len()) {
            return Ok(false);
        }
        self.snap_window(ws, w, &areas, wa, &h.zones)
    }

    // ----------------------------------------------------------------- drag

    fn zones_should_show(&self, shift: bool, toggle: bool) -> bool {
        let s = &self.config.settings;
        let t = toggle && s.mouse_switch;
        if s.shift_drag {
            shift ^ t
        } else {
            !(shift ^ t)
        }
    }

    /// A window started moving (Windows: EVENT_SYSTEM_MOVESIZESTART; macOS:
    /// left-drag that moved a window).
    pub fn drag_start(&mut self, window: WindowKey, start_rect: Rect) {
        self.drag = Some(DragSession {
            window,
            start_rect,
            work_area: None,
            highlighted: vec![],
            initial_highlight: vec![],
            secondary_toggle: false,
            multi_toggle: false,
            zones_active: false,
            edge: None,
        });
    }

    /// Secondary mouse button pressed during a drag.
    pub fn drag_toggle_secondary(&mut self) {
        if let Some(d) = &mut self.drag {
            d.secondary_toggle = !d.secondary_toggle;
        }
    }

    /// Middle mouse button pressed during a drag: toggles multi-zone spanning
    /// (`mouse_middle_click_spanning_multiple_zones`) or acts like the
    /// secondary button.
    pub fn drag_middle_click(&mut self) {
        let span = self.config.settings.mouse_middle_click_spanning_multiple_zones;
        if let Some(d) = &mut self.drag {
            if span {
                d.multi_toggle = !d.multi_toggle;
            } else {
                d.secondary_toggle = !d.secondary_toggle;
            }
        }
    }

    /// Force the next [`Engine::drag_update`] to report a change (after the
    /// work areas were re-instantiated, e.g. a layout switch while dragging).
    pub fn drag_invalidate(&mut self) {
        if let Some(d) = &mut self.drag {
            d.work_area = None;
            d.highlighted.clear();
            d.initial_highlight.clear();
        }
    }

    /// `true` while a drag shows zones.
    pub fn drag_zones_shown(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.zones_active)
    }

    /// Update the drag; returns `Some(overlay)` when what should be drawn changed.
    pub fn drag_update(&mut self, areas: &[WorkArea], input: DragInput) -> Option<OverlayState> {
        let toggle = self.drag.as_ref()?.secondary_toggle;
        let active = self.zones_should_show(input.shift, toggle);
        let many = input.ctrl || self.drag.as_ref()?.multi_toggle;
        let alg = self.config.settings.overlapping_zones_algorithm;
        let wa = Self::work_area_for_point(areas, input.cursor);
        let d = self.drag.as_mut().unwrap();
        let mut changed = d.zones_active != active || d.work_area != wa;
        if d.work_area != wa {
            d.initial_highlight.clear();
        }
        d.zones_active = active;
        d.work_area = wa;
        let new_hl = match (active, wa) {
            (true, Some(i)) => {
                let l = &areas[i].layout;
                let mut hl = l.zones_from_point(input.cursor, alg);
                if many {
                    if d.initial_highlight.is_empty() {
                        d.initial_highlight = hl.clone();
                    } else {
                        hl = l.combined_zone_range(&d.initial_highlight, &hl);
                    }
                } else {
                    d.initial_highlight.clear();
                }
                hl
            }
            _ => {
                d.initial_highlight.clear();
                vec![]
            }
        };
        if new_hl != d.highlighted {
            changed = true;
            d.highlighted = new_hl;
        }
        let edge = match (active, wa) {
            (false, Some(i)) if self.config.settings.edge_snap_enabled => {
                let s = &self.config.settings;
                let others: Vec<Rect> =
                    areas.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, a)| a.monitor.frame).collect();
                let m = &areas[i].monitor;
                edge_snap::edge_action(&s.edge_snap_config(), m.frame, &others, input.cursor)
                    .map(|a| (a, a.rect(m.work_area, s.edge_snap_gap)))
            }
            _ => None,
        };
        if edge != d.edge {
            changed = true;
            d.edge = edge;
        }
        changed.then(|| self.overlay_state(areas))
    }

    /// Overlay of one work area; a work area spanning several monitors is
    /// split into one overlay per physical monitor (an overlay window cannot
    /// span displays on macOS with "Displays have separate Spaces").
    fn push_area(st: &mut OverlayState, a: &WorkArea, highlighted: Vec<usize>) {
        if a.physical_frames.len() > 1 {
            for f in &a.physical_frames {
                st.monitors.push(OverlayMonitor { frame: *f, zones: a.layout.zones.clone(), highlighted: highlighted.clone() });
            }
        } else {
            st.monitors.push(OverlayMonitor { frame: a.monitor.frame, zones: a.layout.zones.clone(), highlighted });
        }
    }

    fn overlay_base(&self) -> OverlayState {
        let s = &self.config.settings;
        let c = |h: &str, d: (u8, u8, u8)| crate::config::parse_hex_color(h).unwrap_or(d);
        OverlayState {
            monitors: vec![],
            edge_preview: None,
            show_numbers: s.show_zone_number,
            zone_color: c(&s.zone_color, (0xAA, 0xCD, 0xFF)),
            border_color: c(&s.zone_border_color, (0xFF, 0xFF, 0xFF)),
            highlight_color: c(&s.zone_highlight_color, (0x00, 0x8C, 0xFF)),
            number_color: c(&s.zone_number_color, (0, 0, 0)),
            opacity: (s.zone_highlight_opacity.min(100) as f64) / 100.0,
        }
    }

    /// The overlay for the current drag (empty when zones are not shown).
    pub fn overlay_state(&self, areas: &[WorkArea]) -> OverlayState {
        let s = &self.config.settings;
        let mut st = self.overlay_base();
        let Some(d) = &self.drag else { return st };
        if !d.zones_active {
            if let (Some((_, rect)), Some(i)) = (d.edge, d.work_area) {
                st.show_numbers = false;
                st.edge_preview = Some(rect);
                let a = &areas[i];
                let frame = a.physical_frames.iter().copied().find(|f| f.contains(rect.center())).unwrap_or(a.monitor.frame);
                st.monitors.push(OverlayMonitor { frame, zones: vec![rect], highlighted: vec![0] });
            }
            return st;
        }
        for (i, a) in areas.iter().enumerate() {
            let is_cur = d.work_area == Some(i);
            if is_cur || s.show_zones_on_all_monitors {
                Self::push_area(&mut st, a, if is_cur { d.highlighted.clone() } else { vec![] });
            }
        }
        st
    }

    /// Drag finished: snap into the highlighted zones or the edge target, or
    /// unsnap if the window was dragged out of its zone / edge snap.
    pub fn drag_end<WS: WindowSystem>(&mut self, ws: &WS, w: &WS::Window, areas: &[WorkArea]) -> Result<DragResult> {
        let Some(d) = self.drag.take() else { return Ok(DragResult::Moved) };
        if d.zones_active && !d.highlighted.is_empty() {
            if let Some(wa) = d.work_area {
                self.snap_window(ws, w, areas, wa, &d.highlighted)?;
                return Ok(DragResult::Zones(d.highlighted));
            }
        }
        if !d.zones_active {
            if let Some((action, rect)) = d.edge {
                self.edge_snap_window(ws, w, rect)?;
                return Ok(DragResult::Edge(action, rect));
            }
        }
        self.unsnap_window(ws, w)?;
        Ok(DragResult::Moved)
    }

    pub fn drag_cancel(&mut self) {
        self.drag = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// In-memory window system for tests.
    pub struct MockWs {
        pub monitors: Vec<Monitor>,
        pub windows: RefCell<HashMap<u64, Rect>>,
        pub focused: Cell<Option<u64>>,
        pub kinds: RefCell<HashMap<u64, WindowKind>>,
        pub vds: RefCell<HashMap<String, String>>,
        pub styled: RefCell<HashMap<u64, bool>>,
    }

    impl MockWs {
        pub fn new(monitors: Vec<Monitor>, windows: &[(u64, Rect)]) -> Self {
            MockWs {
                monitors,
                windows: RefCell::new(windows.iter().copied().collect()),
                focused: Cell::new(windows.first().map(|w| w.0)),
                kinds: RefCell::default(),
                vds: RefCell::default(),
                styled: RefCell::default(),
            }
        }
        pub fn rect(&self, w: u64) -> Rect {
            self.windows.borrow()[&w]
        }
    }

    impl WindowSystem for MockWs {
        type Window = u64;
        fn monitors(&self) -> Result<Vec<Monitor>> {
            Ok(self.monitors.clone())
        }
        fn focused_window(&self) -> Result<Option<u64>> {
            Ok(self.focused.get())
        }
        fn window_key(&self, w: &u64) -> WindowKey {
            *w
        }
        fn window_rect(&self, w: &u64) -> Result<Rect> {
            self.windows.borrow().get(w).copied().ok_or_else(|| anyhow::anyhow!("no window"))
        }
        fn set_window_rect(&self, w: &u64, r: Rect) -> Result<()> {
            self.windows.borrow_mut().insert(*w, r);
            Ok(())
        }
        fn window_app_id(&self, w: &u64) -> Option<String> {
            Some(format!("/Applications/app{w}.app"))
        }
        fn window_kind(&self, w: &u64) -> WindowKind {
            self.kinds.borrow().get(w).copied().unwrap_or_default()
        }
        fn window_from_key(&self, key: WindowKey) -> Option<u64> {
            self.windows.borrow().contains_key(&key).then_some(key)
        }
        fn focus_window(&self, w: &u64) -> Result<()> {
            self.focused.set(Some(*w));
            Ok(())
        }
        fn all_windows(&self) -> Result<Vec<u64>> {
            let mut v: Vec<u64> = self.windows.borrow().keys().copied().collect();
            v.sort_unstable();
            Ok(v)
        }
        fn virtual_desktops(&self, _: &[Monitor]) -> HashMap<String, String> {
            self.vds.borrow().clone()
        }
        fn set_snapped_style(&self, w: &u64, snapped: bool) {
            self.styled.borrow_mut().insert(*w, snapped);
        }
    }

    pub fn monitor(id: &str, frame: Rect) -> Monitor {
        Monitor {
            id: id.into(),
            name: id.into(),
            frame,
            work_area: Rect::new(frame.left, frame.top + 25, frame.right, frame.bottom),
            scale: 2.0,
            primary: frame.left == 0 && frame.top == 0,
        }
    }

    fn setup() -> (Engine, MockWs) {
        let mut cfg = Config::default();
        cfg.settings.default_layout = AppliedLayout::template(LayoutType::Columns, 2).with_spacing(false, 0);
        let ws = MockWs::new(
            vec![monitor("B", Rect::new(1000, 0, 2000, 825)), monitor("A", Rect::new(0, 0, 1000, 825))],
            &[(1, Rect::new(100, 100, 400, 400))],
        );
        (Engine::new(cfg), ws)
    }

    #[test]
    fn work_area_order_and_layouts() {
        let (e, ws) = setup();
        let areas = e.work_areas(&ws.monitors);
        assert_eq!(areas[0].monitor.id, "A");
        assert_eq!(areas[0].layout.zones[0], Rect::new(0, 25, 500, 825));
        assert_eq!(areas[1].layout.zones[1], Rect::new(1500, 25, 2000, 825));
    }

    #[test]
    fn keyboard_snap_by_index_and_across_monitors() {
        let (mut e, ws) = setup();
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        assert_eq!(ws.windows.borrow()[&1], Rect::new(0, 25, 500, 825));
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        assert_eq!(ws.windows.borrow()[&1], Rect::new(500, 25, 1000, 825));
        // single-monitor mode cycles
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        assert_eq!(ws.windows.borrow()[&1], Rect::new(0, 25, 500, 825));
        e.config.settings.move_window_across_monitors = true;
        e.snap_focused(&ws, Direction::Right).unwrap();
        e.snap_focused(&ws, Direction::Right).unwrap();
        assert_eq!(ws.windows.borrow()[&1], Rect::new(1000, 25, 1500, 825));
        assert_eq!(e.assignment(1).unwrap().monitor_id, "B");
        // last-zone history recorded
        assert_eq!(e.config.app_zone_history["/Applications/app1.app"].zones, vec![0]);
        // up/down ignored in index mode
        assert!(!e.snap_focused(&ws, Direction::Up).unwrap());
    }

    #[test]
    fn keyboard_snap_by_position() {
        let (mut e, ws) = setup();
        e.config.settings.move_windows_based_on_position = true;
        e.config.settings.move_window_across_monitors = true;
        e.config.applied_layouts.insert("A".into(), AppliedLayout::template(LayoutType::Grid, 4).with_spacing(false, 0));
        assert!(e.snap_focused(&ws, Direction::Down).unwrap());
        let r = ws.windows.borrow()[&1];
        assert_eq!(r, Rect::new(0, 425, 500, 825));
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        assert_eq!(e.assignment(1).unwrap().monitor_id, "B");
    }

    #[test]
    fn manual_move_invalidates_assignment() {
        let (mut e, ws) = setup();
        e.snap_focused(&ws, Direction::Right).unwrap(); // zone 0 of A
        ws.windows.borrow_mut().insert(1, Rect::new(600, 300, 900, 600)); // user moved it into zone 1 area
        e.snap_focused(&ws, Direction::Left).unwrap(); // treated as unsnapped -> last zone
        assert_eq!(e.assignment(1).unwrap().zones, vec![1]);
    }

    #[test]
    fn keyboard_extend() {
        let (mut e, ws) = setup();
        e.config.applied_layouts.insert("A".into(), AppliedLayout::template(LayoutType::Columns, 3).with_spacing(false, 0));
        e.snap_focused_to_zone(&ws, Some("A"), 0).unwrap();
        assert!(e.extend_focused(&ws, Direction::Right).unwrap());
        assert_eq!(e.assignment(1).unwrap().zones, vec![0, 1]);
        assert!(e.extend_focused(&ws, Direction::Right).unwrap());
        assert_eq!(e.assignment(1).unwrap().zones, vec![0, 1, 2]);
        assert_eq!(ws.windows.borrow()[&1], Rect::new(0, 25, 1000, 825));
        assert!(e.snap_focused_to_zone(&ws, Some("A"), 9).is_err());
    }

    #[test]
    fn drag_session_shift_and_span() {
        let (mut e, ws) = setup();
        let areas = e.work_areas(&ws.monitors);
        e.drag_start(1, Rect::new(100, 100, 400, 400));
        let inp = |x, y, shift, ctrl| DragInput { cursor: Point::new(x, y), shift, ctrl };
        // no shift -> nothing shown
        let o = e.drag_update(&areas, inp(100, 300, false, false));
        assert!(o.is_none_or(|o| o.monitors.is_empty()));
        // shift -> zone 0 highlighted on monitor A only
        let o = e.drag_update(&areas, inp(100, 300, true, false)).unwrap();
        assert_eq!(o.monitors.len(), 1);
        assert_eq!(o.monitors[0].highlighted, vec![0]);
        // unchanged -> None
        assert!(e.drag_update(&areas, inp(110, 300, true, false)).is_none());
        // ctrl: start selection at zone 0, move to zone 1 -> both
        e.drag_update(&areas, inp(100, 300, true, true));
        let o = e.drag_update(&areas, inp(800, 300, true, true)).unwrap();
        assert_eq!(o.monitors[0].highlighted, vec![0, 1]);
        let r = e.drag_end(&ws, &1, &areas).unwrap();
        assert_eq!(r, DragResult::Zones(vec![0, 1]));
        assert_eq!(ws.windows.borrow()[&1], Rect::new(0, 25, 1000, 825));
    }

    #[test]
    fn drag_out_unsnaps_and_restores_size() {
        let (mut e, ws) = setup();
        e.config.settings.restore_size = true;
        e.config.settings.show_zones_on_all_monitors = true;
        let areas = e.work_areas(&ws.monitors);
        e.snap_focused_to_zone(&ws, Some("B"), 1).unwrap();
        e.drag_start(1, ws.windows.borrow()[&1]);
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(1600, 400), shift: true, ctrl: false }).unwrap();
        assert_eq!(o.monitors.len(), 2);
        // release without shift -> unsnap + restore 300x300
        e.drag_update(&areas, DragInput { cursor: Point::new(1600, 400), shift: false, ctrl: false });
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Moved);
        assert!(e.assignment(1).is_none());
        let r = ws.windows.borrow()[&1];
        assert_eq!((r.width(), r.height()), (300, 300));
    }

    #[test]
    fn shift_drag_off_and_mouse_switch() {
        let (mut e, ws) = setup();
        e.config.settings.shift_drag = false;
        e.config.settings.mouse_switch = true;
        let areas = e.work_areas(&ws.monitors);
        e.drag_start(1, Rect::new(0, 0, 10, 10));
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(100, 300), shift: false, ctrl: false }).unwrap();
        assert_eq!(o.monitors.len(), 1);
        e.drag_toggle_secondary();
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(100, 300), shift: false, ctrl: false }).unwrap();
        assert!(o.monitors.is_empty());
    }

    #[test]
    fn span_across_monitors_and_restore_last_zone() {
        let (mut e, ws) = setup();
        e.config.settings.span_zones_across_monitors = true;
        let areas = e.work_areas(&ws.monitors);
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].layout.zones[1], Rect::new(1000, 25, 2000, 825));
        e.snap_focused_to_zone(&ws, None, 1).unwrap();
        ws.windows.borrow_mut().insert(1, Rect::new(0, 0, 50, 50));
        e.forget_window(1);
        assert!(e.restore_last_zone(&ws, &1).unwrap());
        assert_eq!(ws.windows.borrow()[&1], Rect::new(1000, 25, 2000, 825));
        // a layout change invalidates history
        e.config.settings.span_zones_across_monitors = false;
        assert!(!e.restore_last_zone(&ws, &1).unwrap());
    }

    fn edge_setup() -> (Engine, MockWs, Vec<WorkArea>) {
        let (mut e, ws) = setup();
        e.config.settings.edge_snap_enabled = true;
        let areas = e.work_areas(&ws.monitors);
        (e, ws, areas)
    }

    fn at(x: i32, y: i32, shift: bool) -> DragInput {
        DragInput { cursor: Point::new(x, y), shift, ctrl: false }
    }

    #[test]
    fn edge_snap_halves_quarters_and_maximize() {
        let (mut e, ws, areas) = edge_setup();
        // Monitors: A = 0..1000, B = 1000..2000, both 825 tall with a 25 px menu bar.
        for (p, action, rect) in [
            (Point::new(0, 400), EdgeAction::LeftHalf, Rect::new(0, 25, 500, 825)),
            (Point::new(1999, 400), EdgeAction::RightHalf, Rect::new(1500, 25, 2000, 825)),
            (Point::new(500, 0), EdgeAction::Maximize, Rect::new(0, 25, 1000, 825)),
            (Point::new(1500, 824), EdgeAction::BottomHalf, Rect::new(1000, 425, 2000, 825)),
            (Point::new(0, 0), EdgeAction::TopLeft, Rect::new(0, 25, 500, 425)),
            (Point::new(1999, 824), EdgeAction::BottomRight, Rect::new(1500, 425, 2000, 825)),
        ] {
            e.drag_start(1, ws.windows.borrow()[&1]);
            e.drag_update(&areas, at(500, 400, false));
            let o = e.drag_update(&areas, at(p.x, p.y, false)).unwrap();
            assert_eq!(o.edge_preview, Some(rect), "{action:?}");
            assert_eq!(o.monitors.len(), 1);
            assert_eq!(o.monitors[0].zones, vec![rect]);
            assert!(!o.show_numbers);
            assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Edge(action, rect));
            assert_eq!(ws.windows.borrow()[&1], rect);
            assert!(e.is_edge_snapped(1));
        }
    }

    #[test]
    fn edge_snap_preview_hides_when_leaving_margin_and_shared_edges_ignored() {
        let (mut e, ws, areas) = edge_setup();
        e.drag_start(1, ws.windows.borrow()[&1]);
        assert!(e.drag_update(&areas, at(2, 400, false)).unwrap().edge_preview.is_some());
        let o = e.drag_update(&areas, at(300, 400, false)).unwrap();
        assert!(o.monitors.is_empty() && o.edge_preview.is_none());
        // A's right edge / B's left edge are shared -> no snap while crossing.
        assert!(e.drag_update(&areas, at(999, 400, false)).is_none());
        // (moving onto B changes the work area, but still shows nothing)
        assert!(e.drag_update(&areas, at(1000, 400, false)).is_none_or(|o| o.monitors.is_empty()));
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Moved);
        assert_eq!(ws.windows.borrow()[&1], Rect::new(100, 100, 400, 400));
    }

    #[test]
    fn zones_take_precedence_over_edge_snap() {
        let (mut e, ws, areas) = edge_setup();
        e.drag_start(1, ws.windows.borrow()[&1]);
        let o = e.drag_update(&areas, at(0, 400, true)).unwrap();
        assert!(o.edge_preview.is_none());
        assert_eq!(o.monitors[0].highlighted, vec![0]);
        assert_eq!(o.monitors[0].zones.len(), 2);
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Zones(vec![0]));
        assert!(!e.is_edge_snapped(1));
        // disabled -> plain move
        e.config.settings.edge_snap_enabled = false;
        e.drag_start(1, ws.windows.borrow()[&1]);
        assert!(e.drag_update(&areas, at(0, 400, false)).unwrap().monitors.is_empty());
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Moved);
    }

    #[test]
    fn edge_snap_restores_size_when_dragged_out_and_gap_applies() {
        let (mut e, ws, areas) = edge_setup();
        e.config.settings.restore_size = true;
        e.config.settings.edge_snap_gap = 10;
        e.drag_start(1, ws.windows.borrow()[&1]);
        e.drag_update(&areas, at(0, 400, false));
        let r = e.drag_end(&ws, &1, &areas).unwrap();
        assert_eq!(r, DragResult::Edge(EdgeAction::LeftHalf, Rect::new(10, 35, 495, 815)));
        // edge -> zone keeps the original pre-snap size
        e.snap_focused_to_zone(&ws, Some("B"), 0).unwrap();
        assert!(!e.is_edge_snapped(1));
        // zone -> edge -> dragged out: back to 300x300
        e.drag_start(1, ws.windows.borrow()[&1]);
        e.drag_update(&areas, at(1999, 300, false));
        e.drag_end(&ws, &1, &areas).unwrap();
        assert!(e.is_edge_snapped(1) && e.assignment(1).is_none());
        e.drag_start(1, ws.windows.borrow()[&1]);
        e.drag_update(&areas, at(1500, 300, false));
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Moved);
        assert!(!e.is_edge_snapped(1));
        let r = ws.windows.borrow()[&1];
        assert_eq!((r.width(), r.height()), (300, 300));
    }

    #[test]
    fn old_settings_json_gets_edge_snap_defaults() {
        let s: crate::Settings = serde_json::from_str(r#"{"shift_drag": true, "restore_size": true}"#).unwrap();
        assert_eq!(s.edge_snap_margin, 5);
        assert_eq!(s.edge_snap_corner_size, 20);
        assert_eq!(s.edge_snap_top, crate::TopEdgeAction::Maximize);
        assert_eq!(s.edge_snap_bottom, crate::BottomEdgeAction::BottomHalf);
        assert_eq!(s.edge_snap_enabled, cfg!(not(windows)));
        let s: crate::Settings = serde_json::from_str(r#"{"edge_snap_bottom": "thirds", "edge_snap_top": "top-half"}"#).unwrap();
        assert_eq!(s.edge_snap_bottom, crate::BottomEdgeAction::Thirds);
        assert_eq!(s.edge_snap_top, crate::TopEdgeAction::TopHalf);
    }

    use crate::config::LayoutHotkey;
    use crate::layout::{CustomLayout, CustomLayoutInfo, GridLayoutInfo};

    fn columns_custom(uuid: &str, name: &str, n: usize) -> CustomLayout {
        let pct = vec![10000 / n as i32; n];
        let mut pct = pct;
        let rest = 10000 - pct.iter().sum::<i32>();
        pct[n - 1] += rest;
        let mut g = GridLayoutInfo::new(vec![10000], pct, vec![(0..n as i32).collect()]);
        g.show_spacing = false;
        g.spacing = 0;
        CustomLayout { uuid: uuid.into(), name: name.into(), info: CustomLayoutInfo::Grid(g) }
    }

    fn cols(n: i32) -> AppliedLayout {
        AppliedLayout::template(LayoutType::Columns, n).with_spacing(false, 0)
    }

    #[test]
    fn zone_set_change_moves_windows_by_index() {
        let (mut e, ws) = setup();
        ws.windows.borrow_mut().insert(2, Rect::new(10, 10, 50, 50));
        e.config.settings.zone_set_change_move_windows = true;
        e.config.applied_layouts.insert("A".into(), cols(4));
        e.snap_focused_to_zone(&ws, Some("A"), 1).unwrap(); // window 1 -> zone 1
        ws.focused.set(Some(2));
        e.snap_focused_to_zone(&ws, Some("A"), 3).unwrap(); // window 2 -> zone 3
        assert_eq!(ws.rect(1), Rect::new(250, 25, 500, 825));
        e.apply_layout("A", cols(2));
        assert_eq!(e.on_layout_changed(&ws, "A").unwrap(), 1);
        assert_eq!(ws.rect(1), Rect::new(500, 25, 1000, 825));
        let a = e.assignment(1).unwrap();
        assert_eq!((a.zones.clone(), a.layout_kind), (vec![1], LayoutType::Columns));
        // zone 3 no longer exists -> unassigned, not moved
        assert!(e.assignment(2).is_none());
        assert_eq!(ws.rect(2), Rect::new(750, 25, 1000, 825));
        // keyboard navigation continues from the new assignment
        e.snap_focused_to_zone(&ws, Some("B"), 0).unwrap();
        // setting off: windows are only unassigned
        e.config.settings.zone_set_change_move_windows = false;
        e.apply_layout("B", cols(3));
        assert_eq!(e.on_layout_changed(&ws, "B").unwrap(), 0);
        assert!(e.assignment(2).is_none());
        assert_eq!(ws.rect(2), Rect::new(1000, 25, 1500, 825));
    }

    #[test]
    fn quick_layout_switch_flash_and_label() {
        let (mut e, ws) = setup();
        e.config.custom_layouts.push(columns_custom("{A1}", "Two", 2));
        e.config.custom_layouts.push(columns_custom("{B2}", "Three", 3));
        e.config.layout_hotkeys.push(LayoutHotkey { key: 2, layout_uuid: "{B2}".into() });
        assert!(e.apply_quick_layout(&ws, 5, Point::new(1500, 400)).unwrap().is_none());
        let fb = e.apply_quick_layout(&ws, 2, Point::new(1500, 400)).unwrap().unwrap();
        assert_eq!(e.config.applied_layouts["B"].uuid, "{B2}");
        assert!(!e.config.applied_layouts.contains_key("A"));
        let flash = fb.flash.unwrap();
        assert_eq!(flash.monitors.len(), 2); // all work areas flash
        assert_eq!(flash.monitors[1].zones.len(), 3);
        let label = fb.label.unwrap();
        assert_eq!(label.text, "Three");
        assert_eq!(label.area, ws.monitors[0].work_area);
        assert_eq!((label.font_size, label.padding, label.duration_ms), (36, 12, 1500));
        // switches can be disabled
        e.config.settings.flash_zones_on_quick_switch = false;
        e.config.settings.layout_name_label_enabled = false;
        let fb = e.apply_quick_layout(&ws, 2, Point::new(1500, 400)).unwrap().unwrap();
        assert_eq!(fb, SwitchFeedback::default());
        e.config.settings.quick_layout_switch = false;
        assert!(e.apply_quick_layout(&ws, 2, Point::new(1500, 400)).unwrap().is_none());
    }

    #[test]
    fn wheel_cycles_layouts_while_dragging() {
        let (mut e, ws) = setup();
        e.config.settings.mouse_wheel_layout_switch = true;
        e.config.custom_layouts.push(columns_custom("{A1}", "Two", 2));
        e.config.custom_layouts.push(columns_custom("{B2}", "Three", 3));
        e.config.custom_layouts.push(columns_custom("{C3}", "Four", 4));
        // < 2 hotkeys -> all custom layouts in order
        assert_eq!(e.wheel_cycle_ids(), vec!["{A1}", "{B2}", "{C3}"]);
        let areas = e.work_areas(&ws.monitors);
        e.drag_start(1, ws.rect(1));
        // zones not shown -> nothing
        e.drag_update(&areas, DragInput { cursor: Point::new(100, 300), shift: false, ctrl: false });
        assert!(e.drag_wheel(&ws, &areas, false).unwrap().is_none());
        e.drag_update(&areas, DragInput { cursor: Point::new(100, 300), shift: true, ctrl: false });
        assert!(e.drag_zones_shown());
        let fb = e.drag_wheel(&ws, &areas, false).unwrap().unwrap();
        assert_eq!(e.config.applied_layouts["A"].uuid, "{A1}"); // not in cycle -> first
        assert!(fb.flash.is_none()); // never flash while dragging
        assert_eq!(fb.label.unwrap().text, "Two");
        e.drag_wheel(&ws, &areas, false).unwrap();
        assert_eq!(e.config.applied_layouts["A"].uuid, "{B2}");
        e.drag_wheel(&ws, &areas, true).unwrap(); // wheel up = backwards
        assert_eq!(e.config.applied_layouts["A"].uuid, "{A1}");
        // the drag picks up the new zones after re-instantiation
        let areas = e.work_areas(&ws.monitors);
        e.drag_invalidate();
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(900, 300), shift: true, ctrl: false }).unwrap();
        assert_eq!(o.monitors[0].zones.len(), 2);
        assert_eq!(o.monitors[0].highlighted, vec![1]);
        // hotkeyed layouts (>= 2) define the cycle
        e.config.layout_hotkeys = vec![
            LayoutHotkey { key: 7, layout_uuid: "{A1}".into() },
            LayoutHotkey { key: 1, layout_uuid: "{C3}".into() },
            LayoutHotkey { key: 2, layout_uuid: "{gone}".into() },
        ];
        assert_eq!(e.wheel_cycle_ids(), vec!["{C3}", "{A1}"]);
    }

    #[test]
    fn middle_click_spans_zones() {
        let (mut e, ws) = setup();
        e.config.settings.mouse_middle_click_spanning_multiple_zones = true;
        let areas = e.work_areas(&ws.monitors);
        e.drag_start(1, ws.rect(1));
        let inp = |x| DragInput { cursor: Point::new(x, 300), shift: true, ctrl: false };
        e.drag_update(&areas, inp(100));
        e.drag_middle_click();
        e.drag_update(&areas, inp(110));
        let o = e.drag_update(&areas, inp(800)).unwrap();
        assert_eq!(o.monitors[0].highlighted, vec![0, 1]);
        e.drag_middle_click(); // off again
        let o = e.drag_update(&areas, inp(810)).unwrap_or_else(|| e.overlay_state(&areas));
        assert_eq!(o.monitors[0].highlighted, vec![1]);
        // without the setting the middle button acts as the secondary button
        e.config.settings.mouse_middle_click_spanning_multiple_zones = false;
        e.config.settings.mouse_switch = true;
        e.drag_middle_click();
        let o = e.drag_update(&areas, inp(820)).unwrap();
        assert!(o.monitors.is_empty());
    }

    #[test]
    fn display_change_moves_windows_back() {
        let (mut e, mut ws) = setup();
        e.snap_focused_to_zone(&ws, Some("A"), 1).unwrap();
        assert_eq!(ws.rect(1), Rect::new(500, 25, 1000, 825));
        // Dock appears at the bottom of A
        let a = ws.monitors.iter_mut().find(|m| m.id == "A").unwrap();
        a.work_area.bottom = 760;
        assert_eq!(e.on_display_change(&ws).unwrap(), 1);
        assert_eq!(ws.rect(1), Rect::new(500, 25, 1000, 760));
        // A disconnected: the window is now on B (same layout there) -> snapped there
        ws.monitors.retain(|m| m.id != "A");
        ws.windows.borrow_mut().insert(1, Rect::new(1200, 100, 1400, 300));
        assert_eq!(e.on_display_change(&ws).unwrap(), 1);
        assert_eq!(ws.rect(1), Rect::new(1500, 25, 2000, 825));
        assert_eq!(e.assignment(1).unwrap().monitor_id, "B");
        // closed windows are forgotten; setting off -> nothing moves
        e.config.settings.display_change_move_windows = false;
        assert_eq!(e.on_display_change(&ws).unwrap(), 0);
        ws.windows.borrow_mut().clear();
        e.config.settings.display_change_move_windows = true;
        assert_eq!(e.on_display_change(&ws).unwrap(), 0);
        assert!(e.assignment(1).is_none());
    }

    #[test]
    fn new_windows_last_zone_and_active_monitor() {
        let (mut e, ws) = setup();
        e.snap_focused_to_zone(&ws, Some("B"), 1).unwrap();
        e.forget_window(1);
        // window 1 of the same app re-opens at the old place: disabled by default
        ws.windows.borrow_mut().insert(1, Rect::new(100, 100, 400, 400));
        assert!(!e.on_window_created(&ws, &1, Some(Point::new(10, 10))).unwrap());
        e.config.settings.app_last_zone_move_windows = true;
        e.forget_window(1);
        assert!(e.on_window_created(&ws, &1, Some(Point::new(10, 10))).unwrap());
        assert_eq!(ws.rect(1), Rect::new(1500, 25, 2000, 825));
        // handled once
        ws.windows.borrow_mut().insert(1, Rect::new(100, 100, 400, 400));
        e.forget_window(1);
        e.seen_new.insert(1);
        assert!(!e.on_window_created(&ws, &1, Some(Point::new(10, 10))).unwrap());
        // no history -> monitor under the cursor (offset kept)
        e.config.settings.open_window_on_active_monitor = true;
        ws.windows.borrow_mut().insert(3, Rect::new(100, 100, 400, 400));
        assert!(e.on_window_created(&ws, &3, Some(Point::new(1500, 300))).unwrap());
        assert_eq!(ws.rect(3), Rect::new(1100, 100, 1400, 400));
        // already on the active monitor -> untouched
        ws.windows.borrow_mut().insert(4, Rect::new(100, 100, 400, 400));
        assert!(!e.on_window_created(&ws, &4, Some(Point::new(500, 300))).unwrap());
        // excluded apps are ignored
        e.config.settings.excluded_apps = vec!["app5.app".into()];
        ws.windows.borrow_mut().insert(5, Rect::new(100, 100, 400, 400));
        assert!(!e.on_window_created(&ws, &5, Some(Point::new(1500, 300))).unwrap());
        assert_eq!(ws.rect(5), Rect::new(100, 100, 400, 400));
    }

    #[test]
    fn window_switching_cycles_within_zone() {
        let (mut e, ws) = setup();
        for w in [1, 2, 3] {
            ws.windows.borrow_mut().insert(w, Rect::new(0, 0, 100, 100));
            ws.focused.set(Some(w));
            e.snap_focused_to_zone(&ws, Some("A"), 0).unwrap();
        }
        ws.windows.borrow_mut().insert(4, Rect::new(0, 0, 100, 100));
        ws.focused.set(Some(4));
        e.snap_focused_to_zone(&ws, Some("A"), 1).unwrap(); // other zone
        ws.focused.set(Some(1));
        assert!(e.cycle_windows(&ws, false).unwrap());
        assert_eq!(ws.focused.get(), Some(2));
        assert!(e.cycle_windows(&ws, false).unwrap());
        assert!(e.cycle_windows(&ws, false).unwrap());
        assert_eq!(ws.focused.get(), Some(1));
        assert!(e.cycle_windows(&ws, true).unwrap());
        assert_eq!(ws.focused.get(), Some(3));
        // a closed window is dismissed and skipped
        ws.windows.borrow_mut().remove(&1);
        assert!(e.cycle_windows(&ws, false).unwrap());
        assert_eq!(ws.focused.get(), Some(2));
        assert!(e.assignment(1).is_none());
        // alone in its zone / unsnapped -> nothing
        ws.focused.set(Some(4));
        assert!(!e.cycle_windows(&ws, false).unwrap());
    }

    #[test]
    fn monitor_rotation_moves_and_scales() {
        let (mut e, mut ws) = setup();
        // B is twice as large as A
        ws.monitors[0] = monitor("B", Rect::new(1000, 0, 3000, 1625));
        ws.windows.borrow_mut().insert(2, Rect::new(1000, 25, 2000, 825));
        e.snap_focused_to_zone(&ws, Some("A"), 0).unwrap();
        assert_eq!(e.rotate_windows(&ws, false).unwrap(), 2);
        assert_eq!(ws.rect(1), Rect::new(1000, 25, 2000, 1625)); // A zone 0 -> left half of B
        assert_eq!(ws.rect(2), Rect::new(0, 25, 500, 425)); // B top-left quarter -> A
        assert!(e.assignment(1).is_none());
        assert_eq!(e.rotate_windows(&ws, true).unwrap(), 2); // and back
        assert_eq!(ws.rect(2), Rect::new(1000, 25, 2000, 825));
        // preview shows window outlines per monitor
        let p = e.rotation_preview(&ws).unwrap();
        assert_eq!(p.monitors.len(), 2);
        assert_eq!(p.monitors.iter().map(|m| m.zones.len()).sum::<usize>(), 2);
        // excluded / single monitor
        e.config.settings.excluded_apps = vec!["app2".into()];
        assert_eq!(e.rotate_windows(&ws, false).unwrap(), 1);
        ws.monitors.truncate(1);
        assert_eq!(e.rotate_windows(&ws, false).unwrap(), 0);
    }

    #[test]
    fn excluded_and_child_windows_are_not_snapped() {
        let (mut e, ws) = setup();
        e.config.settings.excluded_apps = vec!["APP1.app".into()];
        assert!(!e.snap_focused(&ws, Direction::Right).unwrap());
        assert!(e.snap_focused_to_zone(&ws, None, 0).is_err());
        assert!(!e.is_processable(&ws, &1));
        e.config.settings.excluded_apps.clear();
        ws.kinds.borrow_mut().insert(1, WindowKind::Child);
        assert!(!e.snap_focused(&ws, Direction::Right).unwrap());
        e.config.settings.allow_snap_child_windows = true;
        assert!(e.snap_focused(&ws, Direction::Right).unwrap());
        ws.kinds.borrow_mut().insert(1, WindowKind::Popup);
        assert!(!e.is_processable(&ws, &1));
        e.config.settings.allow_snap_popup_windows = true;
        assert!(e.is_processable(&ws, &1));
        ws.kinds.borrow_mut().insert(1, WindowKind::Never);
        assert!(!e.is_processable(&ws, &1));
    }

    #[test]
    fn round_corner_style_follows_snapping() {
        let (mut e, ws) = setup();
        e.snap_focused_to_zone(&ws, None, 0).unwrap();
        assert!(ws.styled.borrow().is_empty());
        e.config.settings.disable_round_corners = true;
        e.snap_focused_to_zone(&ws, None, 1).unwrap();
        assert!(ws.styled.borrow()[&1]);
        e.unsnap_window(&ws, &1).unwrap();
        assert!(!ws.styled.borrow()[&1]);
    }

    #[test]
    fn layouts_per_virtual_desktop() {
        let (mut e, ws) = setup();
        ws.vds.borrow_mut().insert("A".into(), "d1".into());
        e.areas(&ws).unwrap();
        e.apply_layout("A", cols(4));
        assert_eq!(e.areas(&ws).unwrap()[0].layout.zones.len(), 4);
        // switch to a new desktop: inherits the last applied layout
        ws.vds.borrow_mut().insert("A".into(), "d2".into());
        assert_eq!(e.areas(&ws).unwrap()[0].layout.zones.len(), 4);
        e.apply_layout("A", cols(3));
        assert_eq!(e.areas(&ws).unwrap()[0].layout.zones.len(), 3);
        // back on d1: its own layout
        ws.vds.borrow_mut().insert("A".into(), "d1".into());
        assert_eq!(e.areas(&ws).unwrap()[0].layout.zones.len(), 4);
        assert_eq!(e.config.virtual_desktop_layouts["d2"]["A"].zone_count, 3);
        // disabled: one layout per monitor (the last applied)
        e.config.settings.layouts_per_virtual_desktop = false;
        assert_eq!(e.areas(&ws).unwrap()[0].layout.zones.len(), 3);
        // the config survives a JSON round trip
        let c = Config::from_json(&e.config.to_json().unwrap()).unwrap();
        assert_eq!(c.virtual_desktop_layouts, e.config.virtual_desktop_layouts);
    }

    #[test]
    fn span_overlay_is_split_per_monitor() {
        let (mut e, ws) = setup();
        e.config.settings.span_zones_across_monitors = true;
        let areas = e.work_areas(&ws.monitors);
        assert_eq!(areas[0].physical_frames.len(), 2);
        e.drag_start(1, ws.rect(1));
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(1500, 300), shift: true, ctrl: false }).unwrap();
        assert_eq!(o.monitors.len(), 2);
        assert!(o.monitors.iter().all(|m| m.zones.len() == 2 && m.highlighted == vec![1]));
        let frames: Vec<Rect> = o.monitors.iter().map(|m| m.frame).collect();
        assert!(frames.contains(&Rect::new(0, 0, 1000, 825)) && frames.contains(&Rect::new(1000, 0, 2000, 825)));
        assert_eq!(e.drag_end(&ws, &1, &areas).unwrap(), DragResult::Zones(vec![1]));
        assert_eq!(ws.rect(1), Rect::new(1000, 25, 2000, 825));
        // show on all monitors: both monitors drawn, only the current highlighted
        e.config.settings.span_zones_across_monitors = false;
        e.config.settings.show_zones_on_all_monitors = true;
        let areas = e.work_areas(&ws.monitors);
        e.drag_start(1, ws.rect(1));
        let o = e.drag_update(&areas, DragInput { cursor: Point::new(1300, 300), shift: true, ctrl: false }).unwrap();
        assert_eq!(o.monitors.len(), 2);
        assert_eq!(o.monitors[0].highlighted, Vec::<usize>::new());
        assert_eq!(o.monitors[1].highlighted, vec![0]);
    }

    #[test]
    fn label_placement() {
        let (e, _) = setup();
        let mut l = e.label_state(Rect::new(0, 0, 1000, 500), "x").unwrap();
        assert_eq!(l.chip_origin(100, 50), Point::new(450, 40));
        l.placement = LayoutNameLabelPlacement::BottomRight;
        assert_eq!(l.chip_origin(100, 50), Point::new(880, 410));
        l.placement = LayoutNameLabelPlacement::Center;
        assert_eq!(l.chip_origin(100, 50), Point::new(450, 225));
        assert!(e.label_state(Rect::new(0, 0, 10, 10), "").is_none());
    }
}
