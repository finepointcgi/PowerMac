//! Panel layout: a header (app icon + name + window count) and one tile per
//! window (thumbnail box sized to the window's aspect ratio, title below,
//! close / minimize buttons over the thumbnail's top-right corner).
//!
//! A bottom Dock lays tiles out in rows (wrapping to more rows when the
//! screen is too narrow); a side Dock stacks them in columns. If even that
//! doesn't fit, thumbnails shrink (down to half size).
//!
//! All rects are panel-local, top-left origin.

use crate::geom::{Orientation, RectF};
use crate::PreviewSize;

/// Spacing constants.
pub const PAD: f64 = 12.0;
pub const GAP: f64 = 8.0;
pub const HEADER_H: f64 = 30.0;
pub const TILE_PAD: f64 = 8.0;
pub const LABEL_H: f64 = 18.0;
pub const LABEL_GAP: f64 = 6.0;
pub const BUTTON: f64 = 18.0;
pub const BUTTON_INSET: f64 = 7.0;
pub const BUTTON_GAP: f64 = 6.0;
/// Narrowest / widest thumbnail box relative to its height.
pub const MIN_ASPECT: f64 = 0.72;
pub const MAX_ASPECT: f64 = 1.9;
/// The panel is never narrower than this (room for the header).
pub const MIN_PANEL_W: f64 = 220.0;

/// One window tile.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tile {
    /// Whole tile (hover highlight).
    pub frame: RectF,
    /// Thumbnail box (the image is aspect-fit inside).
    pub thumb: RectF,
    /// Where the thumbnail is expected inside `thumb` (the window's aspect
    /// ratio fit into the box); the buttons sit on its corner.
    pub content: RectF,
    /// Title line.
    pub label: RectF,
    pub close: RectF,
    pub minimize: RectF,
}

/// What a panel-local point hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Close(usize),
    Minimize(usize),
    Tile(usize),
    /// The panel background / header.
    Panel,
    None,
}

/// The computed layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub w: f64,
    pub h: f64,
    pub header: RectF,
    pub tiles: Vec<Tile>,
    /// Thumbnail height used (after shrinking to fit).
    pub thumb_h: f64,
}

fn tile_w(aspect: f64, th: f64) -> f64 {
    (th * clamp_aspect(aspect)).round() + 2.0 * TILE_PAD
}

fn tile_h(th: f64) -> f64 {
    TILE_PAD + th + LABEL_GAP + LABEL_H + TILE_PAD
}

/// Groups of tile indices (rows or columns) filling `limit` along the main
/// axis; `sizes` are the tiles' main-axis lengths.
fn flow(sizes: &[f64], limit: f64) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, &s) in sizes.iter().enumerate() {
        match groups.last_mut() {
            Some(g) if used + GAP + s <= limit => {
                g.push(i);
                used += GAP + s;
            }
            _ => {
                groups.push(vec![i]);
                used = s;
            }
        }
    }
    groups
}

impl Layout {
    /// Lay out tiles for windows with the given aspect ratios (w/h), for a
    /// Dock with `orientation`, within at most `max_w`×`max_h` points.
    pub fn compute(aspects: &[f64], size: PreviewSize, orientation: Orientation, max_w: f64, max_h: f64) -> Layout {
        let base = size.thumb_height();
        // 100%, 90%, ... 50%.
        let mut step = 10;
        loop {
            let l = Self::compute_at(aspects, (base * step as f64 / 10.0).round(), orientation, max_w, max_h);
            if (l.w <= max_w && l.h <= max_h) || step <= 5 {
                return l;
            }
            step -= 1;
        }
    }

    fn compute_at(aspects: &[f64], th: f64, orientation: Orientation, max_w: f64, max_h: f64) -> Layout {
        // Wrap after a sensible extent rather than the full screen so the
        // panel stays compact: rows ≤ 1200 pt, columns ≤ 4 tiles.
        let n = aspects.len();
        let widths: Vec<f64> = aspects.iter().map(|&a| tile_w(a, th)).collect();
        let th_tile = tile_h(th);
        let mut tiles = vec![Tile::default(); n];
        let (content_w, content_h);
        if orientation.horizontal() {
            let rows = flow(&widths, 1200.0f64.min(max_w - 2.0 * PAD));
            let row_w = |r: &Vec<usize>| r.iter().map(|&i| widths[i]).sum::<f64>() + GAP * (r.len().saturating_sub(1)) as f64;
            content_w = rows.iter().map(row_w).fold(0.0, f64::max);
            content_h = rows.len() as f64 * th_tile + GAP * rows.len().saturating_sub(1) as f64;
            let panel_w = (content_w + 2.0 * PAD).max(MIN_PANEL_W);
            for (ri, r) in rows.iter().enumerate() {
                let mut x = ((panel_w - row_w(r)) / 2.0).round();
                let y = PAD + HEADER_H + ri as f64 * (th_tile + GAP);
                for &i in r {
                    tiles[i] = make_tile(RectF::new(x, y, widths[i], th_tile), th, aspects[i]);
                    x += widths[i] + GAP;
                }
            }
        } else {
            let heights = vec![th_tile; n];
            let per_col = (4.0 * th_tile + 3.0 * GAP).min(max_h - 2.0 * PAD - HEADER_H);
            let cols = flow(&heights, per_col);
            let col_w: Vec<f64> = cols.iter().map(|c| c.iter().map(|&i| widths[i]).fold(0.0, f64::max)).collect();
            content_w = col_w.iter().sum::<f64>() + GAP * cols.len().saturating_sub(1) as f64;
            content_h = cols.iter().map(|c| c.len() as f64 * th_tile + GAP * c.len().saturating_sub(1) as f64).fold(0.0, f64::max);
            let panel_w = (content_w + 2.0 * PAD).max(MIN_PANEL_W);
            let mut x = ((panel_w - content_w) / 2.0).round();
            for (ci, c) in cols.iter().enumerate() {
                for (k, &i) in c.iter().enumerate() {
                    let y = PAD + HEADER_H + k as f64 * (th_tile + GAP);
                    tiles[i] = make_tile(RectF::new(x, y, col_w[ci], th_tile), th, aspects[i]);
                }
                x += col_w[ci] + GAP;
            }
        }
        let w = (content_w + 2.0 * PAD).max(MIN_PANEL_W);
        let h = PAD + HEADER_H + content_h + PAD;
        Layout { w, h, header: RectF::new(PAD + 4.0, PAD, w - 2.0 * PAD - 8.0, HEADER_H - 6.0), tiles, thumb_h: th }
    }

    /// Hit test a panel-local point.
    pub fn hit(&self, x: f64, y: f64) -> Hit {
        if !RectF::new(0.0, 0.0, self.w, self.h).contains(x, y) {
            return Hit::None;
        }
        for (i, t) in self.tiles.iter().enumerate() {
            if t.frame.contains(x, y) {
                // Buttons get a little extra slop around their circle.
                if t.close.inset(-2.0).contains(x, y) {
                    return Hit::Close(i);
                }
                if t.minimize.inset(-2.0).contains(x, y) {
                    return Hit::Minimize(i);
                }
                return Hit::Tile(i);
            }
        }
        Hit::Panel
    }

    /// Index of the tile at a panel-local point.
    pub fn tile_at(&self, x: f64, y: f64) -> Option<usize> {
        match self.hit(x, y) {
            Hit::Close(i) | Hit::Minimize(i) | Hit::Tile(i) => Some(i),
            _ => None,
        }
    }
}

fn clamp_aspect(a: f64) -> f64 {
    if a.is_finite() && a > 0.0 {
        a.clamp(MIN_ASPECT, MAX_ASPECT)
    } else {
        1.6
    }
}

fn make_tile(frame: RectF, th: f64, aspect: f64) -> Tile {
    let thumb = RectF::new(frame.x + TILE_PAD, frame.y + TILE_PAD, frame.w - 2.0 * TILE_PAD, th);
    let content = fit(thumb, clamp_aspect(aspect), 1.0);
    let label = RectF::new(frame.x + TILE_PAD, thumb.bottom() + LABEL_GAP, frame.w - 2.0 * TILE_PAD, LABEL_H);
    // Top-right corner, clear of the window's own traffic lights.
    let close = RectF::new(content.right() - BUTTON_INSET - BUTTON, content.y + BUTTON_INSET, BUTTON, BUTTON);
    let minimize = close.offset(-(BUTTON + BUTTON_GAP), 0.0);
    Tile { frame, thumb, content, label, close, minimize }
}

/// Aspect-fit a `w`×`h` box into `r`, centered and pixel-rounded.
pub fn fit(r: RectF, w: f64, h: f64) -> RectF {
    if w <= 0.0 || h <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
        return r;
    }
    let k = (r.w / w).min(r.h / h);
    let (fw, fh) = ((w * k).round(), (h * k).round());
    RectF::new((r.x + (r.w - fw) / 2.0).round(), (r.y + (r.h - fh) / 2.0).round(), fw, fh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_overlap(l: &Layout) {
        for (i, a) in l.tiles.iter().enumerate() {
            assert!(a.frame.x >= PAD - 1e-9 && a.frame.right() <= l.w - PAD + 1e-9, "tile {i} x {:?} in {}", a.frame, l.w);
            assert!(a.frame.y >= PAD + HEADER_H - 1e-9 && a.frame.bottom() <= l.h - PAD + 1e-9);
            for b in &l.tiles[i + 1..] {
                assert_eq!(a.frame.intersection_area(&b.frame), 0.0);
            }
        }
    }

    #[test]
    fn single_row_for_bottom_dock() {
        let l = Layout::compute(&[1.6, 1.0, 1.3], PreviewSize::Medium, Orientation::Bottom, 1500.0, 900.0);
        assert_eq!(l.thumb_h, 136.0);
        assert_eq!(l.tiles.len(), 3);
        let y = l.tiles[0].frame.y;
        assert!(l.tiles.iter().all(|t| t.frame.y == y));
        assert!(l.tiles[0].frame.x < l.tiles[1].frame.x && l.tiles[1].frame.x < l.tiles[2].frame.x);
        // Width follows the aspect ratio.
        assert_eq!(l.tiles[0].thumb.w, (136.0f64 * 1.6).round());
        assert_eq!(l.tiles[1].thumb.w, 136.0);
        assert_eq!(l.h, PAD + HEADER_H + tile_h(136.0) + PAD);
        no_overlap(&l);
    }

    #[test]
    fn aspect_is_clamped() {
        let l = Layout::compute(&[10.0, 0.1, f64::NAN], PreviewSize::Small, Orientation::Bottom, 1500.0, 900.0);
        assert_eq!(l.tiles[0].thumb.w, (96.0 * MAX_ASPECT).round());
        assert_eq!(l.tiles[1].thumb.w, (96.0 * MIN_ASPECT).round());
        assert_eq!(l.tiles[2].thumb.w, (96.0 * 1.6f64).round());
    }

    #[test]
    fn many_windows_wrap_into_rows() {
        let aspects = vec![1.6; 9];
        let l = Layout::compute(&aspects, PreviewSize::Medium, Orientation::Bottom, 1500.0, 900.0);
        let rows: std::collections::BTreeSet<i64> = l.tiles.iter().map(|t| t.frame.y as i64).collect();
        assert!(rows.len() >= 2);
        assert!(l.w <= 1200.0 + 2.0 * PAD + 1e-9);
        no_overlap(&l);
    }

    #[test]
    fn side_dock_stacks_in_columns() {
        let l = Layout::compute(&[1.6, 1.2, 1.5], PreviewSize::Medium, Orientation::Left, 1500.0, 900.0);
        let x = l.tiles[0].frame.x;
        assert!(l.tiles.iter().all(|t| t.frame.x == x));
        assert!(l.tiles[0].frame.y < l.tiles[1].frame.y);
        // Column width = widest tile; narrower windows are centered in it and
        // their buttons follow the content, not the box.
        assert!(l.tiles.iter().all(|t| t.frame.w == l.tiles[0].frame.w));
        let t = l.tiles[1];
        assert!(t.content.w < t.thumb.w);
        assert_eq!(t.close.right(), t.content.right() - BUTTON_INSET);
        no_overlap(&l);
        let six = Layout::compute(&[1.5; 6], PreviewSize::Small, Orientation::Right, 1500.0, 1200.0);
        let cols: std::collections::BTreeSet<i64> = six.tiles.iter().map(|t| t.frame.x as i64).collect();
        assert_eq!(cols.len(), 2);
        no_overlap(&six);
    }

    #[test]
    fn shrinks_to_fit() {
        let l = Layout::compute(&[1.6; 12], PreviewSize::Large, Orientation::Bottom, 1000.0, 600.0);
        assert!(l.thumb_h < 188.0);
        assert!(l.w <= 1000.0 && l.h <= 600.0, "{}x{}", l.w, l.h);
        no_overlap(&l);
        // Never below half size even if it can't fit.
        let tiny = Layout::compute(&[1.6; 40], PreviewSize::Large, Orientation::Bottom, 300.0, 200.0);
        assert!(tiny.thumb_h >= 188.0 * 0.5 - 1.0);
    }

    #[test]
    fn single_window_has_min_width_and_centered_tile() {
        let l = Layout::compute(&[0.72], PreviewSize::Small, Orientation::Bottom, 1500.0, 900.0);
        assert_eq!(l.w, MIN_PANEL_W);
        let t = l.tiles[0].frame;
        assert!(((t.x) - (l.w - t.right())).abs() <= 1.0);
    }

    #[test]
    fn hit_testing() {
        let l = Layout::compute(&[1.6, 1.6], PreviewSize::Medium, Orientation::Bottom, 1500.0, 900.0);
        let t = l.tiles[1];
        assert_eq!(l.hit(t.thumb.center().0, t.thumb.center().1), Hit::Tile(1));
        assert_eq!(l.hit(t.close.center().0, t.close.center().1), Hit::Close(1));
        assert_eq!(l.hit(t.minimize.center().0, t.minimize.center().1), Hit::Minimize(1));
        assert_eq!(l.hit(t.label.center().0, t.label.center().1), Hit::Tile(1));
        assert_eq!(l.tile_at(t.close.x, t.close.y), Some(1));
        assert_eq!(l.hit(2.0, 2.0), Hit::Panel);
        assert_eq!(l.hit(-1.0, 5.0), Hit::None);
        assert_eq!(l.hit(l.w + 1.0, 5.0), Hit::None);
        // Buttons sit inside the thumbnail's top-right corner, close outermost.
        assert!(t.minimize.right() < t.close.x);
        assert!(t.thumb.contains(t.close.right() - 0.1, t.close.bottom() - 0.1));
        assert!(t.thumb.contains(t.minimize.x, t.minimize.y));
    }

    #[test]
    fn fit_centers() {
        let r = fit(RectF::new(0.0, 0.0, 200.0, 100.0), 100.0, 100.0);
        assert_eq!(r, RectF::new(50.0, 0.0, 100.0, 100.0));
        assert_eq!(fit(RectF::new(0.0, 0.0, 10.0, 10.0), 0.0, 5.0), RectF::new(0.0, 0.0, 10.0, 10.0));
    }
}
