//! Pure geometry: notch / expanded panel frames, the notch-shaped outline,
//! content layout and hit testing.
//!
//! All coordinates are global, top-left origin, y down, in points (the same
//! space as `CGEventGetLocation` on macOS and virtual-screen pixels on
//! Windows).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center_x(&self) -> f64 {
        self.x + self.w / 2.0
    }
    pub fn center_y(&self) -> f64 {
        self.y + self.h / 2.0
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    pub fn inflate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x - dx, self.y - dy, self.w + 2.0 * dx, self.h + 2.0 * dy)
    }
    pub fn offset(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn lerp(&self, o: &Rect, t: f64) -> Rect {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Rect::new(l(self.x, o.x), l(self.y, o.y), l(self.w, o.w), l(self.h, o.h))
    }
}

/// A screen as the platform layer reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenGeom {
    pub frame: Rect,
    /// The camera housing, if the screen has one.
    pub notch: Option<Rect>,
    /// Height of the menu bar (0 if unknown / hidden).
    pub menu_bar_h: f64,
}

/// Visual metrics, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub panel_width: f64,
    pub tile_w: f64,
    pub tile_h: f64,
    pub tile_gap: f64,
    pub thumb: f64,
    pub side_pad: f64,
    pub header_min_h: f64,
    pub strip_gap: f64,
    pub bottom_pad: f64,
    /// Concave "shoulder" flare where the panel meets the top edge.
    pub shoulder: f64,
    /// Bottom corner radius when collapsed (a bit larger than the
    /// hardware notch so the black shape always stays inside it).
    pub collapsed_radius: f64,
    pub expanded_radius: f64,
    pub virtual_notch_w: f64,
    pub peek_wing: f64,
    /// Transparent margin around the panel inside the window (shadow and
    /// spring overshoot).
    pub margin: f64,
    pub button: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            panel_width: 620.0,
            tile_w: 84.0,
            tile_h: 92.0,
            tile_gap: 6.0,
            thumb: 56.0,
            side_pad: 18.0,
            header_min_h: 30.0,
            strip_gap: 8.0,
            bottom_pad: 14.0,
            shoulder: 10.0,
            collapsed_radius: 12.0,
            expanded_radius: 28.0,
            virtual_notch_w: 190.0,
            peek_wing: 78.0,
            margin: 36.0,
            button: 26.0,
        }
    }
}

/// The resting (collapsed) shape on a screen: the notch itself, or a
/// top-center "virtual notch" pill when allowed. `None` if the screen gets
/// no shelf.
pub fn collapsed_rect(s: &ScreenGeom, m: &Metrics, allow_virtual: bool) -> Option<Rect> {
    if let Some(n) = s.notch {
        if n.w > 0.0 && n.h > 0.0 {
            return Some(n);
        }
    }
    if !allow_virtual {
        return None;
    }
    let h = if s.menu_bar_h >= 18.0 { s.menu_bar_h } else { 24.0 };
    let w = m.virtual_notch_w.min(s.frame.w * 0.5);
    Some(Rect::new(s.frame.center_x() - w / 2.0, s.frame.y, w, h))
}

pub fn header_height(collapsed: &Rect, m: &Metrics) -> f64 {
    collapsed.h.max(m.header_min_h)
}

/// The fully expanded panel, growing out of `collapsed` and kept on screen.
pub fn expanded_rect(collapsed: &Rect, screen: &Rect, m: &Metrics) -> Rect {
    let h = header_height(collapsed, m) + m.strip_gap + m.tile_h + m.bottom_pad;
    // Room for the title and the buttons on either side of the notch.
    let min_w = collapsed.w + 2.0 * 150.0;
    let w = m.panel_width.max(min_w).min(screen.w - 2.0 * (m.shoulder + 4.0));
    let lo = screen.x + m.shoulder + 4.0;
    let hi = screen.right() - m.shoulder - 4.0 - w;
    let x = (collapsed.center_x() - w / 2.0).clamp(lo, hi.max(lo));
    Rect::new(x, screen.y, w, h)
}

/// A "peek" shape: the notch widened into small ears, for status flashes.
pub fn peek_rect(collapsed: &Rect, m: &Metrics) -> Rect {
    Rect::new(collapsed.x - m.peek_wing, collapsed.y, collapsed.w + 2.0 * m.peek_wing, collapsed.h)
}

/// The (always fixed) window frame: the expanded panel plus a margin for
/// the shadow and overshoot, flush with the top of the screen.
pub fn window_rect(expanded: &Rect, screen: &Rect, m: &Metrics) -> Rect {
    let x0 = (expanded.x - m.margin).max(screen.x);
    let x1 = (expanded.right() + m.margin).min(screen.right());
    let y1 = (expanded.bottom() + m.margin).min(screen.bottom());
    Rect::new(x0, screen.y, x1 - x0, y1 - screen.y)
}

/// Region that counts as "near the notch" for an incoming drag.
pub fn drag_zone(expanded: &Rect, screen: &Rect) -> Rect {
    let x0 = (expanded.x - 80.0).max(screen.x);
    let x1 = (expanded.right() + 80.0).min(screen.right());
    Rect::new(x0, screen.y, x1 - x0, expanded.h + 50.0)
}

/// Area that keeps an expanded panel open (a little hysteresis).
pub fn keep_open_zone(expanded: &Rect, m: &Metrics) -> Rect {
    let r = expanded.inflate(m.shoulder + 6.0, 0.0);
    Rect::new(r.x, r.y - 4.0, r.w, r.h + 14.0)
}

/// Area that triggers hover-to-expand while collapsed.
pub fn hover_zone(collapsed: &Rect) -> Rect {
    Rect::new(collapsed.x - 4.0, collapsed.y - 2.0, collapsed.w + 8.0, collapsed.h + 6.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeParams {
    pub rect: Rect,
    pub shoulder: f64,
    pub radius: f64,
}

impl ShapeParams {
    pub fn lerp(&self, o: &ShapeParams, t: f64) -> ShapeParams {
        let l = |a: f64, b: f64| a + (b - a) * t;
        // Allow overshoot of the rect (springy) but keep the radii sane.
        ShapeParams { rect: self.rect.lerp(&o.rect, t), shoulder: l(self.shoulder, o.shoulder).max(0.0), radius: l(self.radius, o.radius).max(0.0) }
    }
}

pub fn collapsed_shape(c: &Rect, m: &Metrics) -> ShapeParams {
    ShapeParams { rect: *c, shoulder: 0.0, radius: m.collapsed_radius.min(c.h / 2.0) }
}

pub fn expanded_shape(e: &Rect, m: &Metrics) -> ShapeParams {
    ShapeParams { rect: *e, shoulder: m.shoulder, radius: m.expanded_radius }
}

pub fn peek_shape(p: &Rect, m: &Metrics) -> ShapeParams {
    ShapeParams { rect: *p, shoulder: m.shoulder * 0.8, radius: (p.h / 2.0).min(m.collapsed_radius + 4.0) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEl {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

/// Circle-approximation constant for cubic Béziers.
const KAPPA: f64 = 0.552_284_75;

/// The notch outline: flat top edge flush with the screen top, concave
/// shoulders flaring into it, straight sides and rounded bottom corners.
/// Coordinates are offset by `-origin` (window-local).
pub fn notch_path(s: &ShapeParams, origin: Point) -> Vec<PathEl> {
    let r = s.rect.offset(-origin.x, -origin.y);
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let w = r.w.max(0.0);
    let h = r.h.max(0.0);
    let sh = s.shoulder.min(h / 3.0).max(0.0);
    let rad = s.radius.min(w / 2.0).min((h - sh).max(0.0)).max(0.0);
    let k = KAPPA;
    let p = Point::new;
    let mut v = Vec::with_capacity(10);
    v.push(PathEl::Move(p(x0 - sh, y0)));
    // Left shoulder (concave), centered at (x0 - sh, y0 + sh).
    v.push(PathEl::Cubic(p(x0 - sh + k * sh, y0), p(x0, y0 + sh - k * sh), p(x0, y0 + sh)));
    v.push(PathEl::Line(p(x0, y1 - rad)));
    // Bottom-left corner (convex).
    v.push(PathEl::Cubic(p(x0, y1 - rad + k * rad), p(x0 + rad - k * rad, y1), p(x0 + rad, y1)));
    v.push(PathEl::Line(p(x1 - rad, y1)));
    v.push(PathEl::Cubic(p(x1 - rad + k * rad, y1), p(x1, y1 - rad + k * rad), p(x1, y1 - rad)));
    v.push(PathEl::Line(p(x1, y0 + sh)));
    // Right shoulder.
    v.push(PathEl::Cubic(p(x1, y0 + sh - k * sh), p(x1 + sh - k * sh, y0), p(x1 + sh, y0)));
    v.push(PathEl::Close);
    v
}

/// Header buttons on the right of the notch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    DragAll,
    Share,
    Clear,
}

pub const BUTTONS: [Button; 3] = [Button::DragAll, Button::Share, Button::Clear];

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub panel: Rect,
    pub notch: Rect,
    pub header: Rect,
    /// Left of the notch: icon + title + count.
    pub title: Rect,
    pub buttons: Vec<(Button, Rect)>,
    /// Visible tile strip (clips tiles).
    pub strip: Rect,
    pub content_w: f64,
    pub max_scroll: f64,
    pub scroll: f64,
    /// Tile frames, already scrolled.
    pub tiles: Vec<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileParts {
    pub thumb: Rect,
    pub label: Rect,
    pub close: Rect,
}

pub fn tile_parts(tile: &Rect, m: &Metrics) -> TileParts {
    let thumb = Rect::new(tile.center_x() - m.thumb / 2.0, tile.y + 8.0, m.thumb, m.thumb);
    let label = Rect::new(tile.x + 3.0, thumb.bottom() + 7.0, tile.w - 6.0, 16.0);
    let c = 18.0;
    let close = Rect::new(thumb.right() - c / 2.0 - 1.0, thumb.y - c / 2.0 + 1.0, c, c);
    TileParts { thumb, label, close }
}

pub fn clamp_scroll(scroll: f64, max: f64) -> f64 {
    scroll.clamp(0.0, max.max(0.0))
}

pub fn layout(expanded: &Rect, notch: &Rect, n_tiles: usize, scroll: f64, m: &Metrics) -> Layout {
    let hh = header_height(notch, m);
    let header = Rect::new(expanded.x, expanded.y, expanded.w, hh);
    let left0 = expanded.x + m.side_pad + 4.0;
    let left1 = (notch.x - 12.0).max(left0);
    let title = Rect::new(left0, header.y, left1 - left0, hh);
    let right1 = expanded.right() - m.side_pad - 2.0;
    let by = header.y + (hh - m.button) / 2.0;
    let mut buttons = Vec::with_capacity(BUTTONS.len());
    let mut x = right1;
    for b in BUTTONS.iter().rev() {
        x -= m.button;
        buttons.push((*b, Rect::new(x, by, m.button, m.button)));
        x -= 6.0;
    }
    buttons.reverse();
    let strip = Rect::new(expanded.x + m.side_pad - 4.0, header.bottom() + m.strip_gap, expanded.w - 2.0 * (m.side_pad - 4.0), m.tile_h);
    let content_w = if n_tiles == 0 { 0.0 } else { n_tiles as f64 * m.tile_w + (n_tiles - 1) as f64 * m.tile_gap };
    let max_scroll = (content_w - strip.w).max(0.0);
    let scroll = clamp_scroll(scroll, max_scroll);
    // Few tiles: centered; many: start at the left edge and scroll.
    let start = if content_w <= strip.w { strip.center_x() - content_w / 2.0 } else { strip.x - scroll };
    let tiles = (0..n_tiles).map(|i| Rect::new(start + i as f64 * (m.tile_w + m.tile_gap), strip.y, m.tile_w, m.tile_h)).collect();
    Layout { panel: *expanded, notch: *notch, header, title, buttons, strip, content_w, max_scroll, scroll, tiles }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Outside,
    Panel,
    Title,
    Button(Button),
    Strip,
    Tile(usize),
    Close(usize),
}

/// Hit test the expanded panel. `close_for` is the tile whose close badge
/// is showing (it may poke out of the tile).
pub fn hit_test(l: &Layout, p: Point, close_for: Option<usize>, m: &Metrics) -> Hit {
    if !l.panel.contains(p) {
        return Hit::Outside;
    }
    if l.strip.contains(p) {
        if let Some(i) = close_for {
            if let Some(t) = l.tiles.get(i) {
                if tile_parts(t, m).close.inflate(3.0, 3.0).contains(p) {
                    return Hit::Close(i);
                }
            }
        }
        if let Some(i) = l.tiles.iter().position(|t| t.contains(p)) {
            return Hit::Tile(i);
        }
        return Hit::Strip;
    }
    if let Some((b, _)) = l.buttons.iter().find(|(_, r)| r.inflate(2.0, 2.0).contains(p)) {
        return Hit::Button(*b);
    }
    if l.title.contains(p) {
        return Hit::Title;
    }
    Hit::Panel
}

/// Tiles whose frames intersect a rubber-band rectangle.
pub fn tiles_in(l: &Layout, r: &Rect) -> Vec<usize> {
    l.tiles.iter().enumerate().filter(|(_, t)| t.intersects(r) && t.intersects(&l.strip)).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mbp14() -> ScreenGeom {
        ScreenGeom { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), notch: Some(Rect::new(663.5, 0.0, 185.0, 32.0)), menu_bar_h: 32.0 }
    }

    #[test]
    fn collapsed_matches_notch_or_virtual() {
        let m = Metrics::default();
        assert_eq!(collapsed_rect(&mbp14(), &m, false), Some(Rect::new(663.5, 0.0, 185.0, 32.0)));
        let ext = ScreenGeom { frame: Rect::new(1512.0, -200.0, 2560.0, 1440.0), notch: None, menu_bar_h: 25.0 };
        assert_eq!(collapsed_rect(&ext, &m, false), None);
        let v = collapsed_rect(&ext, &m, true).unwrap();
        assert_eq!(v.center_x(), ext.frame.center_x());
        assert_eq!(v.y, -200.0);
        assert_eq!(v.h, 25.0);
        // Hidden menu bar: a sane default height.
        let hidden = ScreenGeom { menu_bar_h: 0.0, ..ext };
        assert_eq!(collapsed_rect(&hidden, &m, true).unwrap().h, 24.0);
    }

    #[test]
    fn expanded_is_centered_and_on_screen() {
        let m = Metrics::default();
        let s = mbp14();
        let c = s.notch.unwrap();
        let e = expanded_rect(&c, &s.frame, &m);
        assert!((e.center_x() - c.center_x()).abs() < 1e-9);
        assert_eq!(e.y, 0.0);
        assert_eq!(e.w, 620.0);
        assert_eq!(e.h, 32.0 + m.strip_gap + m.tile_h + m.bottom_pad);
        // Narrow screen: clamped inside.
        let narrow = Rect::new(0.0, 0.0, 500.0, 800.0);
        let c2 = Rect::new(150.0, 0.0, 200.0, 24.0);
        let e2 = expanded_rect(&c2, &narrow, &m);
        assert!(e2.x >= narrow.x + m.shoulder && e2.right() <= narrow.right() - m.shoulder);
        // Off-center collapsed rect near an edge stays on screen.
        let c3 = Rect::new(10.0, 0.0, 100.0, 24.0);
        let e3 = expanded_rect(&c3, &s.frame, &m);
        assert!(e3.x >= m.shoulder);
    }

    #[test]
    fn window_contains_panel_with_margin() {
        let m = Metrics::default();
        let s = mbp14();
        let e = expanded_rect(&s.notch.unwrap(), &s.frame, &m);
        let w = window_rect(&e, &s.frame, &m);
        assert_eq!(w.y, 0.0);
        assert!(w.x <= e.x - m.shoulder && w.right() >= e.right() + m.shoulder);
        assert!(w.bottom() >= e.bottom() + 20.0);
        let p = peek_rect(&s.notch.unwrap(), &m);
        assert!(w.x <= p.x && w.right() >= p.right());
    }

    #[test]
    fn path_is_closed_and_bounded() {
        let m = Metrics::default();
        let e = Rect::new(100.0, 0.0, 600.0, 146.0);
        let sp = expanded_shape(&e, &m);
        let path = notch_path(&sp, Point::new(50.0, 0.0));
        assert_eq!(path.first(), Some(&PathEl::Move(Point::new(100.0 - 50.0 - m.shoulder, 0.0))));
        assert_eq!(path.last(), Some(&PathEl::Close));
        let bounds = Rect::new(50.0 - m.shoulder - 1e-9, -1e-9, 600.0 + 2.0 * m.shoulder + 2e-9, 146.0 + 2e-9);
        for el in &path {
            let pts: Vec<Point> = match *el {
                PathEl::Move(a) | PathEl::Line(a) => vec![a],
                PathEl::Cubic(a, b, c) => vec![a, b, c],
                PathEl::Close => vec![],
            };
            for p in pts {
                assert!(p.x >= bounds.x && p.x <= bounds.right() && p.y >= bounds.y && p.y <= bounds.bottom(), "{p:?}");
            }
        }
        // Collapsed: no shoulders, so nothing leaves the notch rect.
        let c = collapsed_shape(&Rect::new(663.5, 0.0, 185.0, 32.0), &m);
        for el in notch_path(&c, Point::default()) {
            if let PathEl::Move(p) | PathEl::Line(p) = el {
                assert!(p.x >= 663.5 && p.x <= 848.5 && p.y >= 0.0 && p.y <= 32.0);
            }
        }
        // Degenerate sizes don't produce NaN.
        let z = ShapeParams { rect: Rect::new(0.0, 0.0, 0.0, 0.0), shoulder: 10.0, radius: 10.0 };
        for el in notch_path(&z, Point::default()) {
            if let PathEl::Move(p) | PathEl::Line(p) = el {
                assert!(p.x.is_finite() && p.y.is_finite());
            }
        }
    }

    #[test]
    fn shape_interpolation_overshoots_rect_but_not_radii() {
        let m = Metrics::default();
        let a = collapsed_shape(&Rect::new(663.5, 0.0, 185.0, 32.0), &m);
        let b = expanded_shape(&Rect::new(446.0, 0.0, 620.0, 146.0), &m);
        let mid = a.lerp(&b, 0.5);
        assert!((mid.rect.center_x() - 756.0).abs() < 1e-9);
        let over = a.lerp(&b, 1.05);
        assert!(over.rect.w > b.rect.w);
        let under = a.lerp(&b, -0.5);
        assert!(under.shoulder >= 0.0 && under.radius >= 0.0);
    }

    #[test]
    fn layout_centers_few_tiles_and_scrolls_many() {
        let m = Metrics::default();
        let s = mbp14();
        let n = s.notch.unwrap();
        let e = expanded_rect(&n, &s.frame, &m);
        let l = layout(&e, &n, 3, 50.0, &m);
        assert_eq!(l.max_scroll, 0.0);
        assert_eq!(l.scroll, 0.0);
        let mid = (l.tiles[0].x + l.tiles[2].right()) / 2.0;
        assert!((mid - l.strip.center_x()).abs() < 1e-9);
        // Title stays left of the notch, buttons right of it.
        assert!(l.title.right() <= n.x);
        for (_, r) in &l.buttons {
            assert!(r.x >= n.right() && r.right() <= e.right());
        }
        let many = layout(&e, &n, 20, 1e9, &m);
        assert!(many.max_scroll > 0.0);
        assert_eq!(many.scroll, many.max_scroll);
        assert!((many.tiles[19].right() - many.strip.right()).abs() < 1e-9);
        let start = layout(&e, &n, 20, -5.0, &m);
        assert_eq!(start.tiles[0].x, start.strip.x);
    }

    #[test]
    fn hit_testing() {
        let m = Metrics::default();
        let s = mbp14();
        let n = s.notch.unwrap();
        let e = expanded_rect(&n, &s.frame, &m);
        let l = layout(&e, &n, 2, 0.0, &m);
        let c = |r: Rect| Point::new(r.center_x(), r.center_y());
        assert_eq!(hit_test(&l, Point::new(5.0, 500.0), None, &m), Hit::Outside);
        assert_eq!(hit_test(&l, c(l.tiles[1]), None, &m), Hit::Tile(1));
        let close = tile_parts(&l.tiles[0], &m).close;
        assert_eq!(hit_test(&l, c(close), Some(0), &m), Hit::Close(0));
        assert_eq!(hit_test(&l, c(close), None, &m), Hit::Tile(0));
        for (b, r) in &l.buttons {
            assert_eq!(hit_test(&l, c(*r), None, &m), Hit::Button(*b));
        }
        assert_eq!(hit_test(&l, Point::new(l.strip.x + 2.0, l.strip.center_y()), None, &m), Hit::Strip);
        assert_eq!(hit_test(&l, c(l.title), None, &m), Hit::Title);
        assert_eq!(hit_test(&l, Point::new(e.center_x(), e.bottom() - 3.0), None, &m), Hit::Panel);
        // Rubber band.
        let band = Rect::new(l.tiles[0].x + 5.0, l.strip.y, l.tiles[1].x - l.tiles[0].x, 10.0);
        assert_eq!(tiles_in(&l, &band), vec![0, 1]);
    }

    #[test]
    fn zones() {
        let m = Metrics::default();
        let s = mbp14();
        let n = s.notch.unwrap();
        let e = expanded_rect(&n, &s.frame, &m);
        let dz = drag_zone(&e, &s.frame);
        assert!(dz.contains(Point::new(n.center_x(), 5.0)));
        assert!(!dz.contains(Point::new(n.center_x(), 400.0)));
        assert!(hover_zone(&n).contains(Point::new(n.center_x(), 0.0)));
        assert!(!hover_zone(&n).contains(Point::new(n.center_x(), 60.0)));
        let k = keep_open_zone(&e, &m);
        assert!(k.contains(Point::new(e.x - 5.0, 10.0)));
        assert!(k.contains(Point::new(e.center_x(), e.bottom() + 8.0)));
    }
}
