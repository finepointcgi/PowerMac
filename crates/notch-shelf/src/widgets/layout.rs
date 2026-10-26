//! Home view layout: each page is a horizontal row of rounded cards of
//! fixed size classes (small / medium / large, mixed freely) that is
//! centered in the panel, or scrolls like the shelf strip when it's wider.
//! Pages sit side by side and slide horizontally when switching.

use super::instance::WidgetInstance;
use crate::geometry::Rect;

/// Card height and spacing, in points.
pub const CARD_H: f64 = 112.0;
pub const CARD_GAP: f64 = 8.0;
pub const CARD_RADIUS: f64 = 18.0;
/// Card widths by size class (medium = 2 small + gap, large = 3 small + 2 gaps).
pub const SMALL_W: f64 = 136.0;
pub const MEDIUM_W: f64 = 2.0 * SMALL_W + CARD_GAP;
pub const LARGE_W: f64 = 3.0 * SMALL_W + 2.0 * CARD_GAP;
/// Inner padding of a card.
pub const PAD: f64 = 12.0;
/// Quick-controls button size and spacing.
pub const TOGGLE: f64 = 34.0;
pub const TOGGLE_GAP: f64 = 8.0;
/// Widest the Home view grows the panel on its own.
pub const HOME_MAX_W: f64 = 940.0;
/// Space between pages while they slide.
pub const PAGE_GAP: f64 = 28.0;

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

/// How many controls fit a quick-controls card `w` wide (two rows).
pub fn toggle_capacity(w: f64) -> usize {
    let cols = ((w - 2.0 * PAD + TOGGLE_GAP) / (TOGGLE + TOGGLE_GAP)).floor().max(1.0) as usize;
    2 * cols
}

/// Card spec of a fixed-width card.
pub fn fixed(w: f64) -> CardSpec {
    CardSpec { min_w: w, max_w: w, weight: 0.0 }
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

/// Card specs of a page's instances (fixed widths by size).
pub fn page_specs(list: &[&WidgetInstance]) -> Vec<CardSpec> {
    list.iter().map(|w| fixed(w.effective_size().width())).collect()
}

/// Panel width for several pages: the widest page decides (so switching
/// pages slides instead of resizing).
pub fn pages_width(pages: &[Vec<CardSpec>], side_pad: f64, configured: f64) -> f64 {
    pages.iter().map(|p| home_width(p, side_pad, configured)).fold(configured.max(0.0), f64::max)
}

/// Horizontal offset and opacity of page `k` while the page position
/// (animated, fractional) is `pos`, for rows `row_w` wide.
pub fn page_offset(k: usize, pos: f64, row_w: f64) -> (f64, f64) {
    let d = k as f64 - pos;
    let alpha = (1.0 - d.abs() * 0.85).clamp(0.0, 1.0);
    (d * (row_w + PAGE_GAP), alpha)
}

/// The page a horizontal swipe lands on: `dir` > 0 goes right (next).
pub fn swipe_target(cur: usize, n: usize, dir: f64) -> usize {
    if dir > 0.0 {
        (cur + 1).min(n.saturating_sub(1))
    } else if dir < 0.0 {
        cur.saturating_sub(1)
    } else {
        cur
    }
}

/// Page tabs in the header: tab frames right-aligned in `area` with the
/// given text widths, or `None` when they don't fit (dots are used then).
pub fn page_tabs(area: Rect, text_widths: &[f64]) -> Option<Vec<Rect>> {
    const PADX: f64 = 10.0;
    const H: f64 = 22.0;
    let total: f64 = text_widths.iter().map(|w| w + 2.0 * PADX).sum::<f64>() + 4.0;
    if text_widths.len() < 2 || total > area.w {
        return None;
    }
    let mut x = area.right() - total + 2.0;
    let y = area.center_y() - H / 2.0;
    Some(
        text_widths
            .iter()
            .map(|w| {
                let r = Rect::new(x, y, w + 2.0 * PADX, H);
                x += r.w;
                r
            })
            .collect(),
    )
}

/// Page dots right-aligned in `area` (8 pt hit targets 14 pt apart).
pub fn page_dots(area: Rect, n: usize) -> Vec<Rect> {
    let pitch = 14.0;
    let x0 = area.right() - pitch * n as f64;
    (0..n).map(|i| Rect::new(x0 + i as f64 * pitch, area.center_y() - pitch / 2.0, pitch, pitch)).collect()
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
        let ws = |v: &[f64]| -> Vec<CardSpec> { v.iter().map(|w| fixed(*w)).collect() };
        let specs = ws(&[MEDIUM_W, SMALL_W, MEDIUM_W]);
        let w = home_width(&specs, 18.0, 620.0);
        assert!((w - (natural_width(&specs) + 36.0)).abs() < 1e-9);
        assert!(w > 620.0 && w <= HOME_MAX_W);
        assert_eq!(home_width(&specs[..1], 18.0, 620.0), 620.0);
        let all = ws(&[LARGE_W; 4]);
        assert_eq!(home_width(&all, 18.0, 620.0), HOME_MAX_W);
        assert_eq!(home_width(&all, 18.0, 1200.0), 1200.0);
        assert_eq!(home_width(&[], 18.0, 620.0), 620.0);
    }

    #[test]
    fn mixed_sizes_in_a_row() {
        let list = [
            WidgetInstance::new(super::super::WidgetKind::Battery).sized(super::super::instance::WidgetSize::Small),
            WidgetInstance::new(super::super::WidgetKind::Script).sized(super::super::instance::WidgetSize::Large),
            WidgetInstance::new(super::super::WidgetKind::Note).sized(super::super::instance::WidgetSize::Medium),
            // Not allowed: snaps to medium.
            WidgetInstance::new(super::super::WidgetKind::Battery).sized(super::super::instance::WidgetSize::Large),
        ];
        let refs: Vec<&WidgetInstance> = list.iter().collect();
        let specs = page_specs(&refs);
        assert_eq!(specs.iter().map(|s| s.min_w).collect::<Vec<_>>(), vec![SMALL_W, LARGE_W, MEDIUM_W, MEDIUM_W]);
        let area = Rect::new(0.0, 40.0, 1200.0, CARD_H);
        let l = layout_cards(area, &specs, 0.0);
        // Fixed widths, centered, gaps between.
        assert_eq!(l.cards.iter().map(|c| c.w).collect::<Vec<_>>(), vec![SMALL_W, LARGE_W, MEDIUM_W, MEDIUM_W]);
        assert!((l.cards[1].x - l.cards[0].right() - CARD_GAP).abs() < 1e-9);
        assert!(((l.cards[0].x - area.x) - (area.right() - l.cards[3].right())).abs() < 1e-9);
        // The grid lines up: a large card is as wide as three small ones.
        assert_eq!(LARGE_W, 3.0 * SMALL_W + 2.0 * CARD_GAP);
        // Too wide: scrolls.
        let narrow = Rect::new(0.0, 0.0, 600.0, CARD_H);
        let l = layout_cards(narrow, &specs, 1e9);
        assert!(l.max_scroll > 0.0 && l.scroll == l.max_scroll);
        assert!((l.cards[3].right() - 600.0).abs() < 1e-9);
    }

    #[test]
    fn pages_slide_and_share_a_width() {
        let p1 = vec![fixed(SMALL_W)];
        let p2 = vec![fixed(LARGE_W), fixed(MEDIUM_W)];
        let w = pages_width(&[p1.clone(), p2.clone()], 18.0, 620.0);
        assert_eq!(w, home_width(&p2, 18.0, 620.0));
        assert_eq!(pages_width(&[p1], 18.0, 620.0), 620.0);
        assert_eq!(pages_width(&[], 18.0, 620.0), 620.0);
        assert_eq!(page_offset(1, 1.0, 800.0), (0.0, 1.0));
        let (x, a) = page_offset(2, 1.0, 800.0);
        assert_eq!(x, 800.0 + PAGE_GAP);
        assert!(a < 0.2);
        let (x, a) = page_offset(0, 0.5, 800.0);
        assert!(x < 0.0 && a > 0.5);
        assert_eq!(swipe_target(0, 3, 1.0), 1);
        assert_eq!(swipe_target(2, 3, 1.0), 2);
        assert_eq!(swipe_target(0, 3, -1.0), 0);
        assert_eq!(swipe_target(1, 3, 0.0), 1);
    }

    #[test]
    fn tabs_or_dots() {
        let area = Rect::new(500.0, 0.0, 300.0, 32.0);
        let tabs = page_tabs(area, &[30.0, 40.0, 25.0]).unwrap();
        assert_eq!(tabs.len(), 3);
        assert!((tabs[2].right() - (area.right() - 2.0)).abs() < 1e-9);
        assert!(tabs.windows(2).all(|w| (w[0].right() - w[1].x).abs() < 1e-9));
        assert!(tabs.iter().all(|t| (t.center_y() - area.center_y()).abs() < 1e-9));
        assert!(page_tabs(area, &[200.0, 200.0]).is_none());
        assert!(page_tabs(area, &[20.0]).is_none(), "one page: no tabs");
        let dots = page_dots(area, 4);
        assert_eq!(dots.len(), 4);
        assert!((dots[3].right() - area.right()).abs() < 1e-9);
    }

    #[test]
    fn toggle_grid() {
        assert_eq!(toggle_cols(0), 1);
        assert_eq!(toggle_cols(5), 3);
        assert_eq!(toggle_capacity(SMALL_W), 4);
        assert_eq!(toggle_capacity(MEDIUM_W), 12);
        let card = Rect::new(0.0, 0.0, SMALL_W, CARD_H);
        let cells = toggle_cells(card, 4);
        assert_eq!(cells.len(), 4);
        assert!(cells.iter().all(|c| c.x >= 0.0 && c.right() <= SMALL_W));
        assert_eq!(cells[2].y, cells[0].y + TOGGLE + TOGGLE_GAP);
        assert!(cells.iter().all(|c| c.bottom() <= card.bottom() - PAD + 2.0 + 1e-9 && c.y > 20.0));
    }
}
