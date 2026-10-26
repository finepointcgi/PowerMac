//! Home view layout: a horizontal row of rounded cards that fits the panel
//! (widths flex between a minimum and a maximum; if even the minimums
//! don't fit, the row scrolls like the shelf strip).

use super::WidgetKind;
use crate::geometry::Rect;

/// Card height and spacing, in points.
pub const CARD_H: f64 = 112.0;
pub const CARD_GAP: f64 = 8.0;
pub const CARD_RADIUS: f64 = 18.0;
/// Inner padding of a card.
pub const PAD: f64 = 12.0;
/// Quick-controls button size and spacing.
pub const TOGGLE: f64 = 34.0;
pub const TOGGLE_GAP: f64 = 8.0;
/// Widest the Home view grows the panel on its own.
pub const HOME_MAX_W: f64 = 940.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardSpec {
    pub min_w: f64,
    pub max_w: f64,
    /// Share of extra room.
    pub weight: f64,
}

/// Columns of the quick-controls grid (two rows).
pub fn toggle_cols(n: usize) -> usize {
    n.div_ceil(2).max(1)
}

pub fn spec(kind: WidgetKind, n_controls: usize) -> CardSpec {
    let s = |min_w: f64, max_w: f64, weight: f64| CardSpec { min_w, max_w, weight };
    match kind {
        WidgetKind::NowPlaying => s(264.0, 340.0, 2.0),
        WidgetKind::Calendar => s(214.0, 280.0, 1.5),
        WidgetKind::Weather => s(140.0, 170.0, 0.5),
        WidgetKind::Battery => s(116.0, 136.0, 0.3),
        WidgetKind::SystemStats => s(196.0, 236.0, 1.0),
        WidgetKind::Timer => s(176.0, 210.0, 0.6),
        WidgetKind::QuickToggles => {
            let c = toggle_cols(n_controls) as f64;
            let w = 2.0 * PAD + c * TOGGLE + (c - 1.0) * TOGGLE_GAP;
            s(w.max(112.0), w.max(112.0), 0.0)
        }
        WidgetKind::Note => s(184.0, 280.0, 1.2),
        WidgetKind::Mirror => s(112.0, 150.0, 0.4),
        WidgetKind::Unknown => s(0.0, 0.0, 0.0),
    }
}

/// Width of the cards at their minimum plus gaps.
pub fn natural_width(specs: &[CardSpec]) -> f64 {
    if specs.is_empty() {
        return 0.0;
    }
    specs.iter().map(|s| s.min_w).sum::<f64>() + CARD_GAP * (specs.len() - 1) as f64
}

/// Panel width for the Home view: wide enough for the cards at their
/// minimum (up to [`HOME_MAX_W`] or the configured width, whichever is
/// larger), never narrower than the configured width.
pub fn home_width(specs: &[CardSpec], side_pad: f64, configured: f64) -> f64 {
    let want = natural_width(specs) + 2.0 * side_pad;
    want.min(HOME_MAX_W.max(configured)).max(configured)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardsLayout {
    /// Card frames, already scrolled.
    pub cards: Vec<Rect>,
    pub content_w: f64,
    pub max_scroll: f64,
    pub scroll: f64,
}

/// Distribute `area.w` among the cards: everybody gets `min_w`, the rest is
/// shared by weight (capped at `max_w`, leftovers re-shared); a row that
/// still doesn't fill the area is centered. Too narrow: minimum widths,
/// scrolled by `scroll`.
pub fn layout_cards(area: Rect, specs: &[CardSpec], scroll: f64) -> CardsLayout {
    let n = specs.len();
    if n == 0 {
        return CardsLayout { cards: vec![], content_w: 0.0, max_scroll: 0.0, scroll: 0.0 };
    }
    let mut widths: Vec<f64> = specs.iter().map(|s| s.min_w).collect();
    let gaps = CARD_GAP * (n - 1) as f64;
    let mut extra = area.w - gaps - widths.iter().sum::<f64>();
    // Water-fill the extra room by weight.
    for _ in 0..n {
        if extra <= 0.01 {
            break;
        }
        let open: Vec<usize> = (0..n).filter(|&i| widths[i] < specs[i].max_w - 1e-9 && specs[i].weight > 0.0).collect();
        let wsum: f64 = open.iter().map(|&i| specs[i].weight).sum();
        if open.is_empty() || wsum <= 0.0 {
            break;
        }
        let mut used = 0.0;
        for &i in &open {
            let add = (extra * specs[i].weight / wsum).min(specs[i].max_w - widths[i]);
            widths[i] += add;
            used += add;
        }
        extra -= used;
    }
    let content_w = widths.iter().sum::<f64>() + gaps;
    let max_scroll = (content_w - area.w).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    let mut x = if content_w <= area.w { area.center_x() - content_w / 2.0 } else { area.x - scroll };
    let cards = widths
        .iter()
        .map(|w| {
            let r = Rect::new(x, area.y, *w, area.h);
            x += w + CARD_GAP;
            r
        })
        .collect();
    CardsLayout { cards, content_w, max_scroll, scroll }
}

/// Grid cells of the quick-controls card (card-local), row-major in two
/// rows below a caption line.
pub fn toggle_cells(card: Rect, n: usize) -> Vec<Rect> {
    let cols = toggle_cols(n);
    let grid_w = cols as f64 * TOGGLE + (cols as f64 - 1.0) * TOGGLE_GAP;
    let x0 = card.center_x() - grid_w / 2.0;
    let y0 = card.bottom() - PAD - 2.0 * TOGGLE - TOGGLE_GAP + 2.0;
    (0..n)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            Rect::new(x0 + c as f64 * (TOGGLE + TOGGLE_GAP), y0 + r as f64 * (TOGGLE + TOGGLE_GAP), TOGGLE, TOGGLE)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(min_w: f64, max_w: f64, weight: f64) -> CardSpec {
        CardSpec { min_w, max_w, weight }
    }

    #[test]
    fn cards_fill_by_weight_and_respect_maximums() {
        let area = Rect::new(0.0, 50.0, 600.0, CARD_H);
        let specs = [sp(100.0, 400.0, 1.0), sp(100.0, 150.0, 3.0), sp(100.0, 100.0, 0.0)];
        let l = layout_cards(area, &specs, 0.0);
        let w: Vec<f64> = l.cards.iter().map(|r| r.w).collect();
        // 600 - 16 gaps - 300 = 284 extra: #2 caps at +50, #1 gets the rest.
        assert!((w[1] - 150.0).abs() < 1e-6, "{w:?}");
        assert!((w[0] - 334.0).abs() < 1e-6, "{w:?}");
        assert_eq!(w[2], 100.0);
        assert!((l.content_w - 600.0).abs() < 1e-6);
        assert_eq!(l.cards[0].x, 0.0);
        assert!((l.cards[2].right() - 600.0).abs() < 1e-6);
        assert!(l.cards.iter().all(|c| c.y == 50.0 && c.h == CARD_H));
        assert_eq!(l.max_scroll, 0.0);
    }

    #[test]
    fn small_rows_are_centered_and_wide_rows_scroll() {
        let area = Rect::new(100.0, 0.0, 600.0, CARD_H);
        let l = layout_cards(area, &[sp(120.0, 140.0, 1.0)], 0.0);
        assert_eq!(l.cards[0].w, 140.0);
        assert!((l.cards[0].center_x() - area.center_x()).abs() < 1e-9);
        let many = vec![sp(200.0, 300.0, 1.0); 4];
        let l = layout_cards(area, &many, 1e9);
        assert!(l.cards.iter().all(|c| c.w == 200.0));
        assert!((l.content_w - (800.0 + 3.0 * CARD_GAP)).abs() < 1e-9);
        assert_eq!(l.scroll, l.max_scroll);
        assert!((l.cards[3].right() - area.right()).abs() < 1e-9);
        let l = layout_cards(area, &many, -5.0);
        assert_eq!(l.cards[0].x, area.x);
        assert!(layout_cards(area, &[], 0.0).cards.is_empty());
    }

    #[test]
    fn home_width_grows_within_limits() {
        let specs: Vec<CardSpec> = [WidgetKind::NowPlaying, WidgetKind::Battery, WidgetKind::Timer, WidgetKind::QuickToggles].iter().map(|k| spec(*k, 5)).collect();
        let w = home_width(&specs, 18.0, 620.0);
        assert!((w - (natural_width(&specs) + 36.0)).abs() < 1e-9);
        assert!(w > 620.0 && w <= HOME_MAX_W);
        // Fits the configured width: stays at it.
        assert_eq!(home_width(&specs[..1], 18.0, 620.0), 620.0);
        // Everything enabled: capped.
        let all: Vec<CardSpec> = WidgetKind::ALL.iter().map(|k| spec(*k, 8)).collect();
        assert_eq!(home_width(&all, 18.0, 620.0), HOME_MAX_W);
        // A configured width above the cap wins.
        assert_eq!(home_width(&all, 18.0, 1200.0), 1200.0);
        assert_eq!(home_width(&[], 18.0, 620.0), 620.0);
    }

    #[test]
    fn toggle_grid() {
        assert_eq!(toggle_cols(0), 1);
        assert_eq!(toggle_cols(5), 3);
        let s = spec(WidgetKind::QuickToggles, 6);
        assert_eq!(s.min_w, 2.0 * PAD + 3.0 * TOGGLE + 2.0 * TOGGLE_GAP);
        assert_eq!(spec(WidgetKind::QuickToggles, 1).min_w, 112.0);
        let card = Rect::new(0.0, 0.0, s.min_w, CARD_H);
        let cells = toggle_cells(card, 5);
        assert_eq!(cells.len(), 5);
        assert!((cells[0].x - PAD).abs() < 1e-9);
        assert_eq!(cells[3].y, cells[0].y + TOGGLE + TOGGLE_GAP);
        assert!(cells.iter().all(|c| c.bottom() <= card.bottom() - PAD + 2.0 + 1e-9 && c.y > 20.0));
    }
}
