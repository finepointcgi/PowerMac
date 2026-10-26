//! Platform independent FancyZones engine: work areas, window zone
//! assignment, keyboard snapping, drag sessions and "last zone" history.
//! The OS specific parts are behind the [`WindowSystem`] trait.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{AppZoneHistoryEntry, Config};
use crate::edge_snap::{self, EdgeAction};
use crate::geometry::{Point, Rect};
use crate::layout::{AppliedLayout, Layout, LayoutType};
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

/// OS window management backend.
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
}

/// A monitor with its instantiated layout.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkArea {
    pub monitor: Monitor,
    pub applied: AppliedLayout,
    pub layout: Layout,
}

/// Where a window is currently snapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub monitor_id: String,
    pub layout_uuid: String,
    pub layout_kind: LayoutType,
    pub zones: Vec<usize>,
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
        }
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
            return vec![self.make_work_area(m)];
        }
        let rects: Vec<Rect> = monitors.iter().map(|m| m.work_area).collect();
        navigation::order_monitors(&rects)
            .into_iter()
            .map(|i| self.make_work_area(monitors[i].clone()))
            .collect()
    }

    fn make_work_area(&self, monitor: Monitor) -> WorkArea {
        let applied = self.config.layout_for(&monitor.id);
        let layout = match Layout::new(&applied, monitor.work_area, &self.config.custom_layouts) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("layout for monitor {} failed: {e}", monitor.id);
                Layout::blank(monitor.work_area)
            }
        };
        WorkArea { monitor, applied, layout }
    }

    pub fn apply_layout(&mut self, monitor_id: &str, layout: AppliedLayout) {
        self.config.applied_layouts.insert(monitor_id.to_string(), layout);
        // Assignments on that monitor are no longer meaningful.
        self.assignments.retain(|_, a| a.monitor_id != monitor_id);
    }

    pub fn assignment(&self, w: WindowKey) -> Option<&Assignment> {
        self.assignments.get(&w)
    }

    pub fn forget_window(&mut self, w: WindowKey) {
        self.assignments.remove(&w);
        self.pre_snap_size.remove(&w);
        self.edge_snapped.remove(&w);
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
        let area = &areas[wa];
        let Some(rect) = area.layout.combined_zones_rect(zones) else {
            return Ok(false);
        };
        let key = ws.window_key(w);
        self.remember_pre_snap_size(ws, w, key);
        ws.set_window_rect(w, rect)?;
        self.edge_snapped.remove(&key);
        let mut zs = zones.to_vec();
        zs.sort_unstable();
        zs.dedup();
        let asg = Assignment {
            monitor_id: area.monitor.id.clone(),
            layout_uuid: area.layout.uuid.clone(),
            layout_kind: area.layout.kind,
            zones: zs.clone(),
        };
        if let Some(app) = ws.window_app_id(w) {
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
        let monitors = ws.monitors()?;
        let areas = self.work_areas(&monitors);
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
        let monitors = ws.monitors()?;
        let areas = self.work_areas(&monitors);
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
        let monitors = ws.monitors()?;
        let areas = self.work_areas(&monitors);
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
        let monitors = ws.monitors()?;
        let areas = self.work_areas(&monitors);
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

    /// Update the drag; returns `Some(overlay)` when what should be drawn changed.
    pub fn drag_update(&mut self, areas: &[WorkArea], input: DragInput) -> Option<OverlayState> {
        let toggle = self.drag.as_ref()?.secondary_toggle;
        let active = self.zones_should_show(input.shift, toggle);
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
                if input.ctrl {
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

    /// The overlay for the current drag (empty when zones are not shown).
    pub fn overlay_state(&self, areas: &[WorkArea]) -> OverlayState {
        let s = &self.config.settings;
        let c = |h: &str, d: (u8, u8, u8)| crate::config::parse_hex_color(h).unwrap_or(d);
        let mut st = OverlayState {
            monitors: vec![],
            edge_preview: None,
            show_numbers: s.show_zone_number,
            zone_color: c(&s.zone_color, (0xAA, 0xCD, 0xFF)),
            border_color: c(&s.zone_border_color, (0xFF, 0xFF, 0xFF)),
            highlight_color: c(&s.zone_highlight_color, (0x00, 0x8C, 0xFF)),
            number_color: c(&s.zone_number_color, (0, 0, 0)),
            opacity: (s.zone_highlight_opacity.min(100) as f64) / 100.0,
        };
        let Some(d) = &self.drag else { return st };
        if !d.zones_active {
            if let (Some((_, rect)), Some(i)) = (d.edge, d.work_area) {
                st.show_numbers = false;
                st.edge_preview = Some(rect);
                st.monitors.push(OverlayMonitor { frame: areas[i].monitor.frame, zones: vec![rect], highlighted: vec![0] });
            }
            return st;
        }
        for (i, a) in areas.iter().enumerate() {
            let is_cur = d.work_area == Some(i);
            if is_cur || s.show_zones_on_all_monitors {
                st.monitors.push(OverlayMonitor {
                    frame: a.monitor.frame,
                    zones: a.layout.zones.clone(),
                    highlighted: if is_cur { d.highlighted.clone() } else { vec![] },
                });
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
    use std::cell::RefCell;

    /// In-memory window system for tests.
    pub struct MockWs {
        pub monitors: Vec<Monitor>,
        pub windows: RefCell<HashMap<u64, Rect>>,
        pub focused: Option<u64>,
    }

    impl WindowSystem for MockWs {
        type Window = u64;
        fn monitors(&self) -> Result<Vec<Monitor>> {
            Ok(self.monitors.clone())
        }
        fn focused_window(&self) -> Result<Option<u64>> {
            Ok(self.focused)
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
            Some(format!("app{w}"))
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
        let ws = MockWs {
            monitors: vec![monitor("B", Rect::new(1000, 0, 2000, 825)), monitor("A", Rect::new(0, 0, 1000, 825))],
            windows: RefCell::new(HashMap::from([(1, Rect::new(100, 100, 400, 400))])),
            focused: Some(1),
        };
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
        assert_eq!(e.config.app_zone_history["app1"].zones, vec![0]);
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
}
