//! Where the preview panel goes: next to the hovered icon on the screen
//! interior side of the Dock, centered on the icon, kept inside the
//! screen; plus the "bridge" between icon and panel that keeps the panel
//! open while the pointer travels from one to the other.

use crate::geom::{Orientation, RectF};

/// Gap between the Dock band and the panel (points).
pub const GAP: f64 = 8.0;
/// Minimum distance from the screen edges.
pub const MARGIN: f64 = 8.0;

/// Result of [`place`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Panel frame (global, top-left origin).
    pub panel: RectF,
    /// Region between the icon and the panel.
    pub bridge: RectF,
}

/// Place a `w`×`h` panel for `icon` on a Dock whose band is `band`.
/// `area` is the usable screen area (the screen minus the menu bar).
pub fn place(icon: RectF, band: RectF, orientation: Orientation, w: f64, h: f64, area: RectF) -> Placement {
    let clamp = |v: f64, lo: f64, hi: f64| if hi < lo { lo } else { v.clamp(lo, hi) };
    let (ix, iy) = icon.center();
    let panel = match orientation {
        Orientation::Bottom => {
            let x = clamp((ix - w / 2.0).round(), area.x + MARGIN, area.right() - MARGIN - w);
            let y = (band.y - GAP - h).max(area.y + MARGIN);
            RectF::new(x, y.round(), w, h)
        }
        Orientation::Left => {
            let y = clamp((iy - h / 2.0).round(), area.y + MARGIN, area.bottom() - MARGIN - h);
            let x = (band.right() + GAP).min(area.right() - MARGIN - w).max(area.x + MARGIN);
            RectF::new(x.round(), y, w, h)
        }
        Orientation::Right => {
            let y = clamp((iy - h / 2.0).round(), area.y + MARGIN, area.bottom() - MARGIN - h);
            let x = (band.x - GAP - w).max(area.x + MARGIN);
            RectF::new(x.round(), y, w, h)
        }
    };
    // The gap strip between the panel and the Dock band, spanning the panel
    // and the icon (the icon itself counts as "on the icon" down to the
    // screen edge, so the rest of the Dock row is not part of the bridge).
    let bridge = match orientation {
        Orientation::Bottom => {
            let x0 = panel.x.min(icon.x);
            let x1 = panel.right().max(icon.right());
            let y1 = band.y.max(panel.bottom());
            RectF::new(x0, panel.bottom(), x1 - x0, y1 - panel.bottom() + 1.0)
        }
        Orientation::Left => {
            let y0 = panel.y.min(icon.y);
            let y1 = panel.bottom().max(icon.bottom());
            let x0 = band.right().min(panel.x);
            RectF::new(x0 - 1.0, y0, panel.x - x0 + 1.0, y1 - y0)
        }
        Orientation::Right => {
            let y0 = panel.y.min(icon.y);
            let y1 = panel.bottom().max(icon.bottom());
            let x1 = band.x.max(panel.right());
            RectF::new(panel.right(), y0, x1 - panel.right() + 1.0, y1 - y0)
        }
    };
    Placement { panel, bridge }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: RectF = RectF::new(0.0, 0.0, 1512.0, 982.0);
    const AREA: RectF = RectF::new(0.0, 25.0, 1512.0, 957.0);

    fn inside(r: RectF, a: RectF) -> bool {
        r.x >= a.x + MARGIN - 1e-9 && r.y >= a.y + MARGIN - 1e-9 && r.right() <= a.right() - MARGIN + 1e-9 && r.bottom() <= a.bottom() + 1e-9
    }

    #[test]
    fn bottom_centered_above_icon() {
        let band = RectF::new(318.0, 912.0, 876.0, 70.0);
        let icon = RectF::new(1032.0, 919.0, 46.0, 58.0);
        let p = place(icon, band, Orientation::Bottom, 400.0, 220.0, AREA);
        assert_eq!(p.panel, RectF::new(855.0, 912.0 - GAP - 220.0, 400.0, 220.0));
        assert!(inside(p.panel, AREA));
        // Bridge: the gap between panel and Dock, over the panel's span.
        assert_eq!(p.bridge.y, p.panel.bottom());
        assert_eq!(p.bridge.bottom(), 913.0);
        assert!(p.bridge.contains(icon.center().0, p.panel.bottom() + 1.0));
        assert!(p.bridge.contains(icon.center().0, 912.5));
        assert!(p.bridge.contains(860.0, 908.0));
        // Other icons in the Dock row are not bridge.
        assert!(!p.bridge.contains(860.0, 940.0));
        let _ = SCREEN;
    }

    #[test]
    fn bottom_clamped_at_screen_edges() {
        let band = RectF::new(0.0, 912.0, 1512.0, 70.0);
        let left = place(RectF::new(4.0, 919.0, 46.0, 58.0), band, Orientation::Bottom, 600.0, 200.0, AREA);
        assert_eq!(left.panel.x, MARGIN);
        let right = place(RectF::new(1460.0, 919.0, 46.0, 58.0), band, Orientation::Bottom, 600.0, 200.0, AREA);
        assert_eq!(right.panel.right(), SCREEN.right() - MARGIN);
        // Icon outside the panel's span: the bridge still covers it.
        assert!(right.bridge.x <= 1460.0 && right.bridge.right() >= 1506.0);
        // Too tall: pinned below the menu bar.
        let tall = place(RectF::new(700.0, 919.0, 46.0, 58.0), band, Orientation::Bottom, 600.0, 2000.0, AREA);
        assert_eq!(tall.panel.y, AREA.y + MARGIN);
        // Wider than the screen: starts at the margin.
        let wide = place(RectF::new(700.0, 919.0, 46.0, 58.0), band, Orientation::Bottom, 3000.0, 200.0, AREA);
        assert_eq!(wide.panel.x, AREA.x + MARGIN);
    }

    #[test]
    fn left_dock_panel_to_the_right() {
        let band = RectF::new(0.0, 296.0, 64.0, 400.0);
        let icon = RectF::new(4.0, 300.0, 58.0, 50.0);
        let p = place(icon, band, Orientation::Left, 300.0, 500.0, AREA);
        assert_eq!(p.panel.x, 64.0 + GAP);
        // Centered on the icon (325).
        assert_eq!(p.panel.y, 75.0);
        // Near the top: clamped below the menu bar.
        let top = place(RectF::new(4.0, 30.0, 58.0, 50.0), band, Orientation::Left, 300.0, 500.0, AREA);
        assert_eq!(top.panel.y, AREA.y + MARGIN);
        assert!(inside(p.panel, AREA));
        assert_eq!(p.bridge.right(), p.panel.x);
        assert!(p.bridge.contains(63.5, 325.0) && p.bridge.contains(70.0, 325.0));
        assert!(!p.bridge.contains(30.0, 325.0));
    }

    #[test]
    fn right_dock_panel_to_the_left() {
        let band = RectF::new(1448.0, 296.0, 64.0, 400.0);
        let icon = RectF::new(1450.0, 600.0, 58.0, 50.0);
        let p = place(icon, band, Orientation::Right, 300.0, 200.0, AREA);
        assert_eq!(p.panel.right(), 1448.0 - GAP);
        assert_eq!(p.panel.y, 525.0);
        assert_eq!(p.bridge.x, p.panel.right());
        assert!(p.bridge.contains(1440.0, 620.0) && p.bridge.contains(1448.5, 620.0));
        assert!(!p.bridge.contains(1480.0, 620.0));
        // Near the bottom: clamped up.
        let low = place(RectF::new(1450.0, 950.0, 58.0, 30.0), band, Orientation::Right, 300.0, 200.0, AREA);
        assert_eq!(low.panel.bottom(), AREA.bottom() - MARGIN);
    }
}
