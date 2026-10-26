//! Dock geometry: orientation, visibility and which icon is under the
//! pointer. Global coordinates, top-left origin, y grows down (points).

/// Axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    /// Half-open containment (`[x, right) × [y, bottom)`).
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn intersection_area(&self, o: &RectF) -> f64 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    }
    pub fn union(&self, o: &RectF) -> RectF {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        RectF::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inset(&self, d: f64) -> RectF {
        RectF::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn offset(&self, dx: f64, dy: f64) -> RectF {
        RectF::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

/// Which screen edge the Dock is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Orientation {
    #[default]
    Bottom,
    Left,
    Right,
}

impl Orientation {
    /// Infer from the Dock's list frame and the screen it is on: a wide
    /// strip is a bottom Dock; a tall one is on whichever side it is nearer.
    pub fn infer(list: RectF, screen: RectF) -> Orientation {
        if list.w >= list.h {
            return Orientation::Bottom;
        }
        let (cx, _) = list.center();
        let (sx, _) = screen.center();
        if cx < sx {
            Orientation::Left
        } else {
            Orientation::Right
        }
    }

    /// Whether icons run horizontally.
    pub fn horizontal(self) -> bool {
        self == Orientation::Bottom
    }
}

/// Kind of Dock tile (from the AX subrole).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// `AXApplicationDockItem`
    App,
    /// `AXSeparatorDockItem`
    Separator,
    /// `AXTrashDockItem`
    Trash,
    /// Folders, files, URLs, minimized-window tiles, ...
    Other,
}

impl ItemKind {
    pub fn from_subrole(s: &str) -> ItemKind {
        match s {
            "AXApplicationDockItem" => ItemKind::App,
            "AXSeparatorDockItem" => ItemKind::Separator,
            "AXTrashDockItem" => ItemKind::Trash,
            _ => ItemKind::Other,
        }
    }
}

/// One Dock tile.
#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    pub kind: ItemKind,
    pub title: String,
    /// Bundle path for apps (from `AXURL`), empty otherwise.
    pub path: String,
    /// `AXIsApplicationRunning`.
    pub running: bool,
    pub frame: RectF,
}

/// A snapshot of the Dock.
#[derive(Debug, Clone, PartialEq)]
pub struct Dock {
    pub orientation: Orientation,
    /// Frame of the `AXList` holding the tiles.
    pub list: RectF,
    /// The screen the Dock is on (global frame).
    pub screen: RectF,
    /// Tiles in Dock order.
    pub items: Vec<DockItem>,
}

/// Pointer slack (points) toward the screen interior on top of the icons,
/// so the very top pixel row of an icon still counts.
const INNER_SLACK: f64 = 2.0;

impl Dock {
    /// Build from a list frame and the screens; picks the screen the list is
    /// mostly on (or nearest to, for a hidden Dock).
    pub fn new(list: RectF, items: Vec<DockItem>, screens: &[RectF]) -> Dock {
        let screen = screen_for(list, screens);
        Dock { orientation: Orientation::infer(list, screen), list, screen, items }
    }

    /// Whether the Dock is on screen: at least half of the list is inside its
    /// screen. An auto-hidden Dock is moved (almost) entirely off screen.
    pub fn is_visible(&self) -> bool {
        let a = self.list.w * self.list.h;
        a > 0.0 && self.list.intersection_area(&self.screen) >= a * 0.5
    }

    /// The band in which the pointer is "on the Dock": the union of the list
    /// and the tiles (which grow with magnification), extended to the screen
    /// edge the Dock is attached to.
    pub fn band(&self) -> RectF {
        let mut r = self.list;
        for it in &self.items {
            r = r.union(&it.frame);
        }
        let s = self.screen;
        match self.orientation {
            Orientation::Bottom => {
                let top = r.y - INNER_SLACK;
                RectF::new(r.x, top, r.w, (s.bottom() - top).max(0.0))
            }
            Orientation::Left => RectF::new(s.x, r.y, (r.right() + INNER_SLACK - s.x).max(0.0), r.h),
            Orientation::Right => {
                let left = r.x - INNER_SLACK;
                RectF::new(left, r.y, (s.right() - left).max(0.0), r.h)
            }
        }
    }

    /// `band` grown by `margin` toward the screen interior and along the
    /// Dock (where the worker polls quickly).
    pub fn zone(&self, margin: f64) -> RectF {
        let b = self.band();
        match self.orientation {
            Orientation::Bottom => RectF::new(b.x - margin, b.y - margin, b.w + 2.0 * margin, b.h + margin),
            Orientation::Left => RectF::new(b.x, b.y - margin, b.w + margin, b.h + 2.0 * margin),
            Orientation::Right => RectF::new(b.x - margin, b.y - margin, b.w + margin, b.h + 2.0 * margin),
        }
    }

    /// Index of the tile under (x, y). Tiles own the space up to the midpoint
    /// to their neighbours, so there are no dead gaps between icons, and the
    /// whole depth of the band down to the screen edge.
    pub fn item_at(&self, x: f64, y: f64) -> Option<usize> {
        if self.items.is_empty() || !self.is_visible() || !self.band().contains(x, y) {
            return None;
        }
        let h = self.orientation.horizontal();
        let main = |r: &RectF| if h { (r.x, r.right()) } else { (r.y, r.bottom()) };
        let p = if h { x } else { y };
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by(|&a, &b| main(&self.items[a].frame).0.total_cmp(&main(&self.items[b].frame).0));
        for (k, &i) in order.iter().enumerate() {
            let (s, e) = main(&self.items[i].frame);
            let lo = if k == 0 { s } else { (main(&self.items[order[k - 1]].frame).1 + s) / 2.0 };
            let hi = if k + 1 == order.len() { e } else { (e + main(&self.items[order[k + 1]].frame).0) / 2.0 };
            if p >= lo && p < hi {
                return Some(i);
            }
        }
        None
    }

    /// The running app tile under (x, y), if any.
    pub fn app_at(&self, x: f64, y: f64) -> Option<&DockItem> {
        let it = &self.items[self.item_at(x, y)?];
        (it.kind == ItemKind::App && it.running && !it.path.is_empty()).then_some(it)
    }
}

/// Screen with the largest overlap with `r`, else the nearest one by center
/// distance, else a default.
pub fn screen_for(r: RectF, screens: &[RectF]) -> RectF {
    let mut best: Option<(f64, RectF)> = None;
    for s in screens {
        let a = s.intersection_area(&r);
        if a > 0.0 && best.is_none_or(|(b, _)| a > b) {
            best = Some((a, *s));
        }
    }
    if let Some((_, s)) = best {
        return s;
    }
    let (cx, cy) = r.center();
    screens
        .iter()
        .min_by(|a, b| {
            let d = |s: &RectF| {
                let dx = (s.x - cx).max(0.0).max(cx - s.right());
                let dy = (s.y - cy).max(0.0).max(cy - s.bottom());
                dx * dx + dy * dy
            };
            d(a).total_cmp(&d(b))
        })
        .copied()
        .unwrap_or(RectF::new(0.0, 0.0, 1440.0, 900.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(title: &str, frame: RectF) -> DockItem {
        DockItem { kind: ItemKind::App, title: title.into(), path: format!("/Applications/{title}.app"), running: true, frame }
    }

    const SCREEN: RectF = RectF::new(0.0, 0.0, 1512.0, 982.0);

    /// A bottom Dock like the one on the test machine: 46-pt tiles from x=321.
    fn bottom_dock() -> Dock {
        let mut items = Vec::new();
        let mut x = 321.0;
        for (i, t) in ["Finder", "Safari", "|", "TextEdit", "Trash"].iter().enumerate() {
            let w = if *t == "|" { 20.0 } else { 46.0 };
            let mut it = app(t, RectF::new(x, 919.0, w, 58.0));
            if *t == "|" {
                it.kind = ItemKind::Separator;
            }
            if *t == "Trash" {
                it.kind = ItemKind::Trash;
            }
            if i == 1 {
                it.running = false;
            }
            items.push(it);
            x += w;
        }
        Dock::new(RectF::new(318.0, 914.0, x - 318.0 + 3.0, 58.0), items, &[SCREEN])
    }

    fn side_dock(left: bool) -> Dock {
        let x = if left { 0.0 } else { SCREEN.w - 62.0 };
        let items: Vec<DockItem> = (0..4).map(|i| app(&format!("A{i}"), RectF::new(x + 4.0, 300.0 + 50.0 * i as f64, 58.0, 50.0))).collect();
        Dock::new(RectF::new(x, 296.0, 62.0, 208.0), items, &[SCREEN])
    }

    #[test]
    fn orientation_inference() {
        assert_eq!(bottom_dock().orientation, Orientation::Bottom);
        assert_eq!(side_dock(true).orientation, Orientation::Left);
        assert_eq!(side_dock(false).orientation, Orientation::Right);
        // Second screen to the right: a tall Dock on its left edge.
        let s2 = RectF::new(1512.0, 0.0, 1920.0, 1080.0);
        let d = Dock::new(RectF::new(1512.0, 400.0, 60.0, 300.0), vec![], &[SCREEN, s2]);
        assert_eq!(d.screen, s2);
        assert_eq!(d.orientation, Orientation::Left);
    }

    #[test]
    fn bottom_hit_testing() {
        let d = bottom_dock();
        assert!(d.is_visible());
        // Middle of Finder; bottom screen edge row still hits.
        assert_eq!(d.item_at(344.0, 940.0), Some(0));
        assert_eq!(d.item_at(344.0, 981.5), Some(0));
        // Above the band: nothing.
        assert_eq!(d.item_at(344.0, 900.0), None);
        // Between icons (exact boundary) belongs to the right one.
        assert_eq!(d.item_at(367.0, 940.0), Some(1));
        // Left of the first tile / right of the last.
        assert_eq!(d.item_at(300.0, 940.0), None);
        assert_eq!(d.item_at(600.0, 940.0), None);
        // app_at: Safari isn't running, separator & trash aren't apps.
        assert_eq!(d.app_at(344.0, 940.0).map(|i| i.title.as_str()), Some("Finder"));
        assert!(d.app_at(390.0, 940.0).is_none());
        assert!(d.app_at(420.0, 940.0).is_none());
        assert_eq!(d.app_at(450.0, 940.0).map(|i| i.title.as_str()), Some("TextEdit"));
        assert!(d.app_at(500.0, 940.0).is_none());
    }

    #[test]
    fn gaps_split_at_midpoints() {
        // Tiles with gaps (spacing): 0..40 and 60..100.
        let items = vec![app("A", RectF::new(0.0, 940.0, 40.0, 40.0)), app("B", RectF::new(60.0, 940.0, 40.0, 40.0))];
        let d = Dock::new(RectF::new(0.0, 940.0, 100.0, 42.0), items, &[SCREEN]);
        assert_eq!(d.item_at(49.9, 950.0), Some(0));
        assert_eq!(d.item_at(50.0, 950.0), Some(1));
    }

    #[test]
    fn side_hit_testing() {
        let l = side_dock(true);
        assert_eq!(l.item_at(30.0, 325.0), Some(0));
        assert_eq!(l.item_at(0.0, 375.0), Some(1)); // screen edge column
        assert_eq!(l.item_at(70.0, 375.0), None); // right of the band
        assert_eq!(l.item_at(30.0, 520.0), None);
        let r = side_dock(false);
        assert_eq!(r.item_at(SCREEN.w - 1.0, 475.0), Some(3));
        assert_eq!(r.item_at(SCREEN.w - 30.0, 425.0), Some(2));
        assert_eq!(r.item_at(SCREEN.w - 80.0, 425.0), None);
        // The zone extends inward.
        assert!(r.zone(40.0).contains(SCREEN.w - 80.0, 425.0));
        assert!(!r.zone(40.0).contains(SCREEN.w - 120.0, 425.0));
    }

    #[test]
    fn magnified_tiles_extend_the_band() {
        let mut d = bottom_dock();
        // Magnification: the hovered tile grows above the list.
        d.items[3].frame = RectF::new(433.0, 860.0, 46.0, 117.0);
        assert_eq!(d.item_at(450.0, 870.0), Some(3));
    }

    #[test]
    fn hidden_dock_hits_nothing() {
        let mut d = bottom_dock();
        // Auto-hidden: moved below the screen.
        d.list = d.list.offset(0.0, 70.0);
        for it in &mut d.items {
            it.frame = it.frame.offset(0.0, 70.0);
        }
        assert!(!d.is_visible());
        assert_eq!(d.item_at(344.0, 981.0), None);
    }

    #[test]
    fn screen_for_hidden_list_uses_nearest() {
        let s2 = RectF::new(1512.0, 0.0, 1920.0, 1080.0);
        // Entirely below screen 2.
        assert_eq!(screen_for(RectF::new(2000.0, 1085.0, 400.0, 60.0), &[SCREEN, s2]), s2);
        assert_eq!(screen_for(RectF::new(10.0, 10.0, 10.0, 10.0), &[]), RectF::new(0.0, 0.0, 1440.0, 900.0));
    }
}
