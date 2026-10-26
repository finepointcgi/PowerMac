//! Rectangle-style "drag to a screen edge" snapping: while a window is being
//! dragged *without* zones showing, moving the cursor to a monitor edge
//! previews a half / quarter / maximized / third rect, and releasing the
//! mouse snaps the window there.
//!
//! Detection uses the monitor *frame* (the cursor can reach the menu bar /
//! taskbar row); the target rect is computed inside the *work area*. Edges
//! shared with another monitor are ignored so crossing between displays
//! doesn't trigger a snap.

use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect};

/// What dragging to the top edge does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TopEdgeAction {
    None,
    #[default]
    Maximize,
    TopHalf,
}

/// What dragging to the bottom edge does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BottomEdgeAction {
    None,
    #[default]
    BottomHalf,
    /// Left / center / right third, chosen by the cursor's x position.
    Thirds,
}

/// The snap a drag-to-edge resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeAction {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
    Maximize,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    FirstThird,
    CenterThird,
    LastThird,
}

impl EdgeAction {
    /// Target as fractions (x0, y0, x1, y1) of the work area, in sixths so
    /// halves and thirds are both exact.
    fn sixths(self) -> (i32, i32, i32, i32) {
        match self {
            EdgeAction::LeftHalf => (0, 0, 3, 6),
            EdgeAction::RightHalf => (3, 0, 6, 6),
            EdgeAction::TopHalf => (0, 0, 6, 3),
            EdgeAction::BottomHalf => (0, 3, 6, 6),
            EdgeAction::Maximize => (0, 0, 6, 6),
            EdgeAction::TopLeft => (0, 0, 3, 3),
            EdgeAction::TopRight => (3, 0, 6, 3),
            EdgeAction::BottomLeft => (0, 3, 3, 6),
            EdgeAction::BottomRight => (3, 3, 6, 6),
            EdgeAction::FirstThird => (0, 0, 2, 6),
            EdgeAction::CenterThird => (2, 0, 4, 6),
            EdgeAction::LastThird => (4, 0, 6, 6),
        }
    }

    /// The rect for this action inside `work_area`, with `gap` pixels around
    /// the outside and between adjacent snapped windows.
    pub fn rect(self, work_area: Rect, gap: i32) -> Rect {
        let (x0, y0, x1, y1) = self.sixths();
        let gap = gap.max(0);
        let (w, h) = (work_area.width(), work_area.height());
        // Full gap at the work-area border; interior edges split the gap so two
        // neighbours end up exactly `gap` apart.
        let lo = |frac: i32, size: i32, origin: i32| {
            origin + size * frac / 6 + if frac == 0 { gap } else { gap - gap / 2 }
        };
        let hi = |frac: i32, size: i32, origin: i32| {
            origin + size * frac / 6 - if frac == 6 { gap } else { gap / 2 }
        };
        Rect::new(lo(x0, w, work_area.left), lo(y0, h, work_area.top), hi(x1, w, work_area.left), hi(y1, h, work_area.top))
    }
}

/// Edge-snap configuration (a view of the relevant [`crate::Settings`] fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeSnapConfig {
    pub margin: i32,
    pub corner_size: i32,
    pub top: TopEdgeAction,
    pub bottom: BottomEdgeAction,
}

/// Which action (if any) the cursor at `p` triggers on the monitor with
/// `frame`. `others` are the frames of the remaining monitors, used to ignore
/// edges that continue onto another display.
pub fn edge_action(cfg: &EdgeSnapConfig, frame: Rect, others: &[Rect], p: Point) -> Option<EdgeAction> {
    if !frame.contains(p) {
        return None;
    }
    let m = cfg.margin.max(1);
    let c = cfg.corner_size.max(m);
    // Is the screen continued by another monitor just beyond this point?
    let continued = |q: Point| others.iter().any(|o| o.contains(q));
    let left = p.x - frame.left < m && !continued(Point::new(frame.left - 1, p.y));
    let right = frame.right - 1 - p.x < m && !continued(Point::new(frame.right, p.y));
    let top = p.y - frame.top < m && !continued(Point::new(p.x, frame.top - 1));
    let bottom = frame.bottom - 1 - p.y < m && !continued(Point::new(p.x, frame.bottom));
    let near_left = p.x - frame.left < c;
    let near_right = frame.right - 1 - p.x < c;
    let near_top = p.y - frame.top < c;
    let near_bottom = frame.bottom - 1 - p.y < c;

    // Corners: on an edge and within `corner_size` of the perpendicular edge.
    if (left && near_top) || (top && near_left) {
        return Some(EdgeAction::TopLeft);
    }
    if (right && near_top) || (top && near_right) {
        return Some(EdgeAction::TopRight);
    }
    if (left && near_bottom) || (bottom && near_left) {
        return Some(EdgeAction::BottomLeft);
    }
    if (right && near_bottom) || (bottom && near_right) {
        return Some(EdgeAction::BottomRight);
    }
    if left {
        return Some(EdgeAction::LeftHalf);
    }
    if right {
        return Some(EdgeAction::RightHalf);
    }
    if top {
        return match cfg.top {
            TopEdgeAction::None => None,
            TopEdgeAction::Maximize => Some(EdgeAction::Maximize),
            TopEdgeAction::TopHalf => Some(EdgeAction::TopHalf),
        };
    }
    if bottom {
        return match cfg.bottom {
            BottomEdgeAction::None => None,
            BottomEdgeAction::BottomHalf => Some(EdgeAction::BottomHalf),
            BottomEdgeAction::Thirds => {
                let third = (p.x - frame.left) * 3 / frame.width().max(1);
                Some(match third {
                    0 => EdgeAction::FirstThird,
                    1 => EdgeAction::CenterThird,
                    _ => EdgeAction::LastThird,
                })
            }
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: EdgeSnapConfig =
        EdgeSnapConfig { margin: 5, corner_size: 20, top: TopEdgeAction::Maximize, bottom: BottomEdgeAction::BottomHalf };
    const FRAME: Rect = Rect { left: 0, top: 0, right: 1200, bottom: 800 };
    const WA: Rect = Rect { left: 0, top: 25, right: 1200, bottom: 800 };

    fn act(x: i32, y: i32) -> Option<EdgeAction> {
        edge_action(&CFG, FRAME, &[], Point::new(x, y))
    }

    #[test]
    fn edges_and_corners() {
        assert_eq!(act(0, 400), Some(EdgeAction::LeftHalf));
        assert_eq!(act(1199, 400), Some(EdgeAction::RightHalf));
        assert_eq!(act(600, 0), Some(EdgeAction::Maximize));
        assert_eq!(act(600, 799), Some(EdgeAction::BottomHalf));
        assert_eq!(act(0, 0), Some(EdgeAction::TopLeft));
        assert_eq!(act(0, 15), Some(EdgeAction::TopLeft)); // left edge, near top
        assert_eq!(act(15, 0), Some(EdgeAction::TopLeft)); // top edge, near left
        assert_eq!(act(1199, 10), Some(EdgeAction::TopRight));
        assert_eq!(act(3, 790), Some(EdgeAction::BottomLeft));
        assert_eq!(act(1190, 799), Some(EdgeAction::BottomRight));
        assert_eq!(act(600, 400), None);
    }

    #[test]
    fn margin_boundaries() {
        assert_eq!(act(4, 400), Some(EdgeAction::LeftHalf));
        assert_eq!(act(5, 400), None);
        assert_eq!(act(1195, 400), Some(EdgeAction::RightHalf));
        assert_eq!(act(1194, 400), None);
        assert_eq!(act(600, 4), Some(EdgeAction::Maximize));
        assert_eq!(act(600, 5), None);
        // corner size boundary: 19 px from top on the left edge is a corner, 20 is a half
        assert_eq!(act(0, 19), Some(EdgeAction::TopLeft));
        assert_eq!(act(0, 20), Some(EdgeAction::LeftHalf));
        // outside the frame
        assert_eq!(act(-1, 400), None);
    }

    #[test]
    fn top_and_bottom_options() {
        let mut c = CFG;
        c.top = TopEdgeAction::TopHalf;
        c.bottom = BottomEdgeAction::Thirds;
        let a = |x, y| edge_action(&c, FRAME, &[], Point::new(x, y));
        assert_eq!(a(600, 0), Some(EdgeAction::TopHalf));
        assert_eq!(a(100, 799), Some(EdgeAction::FirstThird));
        assert_eq!(a(600, 799), Some(EdgeAction::CenterThird));
        assert_eq!(a(1100, 799), Some(EdgeAction::LastThird));
        c.top = TopEdgeAction::None;
        c.bottom = BottomEdgeAction::None;
        let a = |x, y| edge_action(&c, FRAME, &[], Point::new(x, y));
        assert_eq!(a(600, 0), None);
        assert_eq!(a(600, 799), None);
        // corners still work
        assert_eq!(a(0, 0), Some(EdgeAction::TopLeft));
    }

    #[test]
    fn shared_edges_between_monitors_are_ignored() {
        let right_mon = Rect::new(1200, 0, 2400, 800);
        let a = |x, y| edge_action(&CFG, FRAME, &[right_mon], Point::new(x, y));
        assert_eq!(a(1199, 400), None); // continues onto the right monitor
        assert_eq!(a(0, 400), Some(EdgeAction::LeftHalf));
        // the top edge is still free, so its corner zone still gives a quarter
        assert_eq!(a(1199, 0), Some(EdgeAction::TopRight));
        // ...but along the shared side nothing triggers
        assert_eq!(a(1199, 10), None);
        // the right monitor's left edge is shared too
        let b = |x, y| edge_action(&CFG, right_mon, &[FRAME], Point::new(x, y));
        assert_eq!(b(1200, 400), None);
        assert_eq!(b(2399, 400), Some(EdgeAction::RightHalf));
        // a shorter monitor beside: only the overlapping part of the edge is shared
        let short = Rect::new(1200, 0, 2000, 400);
        let c = |x, y| edge_action(&CFG, FRAME, &[short], Point::new(x, y));
        assert_eq!(c(1199, 200), None);
        assert_eq!(c(1199, 600), Some(EdgeAction::RightHalf));
    }

    #[test]
    fn rects_and_gaps() {
        assert_eq!(EdgeAction::LeftHalf.rect(WA, 0), Rect::new(0, 25, 600, 800));
        assert_eq!(EdgeAction::RightHalf.rect(WA, 0), Rect::new(600, 25, 1200, 800));
        assert_eq!(EdgeAction::Maximize.rect(WA, 0), WA);
        assert_eq!(EdgeAction::BottomRight.rect(WA, 0), Rect::new(600, 412, 1200, 800));
        assert_eq!(EdgeAction::CenterThird.rect(WA, 0), Rect::new(400, 25, 800, 800));
        // gap 10: 10 at the outside, 10 between neighbours
        let l = EdgeAction::LeftHalf.rect(WA, 10);
        let r = EdgeAction::RightHalf.rect(WA, 10);
        assert_eq!(l, Rect::new(10, 35, 595, 790));
        assert_eq!(r, Rect::new(605, 35, 1190, 790));
        assert_eq!(r.left - l.right, 10);
        // odd gap still totals exactly between neighbours
        let l = EdgeAction::LeftHalf.rect(WA, 7);
        let r = EdgeAction::RightHalf.rect(WA, 7);
        assert_eq!(r.left - l.right, 7);
    }
}
