//! Tile layout: fit N window thumbnails into a centered panel on a screen.
//!
//! Tiles keep their window's aspect ratio (within limits) and share one
//! thumbnail height. Tiles flow left to right into rows that are centered in
//! the panel; the thumbnail height shrinks step by step until every row fits
//! within the screen. If even the minimum height doesn't fit, only the rows
//! that fit are laid out (see [`Layout::tiles`]).

use std::ops::Range;

use crate::model::RectF;

/// Geometry knobs (points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    /// Panel edge to the tiles.
    pub padding: f64,
    /// Gap between tiles (both directions).
    pub spacing: f64,
    /// Tile edge to its thumbnail / label.
    pub tile_padding: f64,
    /// Gap between thumbnail and label row.
    pub label_gap: f64,
    /// Height of the icon + title row.
    pub label_height: f64,
    pub max_thumb_h: f64,
    pub min_thumb_h: f64,
    /// Thumbnail widths are clamped to `[min_aspect, max_aspect] * height`.
    pub min_aspect: f64,
    pub max_aspect: f64,
    /// Minimum tile width so short titles stay readable.
    pub min_tile_w: f64,
    /// Fraction of the screen the panel may use.
    pub max_w_frac: f64,
    pub max_h_frac: f64,
}

impl LayoutParams {
    /// Thumbnail mode.
    pub const THUMBNAILS: LayoutParams = LayoutParams {
        padding: 18.0,
        spacing: 8.0,
        tile_padding: 10.0,
        label_gap: 8.0,
        label_height: 20.0,
        max_thumb_h: 168.0,
        min_thumb_h: 64.0,
        min_aspect: 0.75,
        max_aspect: 1.9,
        min_tile_w: 150.0,
        max_w_frac: 0.92,
        max_h_frac: 0.82,
    };

    /// App-icon mode (no Screen Recording permission or thumbnails off):
    /// square icon areas.
    pub const ICONS: LayoutParams = LayoutParams {
        padding: 18.0,
        spacing: 8.0,
        tile_padding: 12.0,
        label_gap: 8.0,
        label_height: 20.0,
        max_thumb_h: 104.0,
        min_thumb_h: 48.0,
        min_aspect: 1.0,
        max_aspect: 1.0,
        min_tile_w: 156.0,
        max_w_frac: 0.92,
        max_h_frac: 0.82,
    };
}

/// One laid-out tile, in panel-local coordinates (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub frame: RectF,
    /// Where the thumbnail (or big icon) goes.
    pub thumb: RectF,
    /// The icon + title row.
    pub label: RectF,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Panel frame in global coordinates, centered on the screen.
    pub panel: RectF,
    /// One tile per item that fits (normally all of them).
    pub tiles: Vec<Tile>,
    /// Tile index ranges per row.
    pub rows: Vec<Range<usize>>,
    pub thumb_h: f64,
}

/// Panel size when there's nothing to show (a "No windows" message).
pub const EMPTY_PANEL: (f64, f64) = (300.0, 96.0);

/// Arrow-key direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

fn tile_width(aspect: f64, h: f64, p: &LayoutParams) -> f64 {
    let thumb_w = (aspect.clamp(p.min_aspect, p.max_aspect) * h).round();
    (thumb_w + 2.0 * p.tile_padding).max(p.min_tile_w)
}

fn tile_height(h: f64, p: &LayoutParams) -> f64 {
    h + 2.0 * p.tile_padding + p.label_gap + p.label_height
}

/// Greedy row wrapping; returns row ranges and each row's width.
fn wrap(widths: &[f64], max_w: f64, spacing: f64) -> Vec<(Range<usize>, f64)> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut w = 0.0;
    for (i, &tw) in widths.iter().enumerate() {
        let next = if i == start { tw } else { w + spacing + tw };
        if i > start && next > max_w {
            rows.push((start..i, w));
            start = i;
            w = tw;
        } else {
            w = next;
        }
    }
    if start < widths.len() {
        rows.push((start..widths.len(), w));
    }
    rows
}

/// Lay out tiles for windows with the given aspect ratios (width / height)
/// on `screen` (global coordinates).
pub fn compute(aspects: &[f64], screen: RectF, p: &LayoutParams) -> Layout {
    if aspects.is_empty() {
        let (w, h) = EMPTY_PANEL;
        return Layout { panel: centered(screen, w, h), tiles: vec![], rows: vec![], thumb_h: 0.0 };
    }
    let max_inner_w = (screen.w * p.max_w_frac - 2.0 * p.padding).max(p.min_tile_w);
    let max_inner_h = (screen.h * p.max_h_frac - 2.0 * p.padding).max(tile_height(p.min_thumb_h, p));

    let mut h = p.max_thumb_h;
    let (rows, widths) = loop {
        let widths: Vec<f64> = aspects.iter().map(|&a| tile_width(a, h, p).min(max_inner_w)).collect();
        let rows = wrap(&widths, max_inner_w, p.spacing);
        let total_h = rows.len() as f64 * tile_height(h, p) + (rows.len().saturating_sub(1)) as f64 * p.spacing;
        if total_h <= max_inner_h || h <= p.min_thumb_h {
            break (rows, widths);
        }
        h = (h * 0.92).floor().max(p.min_thumb_h);
    };

    // Keep only the rows that fit (only possible at the minimum height).
    let th = tile_height(h, p);
    let max_rows = (((max_inner_h + p.spacing) / (th + p.spacing)).floor() as usize).max(1);
    let rows: Vec<_> = rows.into_iter().take(max_rows).collect();

    let inner_w = rows.iter().map(|(_, w)| *w).fold(0.0, f64::max);
    let inner_h = rows.len() as f64 * th + rows.len().saturating_sub(1) as f64 * p.spacing;
    let panel_w = (inner_w + 2.0 * p.padding).ceil();
    let panel_h = (inner_h + 2.0 * p.padding).ceil();

    let mut tiles = Vec::new();
    let mut out_rows = Vec::new();
    for (r, (range, row_w)) in rows.iter().enumerate() {
        let y = p.padding + r as f64 * (th + p.spacing);
        let mut x = ((panel_w - row_w) / 2.0).round();
        for &tw in &widths[range.clone()] {
            let frame = RectF::new(x, y, tw, th);
            let inner = frame.inset(p.tile_padding);
            let thumb = RectF::new(inner.x, inner.y, inner.w, h);
            let label = RectF::new(inner.x, inner.y + h + p.label_gap, inner.w, p.label_height);
            tiles.push(Tile { frame, thumb, label });
            x += tw + p.spacing;
        }
        out_rows.push(range.clone());
    }
    Layout { panel: centered(screen, panel_w, panel_h), tiles, rows: out_rows, thumb_h: h }
}

fn centered(screen: RectF, w: f64, h: f64) -> RectF {
    RectF::new((screen.x + (screen.w - w) / 2.0).round(), (screen.y + (screen.h - h) / 2.0).round(), w, h)
}

impl Layout {
    /// Tile under a panel-local point.
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.tiles.iter().position(|t| t.frame.contains(x, y))
    }

    /// Tile under a global point.
    pub fn hit_global(&self, x: f64, y: f64) -> Option<usize> {
        self.hit(x - self.panel.x, y - self.panel.y)
    }

    fn row_of(&self, i: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(&i))
    }

    /// The tile reached from `i` with an arrow key. Left/Right wrap through
    /// the whole list; Up/Down move to the closest tile (by center x) in the
    /// row above/below, wrapping between the first and last row.
    pub fn neighbor(&self, i: usize, dir: Dir) -> usize {
        let n = self.tiles.len();
        if n == 0 {
            return 0;
        }
        let i = i.min(n - 1);
        match dir {
            Dir::Right => (i + 1) % n,
            Dir::Left => (i + n - 1) % n,
            Dir::Up | Dir::Down => {
                let Some(r) = self.row_of(i) else { return i };
                let nr = self.rows.len();
                if nr < 2 {
                    return i;
                }
                let target = if dir == Dir::Down { (r + 1) % nr } else { (r + nr - 1) % nr };
                let cx = self.tiles[i].frame.center().0;
                self.rows[target]
                    .clone()
                    .min_by(|&a, &b| {
                        let da = (self.tiles[a].frame.center().0 - cx).abs();
                        let db = (self.tiles[b].frame.center().0 - cx).abs();
                        da.total_cmp(&db)
                    })
                    .unwrap_or(i)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: RectF = RectF::new(0.0, 0.0, 1440.0, 900.0);

    fn assert_inside(l: &Layout, screen: RectF) {
        assert!(l.panel.x >= screen.x && l.panel.right() <= screen.right(), "panel {:?} wider than screen", l.panel);
        assert!(l.panel.y >= screen.y && l.panel.bottom() <= screen.bottom());
        for t in &l.tiles {
            assert!(t.frame.x >= 0.0 && t.frame.right() <= l.panel.w + 0.5, "tile {:?} outside panel {:?}", t.frame, l.panel);
            assert!(t.frame.y >= 0.0 && t.frame.bottom() <= l.panel.h + 0.5);
            assert!(t.thumb.y >= t.frame.y && t.label.bottom() <= t.frame.bottom() + 1e-9);
        }
        // No overlaps.
        for (i, a) in l.tiles.iter().enumerate() {
            for b in &l.tiles[i + 1..] {
                assert_eq!(a.frame.intersection_area(&b.frame), 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn empty() {
        let l = compute(&[], SCREEN, &LayoutParams::THUMBNAILS);
        assert!(l.tiles.is_empty());
        assert_eq!((l.panel.w, l.panel.h), EMPTY_PANEL);
        assert_eq!(l.panel.center(), SCREEN.center());
    }

    #[test]
    fn few_windows_one_row_full_size_centered() {
        let l = compute(&[1.6, 1.6, 1.0], SCREEN, &LayoutParams::THUMBNAILS);
        assert_eq!(l.rows, vec![0..3]);
        assert_eq!(l.thumb_h, LayoutParams::THUMBNAILS.max_thumb_h);
        assert_eq!(l.tiles.len(), 3);
        // Wider window, wider tile.
        assert!(l.tiles[0].frame.w > l.tiles[2].frame.w);
        // Panel centered on the screen.
        let (cx, cy) = l.panel.center();
        assert!((cx - 720.0).abs() <= 1.0 && (cy - 450.0).abs() <= 1.0);
        assert_inside(&l, SCREEN);
    }

    #[test]
    fn many_windows_wrap_and_shrink() {
        for n in [1usize, 2, 5, 9, 14, 25, 40, 60] {
            let aspects: Vec<f64> = (0..n).map(|i| [1.6, 1.33, 0.8, 2.2][i % 4]).collect();
            for screen in [SCREEN, RectF::new(-1920.0, -200.0, 1920.0, 1080.0), RectF::new(0.0, 0.0, 1024.0, 640.0)] {
                for p in [LayoutParams::THUMBNAILS, LayoutParams::ICONS] {
                    let l = compute(&aspects, screen, &p);
                    assert_inside(&l, screen);
                    assert!(l.thumb_h <= p.max_thumb_h && l.thumb_h >= p.min_thumb_h);
                    assert!(!l.tiles.is_empty());
                    // Rows cover the tiles contiguously.
                    let covered: usize = l.rows.iter().map(|r| r.len()).sum();
                    assert_eq!(covered, l.tiles.len());
                    if l.tiles.len() < n {
                        // Only truncated at the minimum size.
                        assert_eq!(l.thumb_h, p.min_thumb_h, "n={n}");
                    }
                }
            }
        }
        // 14 windows on a laptop screen: multiple rows, smaller thumbnails, all fit.
        let l = compute(&[1.6; 14], SCREEN, &LayoutParams::THUMBNAILS);
        assert!(l.rows.len() >= 2);
        assert_eq!(l.tiles.len(), 14);
        assert!(l.thumb_h < LayoutParams::THUMBNAILS.max_thumb_h);
    }

    #[test]
    fn rows_are_centered() {
        let l = compute(&[1.6; 7], RectF::new(0.0, 0.0, 1100.0, 900.0), &LayoutParams::THUMBNAILS);
        assert!(l.rows.len() >= 2);
        for r in &l.rows {
            let left = l.tiles[r.start].frame.x;
            let right = l.panel.w - l.tiles[r.end - 1].frame.right();
            assert!((left - right).abs() <= 1.0, "row {r:?} not centered: {left} vs {right}");
        }
    }

    #[test]
    fn extreme_aspects_are_clamped() {
        let p = LayoutParams::THUMBNAILS;
        let l = compute(&[10.0, 0.05], SCREEN, &p);
        let h = l.thumb_h;
        assert!(l.tiles[0].thumb.w <= p.max_aspect * h + 1.0);
        assert!(l.tiles[1].frame.w >= p.min_tile_w);
    }

    #[test]
    fn hit_testing() {
        let l = compute(&[1.6, 1.6], SCREEN, &LayoutParams::THUMBNAILS);
        let t = l.tiles[1].frame;
        assert_eq!(l.hit(t.x + 5.0, t.y + 5.0), Some(1));
        assert_eq!(l.hit_global(l.panel.x + t.x + 5.0, l.panel.y + t.y + 5.0), Some(1));
        assert_eq!(l.hit(1.0, 1.0), None);
    }

    #[test]
    fn arrow_navigation() {
        // 3 rows: force wrapping with a narrow screen.
        let screen = RectF::new(0.0, 0.0, 700.0, 1400.0);
        let l = compute(&[1.6; 7], screen, &LayoutParams::THUMBNAILS);
        assert!(l.rows.len() >= 3, "{:?}", l.rows);
        let n = l.tiles.len();
        assert_eq!(l.neighbor(0, Dir::Right), 1);
        assert_eq!(l.neighbor(n - 1, Dir::Right), 0);
        assert_eq!(l.neighbor(0, Dir::Left), n - 1);
        // Down from first row lands in the second row, closest column.
        let d = l.neighbor(0, Dir::Down);
        assert!(l.rows[1].contains(&d));
        assert_eq!(l.neighbor(d, Dir::Up), 0);
        // Up from the first row wraps to the last row.
        let u = l.neighbor(0, Dir::Up);
        assert!(l.rows.last().unwrap().contains(&u));
        // Single row: up/down are no-ops.
        let one = compute(&[1.6; 3], SCREEN, &LayoutParams::THUMBNAILS);
        assert_eq!(one.neighbor(1, Dir::Down), 1);
        assert_eq!(compute(&[], SCREEN, &LayoutParams::THUMBNAILS).neighbor(3, Dir::Right), 0);
    }
}
