//! Canvas (free-form) layout editing: geometry ported from the FancyZones
//! editor's `CanvasZone.xaml.cs` (`SnappyHelperMagnetic`) and
//! `CanvasLayoutModel.cs`, plus the egui canvas editor.
//!
//! Zones are kept in work-area pixels; `ref-width` / `ref-height` of the
//! saved layout are the work area size it was edited on (like FancyZones).

use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Ui};
use fancyzones::layout::{CanvasLayoutInfo, CanvasZone};

use crate::widgets::ACCENT;

/// `CanvasZone.MinZoneWidth` / `MinZoneHeight`.
pub const MIN_W: i32 = 64;
pub const MIN_H: i32 = 72;
/// `CanvasLayoutModel`: first zone offset, offset between new zones, size.
const DEFAULT_OFFSET: i32 = 100;
const OFFSET_SHIFT: i32 = 50;
const ZONE_SIZE_MULTIPLIER: f64 = 0.4;

/// Which edge(s) of a zone a drag moves along one axis (`ResizeMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// The left / top edge (`BottomEdge` in FancyZones' naming).
    Low,
    /// The right / bottom edge (`TopEdge`).
    High,
    /// The whole zone (`BothEdges`): moving.
    Both,
}

/// Part of a zone being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Move,
    N,
    S,
    W,
    E,
    NW,
    NE,
    SW,
    SE,
}

impl Handle {
    /// (x, y) tracking of this handle.
    pub fn tracks(self) -> (Option<Track>, Option<Track>) {
        use Handle::*;
        use Track::*;
        match self {
            Move => (Some(Both), Some(Both)),
            N => (None, Some(Low)),
            S => (None, Some(High)),
            W => (Some(Low), None),
            E => (Some(High), None),
            NW => (Some(Low), Some(Low)),
            NE => (Some(High), Some(Low)),
            SW => (Some(Low), Some(High)),
            SE => (Some(High), Some(High)),
        }
    }

    fn cursor(self) -> CursorIcon {
        use Handle::*;
        match self {
            Move => CursorIcon::Grab,
            N | S => CursorIcon::ResizeVertical,
            W | E => CursorIcon::ResizeHorizontal,
            NW | SE => CursorIcon::ResizeNwSe,
            NE | SW => CursorIcon::ResizeNeSw,
        }
    }
}

/// `SnappyHelperBase` + `SnappyHelperMagnetic` for one axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapper {
    snaps: Vec<i32>,
    magnets: Vec<i32>,
    min: i32,
    max: i32,
    start: i32,
    pub mode: Track,
    magnetic: bool,
}

fn axis(z: &CanvasZone, is_x: bool) -> (i32, i32) {
    if is_x {
        (z.x, z.width)
    } else {
        (z.y, z.height)
    }
}

impl Snapper {
    /// Snap points are the other zones' edges and the screen edges (and, when
    /// moving, the positions where this zone's far edge would touch them).
    pub fn new(zones: &[CanvasZone], index: usize, is_x: bool, mode: Track, screen: i32, magnetic: bool) -> Self {
        let (pos, size) = axis(&zones[index], is_x);
        let min_size = if is_x { MIN_W } else { MIN_H };
        let mut keys = Vec::new();
        for (i, z) in zones.iter().enumerate() {
            if i == index {
                continue;
            }
            let (p, s) = axis(z, is_x);
            keys.push(p);
            keys.push(p + s);
            if mode == Track::Both {
                keys.push(p - size);
                keys.push(p + s - size);
            }
        }
        keys.push(0);
        keys.push(screen);
        if mode == Track::Both {
            keys.push(-size);
            keys.push(screen - size);
        }
        keys.sort_unstable();
        keys.dedup();
        let max_magnet = (0.08 * screen as f64) as i32;
        // FancyZones uses 0 / the screen size as the outer neighbours, which
        // gives the screen edges themselves no magnet at all; give them a
        // full-size one instead.
        let magnets = (0..keys.len())
            .map(|i| {
                let prev = if i == 0 { keys[0] - 2 * max_magnet } else { keys[i - 1] };
                let next = if i == keys.len() - 1 { keys[i] + 2 * max_magnet } else { keys[i + 1] };
                (keys[i] - prev).min((next - keys[i]).min(max_magnet)) / 2
            })
            .collect();
        let (min, max, start) = match mode {
            Track::Low => (0, pos + size - min_size, pos),
            Track::High => (pos + min_size, screen, pos + size),
            Track::Both => (0, screen - size, pos),
        };
        Self { snaps: keys, magnets, min, max: max.max(min), start, mode, magnetic }
    }

    /// `Move(delta)`: the tracked edge's position after dragging by `delta`.
    pub fn position(&self, delta: i32) -> i32 {
        let free = self.start + delta;
        let mut p = free;
        if self.magnetic {
            if let Some(i) = (0..self.snaps.len()).find(|&i| (free - self.snaps[i]).abs() <= self.magnets[i]) {
                let (k, m) = (self.snaps[i], self.magnets[i]);
                let dead = (m + 1) / 2;
                p = if (free - k).abs() <= dead {
                    k
                } else if free < k {
                    free + (free - (k - m))
                } else {
                    free - ((k + m) - free)
                };
            }
        }
        p.clamp(self.min, self.max)
    }
}

/// Apply a tracked edge `position` to one axis of `z` (`UpdateFromSnappyHelpers`).
fn apply_axis(z: &mut CanvasZone, is_x: bool, mode: Track, position: i32) {
    let (p, s) = if is_x { (&mut z.x, &mut z.width) } else { (&mut z.y, &mut z.height) };
    match mode {
        Track::Low => {
            let change = position - *p;
            *p += change;
            *s -= change;
        }
        Track::High => *s = position - *p,
        Track::Both => *p = position,
    }
}

/// Zone `index` of `zones` after dragging `handle` by (`dx`, `dy`) pixels on a
/// `screen` (w, h) work area.
pub fn drag_zone(zones: &[CanvasZone], index: usize, handle: Handle, dx: i32, dy: i32, screen: (i32, i32), magnetic: bool) -> CanvasZone {
    let mut z = zones[index];
    let (tx, ty) = handle.tracks();
    if let Some(m) = tx {
        let s = Snapper::new(zones, index, true, m, screen.0, magnetic);
        apply_axis(&mut z, true, m, s.position(dx));
    }
    if let Some(m) = ty {
        let s = Snapper::new(zones, index, false, m, screen.1, magnetic);
        apply_axis(&mut z, false, m, s.position(dy));
    }
    z
}

/// Keep a zone on screen with at least the minimum size.
pub fn clamp_zone(z: CanvasZone, screen: (i32, i32)) -> CanvasZone {
    let w = z.width.clamp(MIN_W.min(screen.0), screen.0.max(1));
    let h = z.height.clamp(MIN_H.min(screen.1), screen.1.max(1));
    CanvasZone { x: z.x.clamp(0, (screen.0 - w).max(0)), y: z.y.clamp(0, (screen.1 - h).max(0)), width: w, height: h }
}

/// `CanvasLayoutModel.ScaleLayout`: zones of `info` on a `to` (w, h) work area.
pub fn scale_zones(info: &CanvasLayoutInfo, to: (i32, i32)) -> Vec<CanvasZone> {
    if info.ref_width <= 0 || info.ref_height <= 0 || (info.ref_width, info.ref_height) == to {
        return info.zones.clone();
    }
    let (sx, sy) = (to.0 as f64 / info.ref_width as f64, to.1 as f64 / info.ref_height as f64);
    info.zones
        .iter()
        .map(|z| CanvasZone {
            x: (z.x as f64 * sx) as i32,
            y: (z.y as f64 * sy) as i32,
            width: (z.width as f64 * sx) as i32,
            height: (z.height as f64 * sy) as i32,
        })
        .collect()
}

/// `CanvasLayoutModel.AddNewZone`: the next new zone, cascading from the
/// top-left. `top_left` is the editor's running offset.
pub fn new_zone(zones: &[CanvasZone], top_left: &mut i32, screen: (i32, i32)) -> CanvasZone {
    if zones.is_empty() {
        *top_left = DEFAULT_OFFSET;
    } else if zones.last().map(|z| z.x) == Some(*top_left) {
        *top_left += OFFSET_SHIFT;
    }
    let w = (screen.0 as f64 * ZONE_SIZE_MULTIPLIER) as i32;
    let h = (screen.1 as f64 * ZONE_SIZE_MULTIPLIER) as i32;
    if *top_left + w >= screen.0 || *top_left + h >= screen.1 {
        *top_left = DEFAULT_OFFSET;
    }
    let z = clamp_zone(CanvasZone { x: *top_left, y: *top_left, width: w, height: h }, screen);
    *top_left += OFFSET_SHIFT;
    z
}

// ---------------------------------------------------------------------------
// UI

/// The delete button of a zone drawn at `r`.
fn close_rect(r: Rect) -> Rect {
    Rect::from_min_size(egui::pos2(r.right() - 24.0, r.top() + 4.0), egui::vec2(20.0, 20.0))
}

/// Topmost zone (in drawing `order`) and handle under `p`; `None` as the
/// handle means its delete button.
fn hit_test(zones: &[CanvasZone], order: &[usize], zr: &impl Fn(&CanvasZone) -> Rect, p: Pos2) -> Option<(usize, Option<Handle>)> {
    for &i in order.iter().rev() {
        let r = zr(&zones[i]);
        if !r.expand(4.0).contains(p) {
            continue;
        }
        if close_rect(r).contains(p) {
            return Some((i, None));
        }
        let e = 8.0_f32.min(r.width() / 4.0).min(r.height() / 4.0);
        let (l, rt, t, b) = (p.x < r.left() + e, p.x > r.right() - e, p.y < r.top() + e, p.y > r.bottom() - e);
        let h = match (l, rt, t, b) {
            (true, _, true, _) => Handle::NW,
            (_, true, true, _) => Handle::NE,
            (true, _, _, true) => Handle::SW,
            (_, true, _, true) => Handle::SE,
            (true, ..) => Handle::W,
            (_, true, ..) => Handle::E,
            (_, _, true, _) => Handle::N,
            (.., true) => Handle::S,
            _ => Handle::Move,
        };
        return Some((i, Some(h)));
    }
    None
}

#[derive(Debug, Clone)]
struct Drag {
    index: usize,
    handle: Handle,
    base: Vec<CanvasZone>,
    start: Pos2,
}

/// The canvas zone editor (state for one editing session).
#[derive(Debug, Clone)]
pub struct CanvasEditor {
    pub zones: Vec<CanvasZone>,
    /// Work area (w, h) in pixels; saved as `ref-width` / `ref-height`.
    pub work: (i32, i32),
    pub sensitivity_radius: i32,
    pub selected: Option<usize>,
    /// Magnetic snapping to zone and screen edges.
    pub magnetic: bool,
    top_left: i32,
    drag: Option<Drag>,
    /// Where the editor was last drawn.
    pub last_rect: Option<Rect>,
}

impl CanvasEditor {
    pub fn new(info: &CanvasLayoutInfo, work: (i32, i32)) -> Self {
        let work = (work.0.max(1), work.1.max(1));
        let zones = scale_zones(info, work).into_iter().map(|z| clamp_zone(z, work)).collect();
        Self { zones, work, sensitivity_radius: info.sensitivity_radius, selected: None, magnetic: true, top_left: DEFAULT_OFFSET, drag: None, last_rect: None }
    }

    /// A new canvas layout with one zone (like the FancyZones editor).
    pub fn new_layout(work: (i32, i32)) -> Self {
        let info = CanvasLayoutInfo {
            ref_width: work.0.max(1),
            ref_height: work.1.max(1),
            zones: vec![],
            sensitivity_radius: fancyzones::layout::defaults::SENSITIVITY_RADIUS,
        };
        let mut e = Self::new(&info, work);
        e.add_zone();
        e
    }

    pub fn add_zone(&mut self) {
        let z = new_zone(&self.zones, &mut self.top_left, self.work);
        self.zones.push(z);
        self.selected = Some(self.zones.len() - 1);
    }

    pub fn delete_zone(&mut self, i: usize) {
        if i < self.zones.len() {
            self.zones.remove(i);
            self.selected = None;
            self.drag = None;
        }
    }

    pub fn to_info(&self) -> CanvasLayoutInfo {
        CanvasLayoutInfo { ref_width: self.work.0, ref_height: self.work.1, zones: self.zones.clone(), sensitivity_radius: self.sensitivity_radius }
    }

    /// Indices in drawing order (selected zone on top).
    fn draw_order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.zones.len()).filter(|&i| Some(i) != self.selected).collect();
        if let Some(s) = self.selected.filter(|&s| s < self.zones.len()) {
            v.push(s);
        }
        v
    }

    pub fn show(&mut self, ui: &mut Ui, rect: Rect) {
        self.last_rect = Some(rect);
        let resp = ui.interact(rect, ui.scope_id().with("canvas-editor"), Sense::click_and_drag());
        let s = rect.width() / self.work.0 as f32;
        let zr = |z: &CanvasZone| {
            Rect::from_min_size(rect.min + egui::vec2(z.x as f32 * s, z.y as f32 * s), egui::vec2(z.width as f32 * s, z.height as f32 * s))
        };
        let hit = |zones: &[CanvasZone], order: &[usize], p: Pos2| hit_test(zones, order, &zr, p);
        if resp.drag_started() {
            // Where the button went down (the pointer has moved since).
            if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                if let Some((i, Some(h))) = hit(&self.zones, &self.draw_order(), p) {
                    self.selected = Some(i);
                    self.drag = Some(Drag { index: i, handle: h, base: self.zones.clone(), start: p });
                }
            }
        }
        if resp.dragged() {
            if let (Some(d), Some(p)) = (&self.drag, resp.interact_pointer_pos()) {
                let dx = ((p.x - d.start.x) / s).round() as i32;
                let dy = ((p.y - d.start.y) / s).round() as i32;
                // Alt temporarily turns snapping off.
                let magnetic = self.magnetic && !ui.input(|i| i.modifiers.alt);
                if d.index < self.zones.len() {
                    self.zones[d.index] = drag_zone(&d.base, d.index, d.handle, dx, dy, self.work, magnetic);
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if resp.clicked() {
            match resp.interact_pointer_pos().and_then(|p| hit(&self.zones, &self.draw_order(), p)) {
                Some((i, None)) => self.delete_zone(i),
                Some((i, Some(_))) => self.selected = Some(i),
                None => self.selected = None,
            }
        }
        if !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            if let Some(i) = self.selected {
                self.delete_zone(i);
            }
        }

        // --- drawing
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
        let hover = resp.hover_pos().and_then(|p| hit(&self.zones, &self.draw_order(), p));
        for i in self.draw_order() {
            let z = self.zones[i];
            let r = zr(&z);
            let sel = self.selected == Some(i);
            let fill = if sel { Color32::from_rgba_unmultiplied(0x2B, 0x88, 0xD8, 170) } else { Color32::from_rgba_unmultiplied(0x2B, 0x88, 0xD8, 90) };
            painter.rect(r, 4.0, fill, Stroke::new(if sel { 2.5 } else { 1.5 }, ACCENT), egui::StrokeKind::Inside);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(28.0), visuals.text_color());
            painter.text(
                r.center() + egui::vec2(0.0, 24.0),
                egui::Align2::CENTER_CENTER,
                format!("{} × {}", z.width, z.height),
                egui::FontId::proportional(12.0),
                visuals.weak_text_color(),
            );
            if sel || hover.map(|h| h.0) == Some(i) {
                let c = close_rect(r);
                let hot = hover == Some((i, None));
                painter.rect_filled(c, 4.0, if hot { crate::widgets::ERROR } else { visuals.widgets.inactive.bg_fill });
                painter.text(c.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(16.0), visuals.strong_text_color());
                for p in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()] {
                    painter.rect_filled(Rect::from_center_size(p, egui::vec2(8.0, 8.0)), 1.0, ACCENT);
                }
            }
        }
        if self.zones.is_empty() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "No zones — use “Add zone”", egui::FontId::proportional(16.0), visuals.weak_text_color());
        }
        let cursor = match &self.drag {
            Some(d) => Some(if d.handle == Handle::Move { CursorIcon::Grabbing } else { d.handle.cursor() }),
            None => match hover {
                Some((_, Some(h))) => Some(h.cursor()),
                Some((_, None)) => Some(CursorIcon::PointingHand),
                None => None,
            },
        };
        if let Some(c) = cursor {
            ui.ctx().set_cursor_icon(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancyzones::layout::{canvas_zones, CustomLayout, CustomLayoutInfo};
    use fancyzones::Rect as FzRect;

    const SCREEN: (i32, i32) = (1920, 1080);

    fn z(x: i32, y: i32, w: i32, h: i32) -> CanvasZone {
        CanvasZone { x, y, width: w, height: h }
    }

    #[test]
    fn move_without_snapping_clamps_to_screen() {
        let zones = [z(100, 100, 400, 300)];
        let m = drag_zone(&zones, 0, Handle::Move, 50, 20, SCREEN, false);
        assert_eq!(m, z(150, 120, 400, 300));
        assert_eq!(drag_zone(&zones, 0, Handle::Move, -500, -500, SCREEN, false), z(0, 0, 400, 300));
        assert_eq!(drag_zone(&zones, 0, Handle::Move, 5000, 5000, SCREEN, false), z(1520, 780, 400, 300));
    }

    #[test]
    fn move_snaps_to_screen_edges() {
        let zones = [z(100, 100, 400, 300)];
        // x screen magnet: keys include 0; next key is 400 (= screen-size pairs) so
        // magnet = min(0-0, ...) -> 0 at 0? check a point near the far edge instead.
        let m = drag_zone(&zones, 0, Handle::Move, 1515 - 100, 0, SCREEN, true);
        assert_eq!(m.x, 1520, "right edge snaps to the screen edge");
        let m = drag_zone(&zones, 0, Handle::Move, -95, -96, SCREEN, true);
        assert_eq!((m.x, m.y), (0, 0), "near the top-left corner snaps into it");
    }

    #[test]
    fn move_snaps_to_other_zone_edges() {
        let zones = [z(0, 0, 960, 1080), z(1100, 200, 400, 300)];
        // Dragging zone 1 left so its left edge nears 960 (zone 0's right edge).
        let m = drag_zone(&zones, 1, Handle::Move, -130, 0, SCREEN, true);
        assert_eq!(m.x, 960);
        // Further away: free movement.
        let m = drag_zone(&zones, 1, Handle::Move, -60, 0, SCREEN, true);
        assert_eq!(m.x, 1040);
    }

    #[test]
    fn magnet_outside_dead_zone_is_continuous() {
        let s = Snapper::new(&[z(0, 0, 960, 1080), z(1200, 0, 300, 300)], 1, true, Track::Low, 1920, true);
        // Key 960; neighbours 0 and 1920 -> magnet = min(960, min(960, 153))/2 = 76, dead = 38.
        assert_eq!(s.position(960 - 1200 + 30), 960);
        let free = 960 + 60;
        assert_eq!(s.position(free - 1200), free - ((960 + 76) - free));
        assert_eq!(s.position(960 + 100 - 1200), 1060);
    }

    #[test]
    fn resize_handles_and_min_size() {
        let zones = [z(100, 100, 400, 300)];
        assert_eq!(drag_zone(&zones, 0, Handle::SE, 100, 50, SCREEN, false), z(100, 100, 500, 350));
        assert_eq!(drag_zone(&zones, 0, Handle::NW, -50, 20, SCREEN, false), z(50, 120, 450, 280));
        assert_eq!(drag_zone(&zones, 0, Handle::E, -1000, 999, SCREEN, false), z(100, 100, MIN_W, 300));
        assert_eq!(drag_zone(&zones, 0, Handle::N, 0, 1000, SCREEN, false), z(100, 400 - MIN_H, 400, MIN_H));
        assert_eq!(drag_zone(&zones, 0, Handle::W, -1000, 0, SCREEN, false), z(0, 100, 500, 300));
        assert_eq!(drag_zone(&zones, 0, Handle::S, 0, 5000, SCREEN, false), z(100, 100, 400, 980));
        assert_eq!(drag_zone(&zones, 0, Handle::NE, 50, -50, SCREEN, false), z(100, 50, 450, 350));
        assert_eq!(drag_zone(&zones, 0, Handle::SW, 50, 50, SCREEN, false), z(150, 100, 350, 350));
        // Resizing snaps the dragged edge to the screen edge.
        assert_eq!(drag_zone(&zones, 0, Handle::E, 1915 - 500, 0, SCREEN, true).width, 1820);
    }

    #[test]
    fn clamp_and_scale() {
        assert_eq!(clamp_zone(z(-10, 2000, 10, 5000), SCREEN), z(0, 0, MIN_W, 1080));
        assert_eq!(clamp_zone(z(1900, 10, 100, 100), SCREEN), z(1820, 10, 100, 100));
        let info = CanvasLayoutInfo { ref_width: 1000, ref_height: 500, zones: vec![z(0, 0, 500, 500), z(500, 250, 500, 250)], sensitivity_radius: 20 };
        assert_eq!(scale_zones(&info, (2000, 1000)), vec![z(0, 0, 1000, 1000), z(1000, 500, 1000, 500)]);
        assert_eq!(scale_zones(&info, (1000, 500)), info.zones);
    }

    #[test]
    fn new_zones_cascade_like_powertoys() {
        let mut tl = 0;
        let mut zones = vec![];
        let a = new_zone(&zones, &mut tl, SCREEN);
        assert_eq!(a, z(100, 100, 768, 432));
        zones.push(a);
        let b = new_zone(&zones, &mut tl, SCREEN);
        assert_eq!(b, z(150, 150, 768, 432));
        zones.push(b);
        for _ in 0..20 {
            let n = new_zone(&zones, &mut tl, SCREEN);
            assert!(n.x + n.width < SCREEN.0 && n.y + n.height < SCREEN.1, "{n:?}");
            zones.push(n);
        }
        let e = CanvasEditor::new_layout(SCREEN);
        assert_eq!(e.zones, vec![z(100, 100, 768, 432)]);
    }

    #[test]
    fn editor_info_matches_fancyzones() {
        let mut e = CanvasEditor::new_layout((1440, 900));
        e.add_zone();
        e.zones[1] = drag_zone(&e.zones, 1, Handle::Move, 400, 0, e.work, false);
        let info = e.to_info();
        assert_eq!((info.ref_width, info.ref_height), (1440, 900));
        let l = CustomLayout { uuid: "{X}".into(), name: "c".into(), info: CustomLayoutInfo::Canvas(info.clone()) };
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["type"], "canvas");
        assert_eq!(v["info"]["ref-width"], 1440);
        assert_eq!(v["info"]["zones"][1]["X"], 550);
        assert_eq!(v["info"]["zones"][1]["Y"], 150);
        assert_eq!(v["info"]["zones"][1]["width"], 576);
        // FancyZones scales it to the real work area.
        let r = canvas_zones(2880, 1800, &info).unwrap();
        assert_eq!(r[0], FzRect::new(200, 200, 200 + 1152, 200 + 720));
        e.delete_zone(0);
        assert_eq!(e.zones.len(), 1);
        e.delete_zone(5);
        assert_eq!(e.zones.len(), 1);
    }
}
