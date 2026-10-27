//! Pure layout of a shelf panel: its size in each mode, the fanned stack,
//! the grid of tiles, the header buttons, and hit testing. Coordinates are
//! panel-local, top-left origin, in points.

use pm_shelf_core::geom::{Point, Rect};

/// What a shelf shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// The items as a fanned stack with a caption (Dropover's look).
    #[default]
    Stack,
    /// Every item as a tile, to drag or act on one.
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub stack_w: f64,
    pub stack_h: f64,
    pub header_h: f64,
    pub radius: f64,
    /// Transparent border of the window around the panel (shadow, spring).
    pub margin: f64,
    pub button: f64,
    /// Largest side of a card in the stack.
    pub card: f64,
    pub tile_w: f64,
    pub tile_h: f64,
    pub tile_gap: f64,
    pub thumb: f64,
    pub pad: f64,
    pub max_cols: usize,
    pub max_rows: usize,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            stack_w: 208.0,
            stack_h: 236.0,
            header_h: 40.0,
            radius: 26.0,
            margin: 40.0,
            button: 24.0,
            card: 104.0,
            tile_w: 84.0,
            tile_h: 92.0,
            tile_gap: 6.0,
            thumb: 56.0,
            pad: 14.0,
            max_cols: 4,
            max_rows: 3,
        }
    }
}

/// One card of the stack: its box and rotation (radians, clockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    pub rect: Rect,
    pub angle: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Layout {
    pub w: f64,
    pub h: f64,
    pub close: Rect,
    pub toggle: Rect,
    pub menu: Rect,
    /// The header title (grid mode).
    pub title: Rect,
    /// The area of the stack (drag it to drag everything out).
    pub stack: Rect,
    /// Back to front; the last one is the newest item.
    pub cards: Vec<Card>,
    pub badge: Rect,
    pub caption: Rect,
    pub subcaption: Rect,
    /// The dashed drop zone of an empty shelf.
    pub empty: Rect,
    /// The visible area of the grid.
    pub grid: Rect,
    /// Tiles in grid order (scrolled).
    pub tiles: Vec<Rect>,
    pub scroll: f64,
    pub max_scroll: f64,
}

/// Columns of the grid for `n` items.
pub fn grid_cols(n: usize, m: &Metrics) -> usize {
    n.clamp(2, m.max_cols.max(2))
}

/// Panel size for `n` items in `mode`.
pub fn panel_size(mode: Mode, n: usize, m: &Metrics) -> (f64, f64) {
    match mode {
        Mode::Stack => (m.stack_w, m.stack_h),
        Mode::Grid => {
            let cols = grid_cols(n, m);
            let rows = n.div_ceil(cols).max(1);
            let vis = rows.min(m.max_rows) as f64;
            let w = (2.0 * m.pad + cols as f64 * m.tile_w + (cols as f64 - 1.0) * m.tile_gap).max(m.stack_w);
            let h = m.header_h + vis * m.tile_h + (vis - 1.0) * m.tile_gap + m.pad;
            (w, h.max(m.header_h + m.tile_h + m.pad))
        }
    }
}

/// Fan of up to three cards for `n` items, centered at `c`.
pub fn fan(n: usize, c: Point, card: f64) -> Vec<Card> {
    let deg = std::f64::consts::PI / 180.0;
    let r = |dx: f64, dy: f64, s: f64, a: f64| Card { rect: Rect::new(c.x + dx - card * s / 2.0, c.y + dy - card * s / 2.0, card * s, card * s), angle: a * deg };
    match n {
        0 => vec![],
        1 => vec![r(0.0, 0.0, 1.0, 0.0)],
        2 => vec![r(-16.0, 4.0, 0.88, -10.0), r(5.0, 0.0, 1.0, 2.0)],
        _ => vec![r(-22.0, 6.0, 0.84, -13.0), r(22.0, 5.0, 0.86, 11.0), r(0.0, 0.0, 1.0, -1.0)],
    }
}

pub fn layout(mode: Mode, n: usize, scroll: f64, m: &Metrics) -> Layout {
    let (w, h) = panel_size(mode, n, m);
    let b = m.button;
    let by = (m.header_h - b) / 2.0 + 1.0;
    let close = Rect::new(12.0, by, b, b);
    let menu = Rect::new(w - 12.0 - b, by, b, b);
    let toggle = Rect::new(menu.x - 4.0 - b, by, b, b);
    let title = Rect::new(close.right() + 8.0, by, (toggle.x - 8.0 - close.right() - 8.0).max(0.0), b);
    let mut l = Layout { w, h, close, toggle, menu, title, ..Default::default() };
    match mode {
        Mode::Stack => {
            let caption_h = 54.0;
            l.stack = Rect::new(m.pad, m.header_h, w - 2.0 * m.pad, h - m.header_h - caption_h);
            let c = Point::new(w / 2.0, l.stack.center_y() + 2.0);
            l.cards = fan(n, c, m.card);
            if let Some(front) = l.cards.last() {
                let bw = 28.0;
                l.badge = Rect::new(front.rect.right() - bw / 2.0 - 6.0, front.rect.y - 8.0, bw, 20.0);
            }
            l.caption = Rect::new(m.pad, h - caption_h + 6.0, w - 2.0 * m.pad, 18.0);
            l.subcaption = Rect::new(m.pad, l.caption.bottom() + 2.0, w - 2.0 * m.pad, 15.0);
            l.empty = Rect::new(m.pad, m.header_h, w - 2.0 * m.pad, h - m.header_h - m.pad);
        }
        Mode::Grid => {
            let cols = grid_cols(n, m);
            let rows = n.div_ceil(cols).max(1);
            l.grid = Rect::new(0.0, m.header_h, w, h - m.header_h - m.pad + m.tile_gap);
            let content_h = rows as f64 * m.tile_h + (rows as f64 - 1.0) * m.tile_gap;
            l.max_scroll = (content_h - (h - m.header_h - m.pad)).max(0.0);
            l.scroll = scroll.clamp(0.0, l.max_scroll);
            let row_w = cols as f64 * m.tile_w + (cols as f64 - 1.0) * m.tile_gap;
            let x0 = (w - row_w) / 2.0;
            l.tiles = (0..n)
                .map(|i| {
                    let (r, c) = (i / cols, i % cols);
                    Rect::new(x0 + c as f64 * (m.tile_w + m.tile_gap), m.header_h + r as f64 * (m.tile_h + m.tile_gap) - l.scroll, m.tile_w, m.tile_h)
                })
                .collect();
            l.empty = Rect::new(m.pad, m.header_h, w - 2.0 * m.pad, h - m.header_h - m.pad);
        }
    }
    l
}

/// Thumbnail, label and remove-button frames inside a tile at the origin.
pub struct TileParts {
    pub thumb: Rect,
    pub label: Rect,
    pub remove: Rect,
}

pub fn tile_parts(m: &Metrics) -> TileParts {
    let t = m.thumb;
    TileParts {
        thumb: Rect::new((m.tile_w - t) / 2.0, 9.0, t, t),
        label: Rect::new(4.0, 9.0 + t + 7.0, m.tile_w - 8.0, 15.0),
        remove: Rect::new(m.tile_w - 22.0, 3.0, 18.0, 18.0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Outside,
    /// The panel, but no control (moves the shelf).
    Background,
    Close,
    Toggle,
    Menu,
    /// The stack (drag everything / click to expand).
    Stack,
    Tile(usize),
    /// The remove badge of tile `k` (shown while it is hovered).
    Remove(usize),
}

/// What is under `p`. `hover_tile` is the tile whose remove badge is shown.
pub fn hit_test(l: &Layout, mode: Mode, p: Point, hover_tile: Option<usize>, m: &Metrics) -> Hit {
    if !Rect::new(0.0, 0.0, l.w, l.h).contains(p) {
        return Hit::Outside;
    }
    for (r, h) in [(l.close, Hit::Close), (l.toggle, Hit::Toggle), (l.menu, Hit::Menu)] {
        if r.inflate(3.0, 3.0).contains(p) {
            return h;
        }
    }
    match mode {
        Mode::Stack => {
            if l.cards.iter().any(|c| c.rect.inflate(6.0, 6.0).contains(p)) {
                return Hit::Stack;
            }
        }
        Mode::Grid => {
            if l.grid.contains(p) {
                if let Some(k) = hover_tile {
                    if let Some(t) = l.tiles.get(k) {
                        let parts = tile_parts(m);
                        if parts.remove.offset(t.x, t.y).inflate(2.0, 2.0).contains(p) {
                            return Hit::Remove(k);
                        }
                    }
                }
                if let Some(k) = l.tiles.iter().position(|t| t.contains(p)) {
                    return Hit::Tile(k);
                }
            }
        }
    }
    Hit::Background
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let m = Metrics::default();
        assert_eq!(panel_size(Mode::Stack, 0, &m), (208.0, 236.0));
        assert_eq!(panel_size(Mode::Stack, 30, &m), (208.0, 236.0));
        // Grid: at least the stack's width; 4 columns at most; 3 rows visible.
        let (w2, h2) = panel_size(Mode::Grid, 2, &m);
        assert_eq!(w2, 208.0);
        assert_eq!(h2, 40.0 + 92.0 + 14.0);
        let (w4, _) = panel_size(Mode::Grid, 4, &m);
        assert_eq!(w4, 28.0 + 4.0 * 84.0 + 3.0 * 6.0);
        let (w9, h9) = panel_size(Mode::Grid, 9, &m);
        assert_eq!(w9, w4);
        assert_eq!(h9, 40.0 + 3.0 * 92.0 + 2.0 * 6.0 + 14.0);
        assert_eq!(panel_size(Mode::Grid, 40, &m), (w9, h9));
    }

    #[test]
    fn stack_fans_and_hits() {
        let m = Metrics::default();
        for n in 0..5 {
            let l = layout(Mode::Stack, n, 0.0, &m);
            assert_eq!(l.cards.len(), n.min(3));
            for c in &l.cards {
                assert!(c.rect.x >= 0.0 && c.rect.right() <= l.w && c.rect.y >= m.header_h - 12.0, "{n}: {c:?}");
            }
        }
        let l = layout(Mode::Stack, 3, 0.0, &m);
        assert!(l.cards[0].angle < 0.0 && l.cards[1].angle > 0.0, "back cards lean apart");
        let front = l.cards[2].rect;
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(front.center_x(), front.center_y()), None, &m), Hit::Stack);
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(l.close.center_x(), l.close.center_y()), None, &m), Hit::Close);
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(l.toggle.center_x(), l.toggle.center_y()), None, &m), Hit::Toggle);
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(l.menu.center_x(), l.menu.center_y()), None, &m), Hit::Menu);
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(l.w / 2.0, l.h - 6.0), None, &m), Hit::Background);
        assert_eq!(hit_test(&l, Mode::Stack, Point::new(-1.0, 5.0), None, &m), Hit::Outside);
        assert!(l.toggle.right() < l.menu.x && l.close.right() < l.toggle.x);
    }

    #[test]
    fn grid_tiles_scroll_and_hits() {
        let m = Metrics::default();
        let l = layout(Mode::Grid, 14, 0.0, &m);
        assert_eq!(l.tiles.len(), 14);
        // 4 columns, centered.
        assert_eq!(l.tiles[0].y, l.tiles[3].y);
        assert_eq!(l.tiles[4].x, l.tiles[0].x);
        assert!((l.tiles[0].x - (l.w - l.tiles[3].right())).abs() < 1e-9);
        // 4 rows of which 3 are visible: one row of scrolling.
        assert_eq!(l.max_scroll, m.tile_h + m.tile_gap);
        let s = layout(Mode::Grid, 14, 1e9, &m);
        assert_eq!(s.scroll, s.max_scroll);
        assert_eq!(s.tiles[13].bottom(), l.tiles[13].bottom() - s.max_scroll);
        let t5 = l.tiles[5];
        assert_eq!(hit_test(&l, Mode::Grid, Point::new(t5.center_x(), t5.center_y()), None, &m), Hit::Tile(5));
        let parts = tile_parts(&m);
        let rm = parts.remove.offset(t5.x, t5.y);
        let p = Point::new(rm.center_x(), rm.center_y());
        assert_eq!(hit_test(&l, Mode::Grid, p, Some(5), &m), Hit::Remove(5));
        assert_eq!(hit_test(&l, Mode::Grid, p, None, &m), Hit::Tile(5));
        // Between tiles: background.
        let gap = Point::new(l.tiles[0].right() + m.tile_gap / 2.0, l.tiles[0].center_y());
        assert_eq!(hit_test(&l, Mode::Grid, gap, None, &m), Hit::Background);
        // Two items: still the stack's width, tiles centered.
        let two = layout(Mode::Grid, 2, 0.0, &m);
        assert!((two.tiles[0].x - (two.w - two.tiles[1].right())).abs() < 1e-9);
    }
}
