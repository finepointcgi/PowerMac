//! Pure logic, independent of AppKit: section assignment from separator
//! positions, notch overlap math, the auto-rehide timer, hover dwell, the
//! separator length on macOS 26+, and the Cmd-drag geometry used to move
//! items.
//!
//! All coordinates are global points with a top-left origin, y down (the
//! space of `CGWindowListCopyWindowInfo`, `CGEventGetLocation` and the
//! Accessibility API).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn overlaps_x(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right()
    }
}

/// Where an item lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Section {
    /// Always shown (right of the hidden-section separator).
    #[default]
    Visible,
    /// Shown when the chevron is clicked.
    Hidden,
    /// Shown only on Option-click of the chevron (or in the floating bar).
    AlwaysHidden,
}

/// Which sections are currently shown in the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Expansion {
    /// Only the visible section.
    #[default]
    Collapsed,
    /// Visible + hidden.
    Hidden,
    /// Everything, including the always-hidden section.
    All,
}

impl Expansion {
    pub fn shows(self, s: Section) -> bool {
        match s {
            Section::Visible => true,
            Section::Hidden => self != Expansion::Collapsed,
            Section::AlwaysHidden => self == Expansion::All,
        }
    }
}

/// Assign a section from an item's frame and the right edges of our
/// separators. `always_hidden_right` is `None` when there is no
/// always-hidden separator *or* its frame can't be trusted right now (on
/// macOS 26+ a collapsed hidden section parks the separator). Returns
/// `None` when the item is left of the hidden separator but which of the two
/// left sections it is in can't be told.
pub fn assign_section(item: &Rect, hidden_right: f64, always_hidden_right: Option<f64>, has_always_hidden: bool) -> Option<Section> {
    let cx = item.center_x();
    if cx > hidden_right {
        return Some(Section::Visible);
    }
    if !has_always_hidden {
        return Some(Section::Hidden);
    }
    match always_hidden_right {
        Some(a) if cx > a => Some(Section::Hidden),
        Some(_) => Some(Section::AlwaysHidden),
        None => None,
    }
}

/// Our three control items should be ordered (left to right)
/// always-hidden separator, hidden separator, chevron. Checked only while
/// everything is shown (frames are then all real).
pub fn order_problem(toggle: Rect, hidden: Rect, always_hidden: Option<Rect>) -> Option<&'static str> {
    if toggle.center_x() < hidden.center_x() {
        return Some("the chevron was dragged left of the hidden-section separator; drag it back to the right of the separator");
    }
    if let Some(a) = always_hidden {
        if a.center_x() > hidden.center_x() {
            return Some("the always-hidden separator was dragged right of the hidden-section separator");
        }
    }
    None
}

/// The notch as a rect in global top-left coordinates, from a screen frame
/// (top-left), the menu bar height and `NSScreen.auxiliaryTopLeftArea` /
/// `auxiliaryTopRightArea` widths (given as `left_area_right` = x where the
/// left area ends and `right_area_left` = x where the right area begins, in
/// the same global space).
pub fn notch_rect(screen: Rect, menu_bar_h: f64, left_area_right: f64, right_area_left: f64) -> Option<Rect> {
    if right_area_left - left_area_right < 1.0 || left_area_right <= screen.x || right_area_left >= screen.right() {
        return None;
    }
    Some(Rect::new(left_area_right, screen.y, right_area_left - left_area_right, menu_bar_h))
}

/// Whether a status item that is supposed to be shown can't be seen
/// because of the notch: it overlaps the camera housing, or it was pushed
/// left of it (into the app-menu side, where macOS doesn't draw status
/// items). Only items in the menu bar of the notched screen count.
pub fn hidden_by_notch(item: &Rect, screen: &Rect, notch: &Rect) -> bool {
    let on_screen_bar = item.y < notch.bottom() && item.bottom() > screen.y - 1.0 && item.right() > screen.x && item.x < screen.right();
    if !on_screen_bar || item.w <= 0.0 {
        return false;
    }
    item.x < notch.right() - 0.5
}

/// Would showing a section in place push something behind the notch?
/// `right_edge` is where the leftmost currently shown status item begins
/// (the hidden separator's left edge once it's short), `widths` the widths
/// of the items that would appear.
pub fn expansion_overflows_notch(right_edge: f64, notch: &Rect, widths: &[f64]) -> bool {
    let need: f64 = widths.iter().sum();
    right_edge - need < notch.right()
}

/// Auto-rehide: hide `delay` seconds after the last activity (the pointer
/// in the menu bar or the floating bar, an open menu, a click).
#[derive(Clone, Debug, Default)]
pub struct RehideTimer {
    deadline: Option<f64>,
}

impl RehideTimer {
    /// Arm (or re-arm) when a section is shown.
    pub fn start(&mut self, now: f64, delay: f64) {
        self.deadline = Some(now + delay.max(0.0));
    }
    pub fn cancel(&mut self) {
        self.deadline = None;
    }
    pub fn armed(&self) -> bool {
        self.deadline.is_some()
    }
    /// Returns true once when it's time to hide. Activity pushes the
    /// deadline out to `now + delay`.
    pub fn tick(&mut self, now: f64, delay: f64, activity: bool) -> bool {
        let Some(d) = self.deadline else { return false };
        if activity {
            self.deadline = Some(d.max(now + delay.max(0.0)));
            return false;
        }
        if now >= d {
            self.deadline = None;
            return true;
        }
        false
    }
}

/// Show-on-hover: the pointer must rest in the empty part of the menu bar
/// for `dwell` seconds; a section shown this way hides again once the
/// pointer has been out of the menu bar for `leave` seconds (unless a menu
/// is open).
#[derive(Clone, Debug, Default)]
pub struct Hover {
    since: Option<f64>,
    away_since: Option<f64>,
    pub shown_by_hover: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverAction {
    None,
    Show,
    Hide,
}

/// What the pointer and menu bar are doing this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct HoverInput {
    /// Over the empty part of the menu bar (no button pressed).
    pub over_empty_bar: bool,
    /// Anywhere in the menu bar (or the floating bar).
    pub in_menu_bar: bool,
    /// A hidden section is shown.
    pub shown: bool,
    pub menu_open: bool,
}

impl Hover {
    pub fn tick(&mut self, now: f64, i: HoverInput, dwell: f64, leave: f64) -> HoverAction {
        let HoverInput { over_empty_bar, in_menu_bar, shown, menu_open } = i;
        if !shown {
            self.shown_by_hover = false;
            self.away_since = None;
            if over_empty_bar {
                let s = *self.since.get_or_insert(now);
                if now - s >= dwell {
                    self.since = None;
                    self.shown_by_hover = true;
                    return HoverAction::Show;
                }
            } else {
                self.since = None;
            }
            return HoverAction::None;
        }
        self.since = None;
        if !self.shown_by_hover {
            return HoverAction::None;
        }
        if in_menu_bar || menu_open {
            self.away_since = None;
            return HoverAction::None;
        }
        let a = *self.away_since.get_or_insert(now);
        if now - a >= leave {
            self.away_since = None;
            self.shown_by_hover = false;
            return HoverAction::Hide;
        }
        HoverAction::None
    }
}

/// Separator length on macOS 26+, where the menu bar moves status items
/// that don't fit (or would cross the notch) into its overflow ("»"),
/// together with everything left of them, instead of letting them be
/// pushed off-screen. The separator is made long enough to fill the space
/// between `limit` (the notch's right edge, or the end of the app menus)
/// and its right edge `right`: every item left of it would have to cross
/// the limit and is taken out of the bar. `pad` is how much wider a status
/// item window is than its length. When there's no room right of the limit,
/// the separator is made to cross it, which sends it to the overflow too.
pub fn fitted_length(right: f64, screen: &Rect, notch: Option<&Rect>, menus_right: Option<f64>, pad: f64) -> f64 {
    let limit = match (notch, menus_right) {
        (Some(n), _) if n.right() < right => n.right() + 4.0,
        (_, Some(m)) if m < right => m + 12.0,
        _ => screen.x + (right - screen.x) * 0.5,
    };
    let l = right - limit - pad;
    if l < 24.0 {
        return (right - limit + 40.0).max(60.0);
    }
    l
}

/// A smooth (ease-out) sequence of lengths from `from` to `to`.
pub fn animation_steps(from: f64, to: f64, steps: usize) -> Vec<f64> {
    let n = steps.max(1);
    (1..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let e = 1.0 - (1.0 - t).powi(3);
            from + (to - from) * e
        })
        .collect()
}

/// Where to drop an item being Cmd-dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropSide {
    LeftOf,
    RightOf,
}

/// The Cmd-drag path: from the item's center to the target edge, with
/// intermediate points so the menu bar sees a real drag.
pub fn drag_path(item: &Rect, target: &Rect, side: DropSide, steps: usize) -> Vec<(f64, f64)> {
    let start = (item.center_x(), item.center_y());
    let end_x = match side {
        DropSide::LeftOf => target.x + 1.0,
        DropSide::RightOf => target.right() - 1.0,
    };
    let end = (end_x, target.center_y());
    let n = steps.max(1);
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            (start.0 + (end.0 - start.0) * t, start.1 + (end.1 - start.1) * t)
        })
        .collect()
}

/// After a move, is the item on the requested side of the target?
pub fn move_succeeded(item: &Rect, target: &Rect, side: DropSide) -> bool {
    match side {
        DropSide::LeftOf => item.center_x() < target.center_x(),
        DropSide::RightOf => item.center_x() > target.center_x(),
    }
}

/// Stable identity for an item across scans: owner + what it calls itself
/// (accessibility identifier / title / description) + its index among the
/// owner's items as a last resort.
pub fn item_id(owner: &str, bundle_id: Option<&str>, label: Option<&str>, index: usize) -> String {
    let who = bundle_id.filter(|b| !b.is_empty()).unwrap_or(owner);
    match label.filter(|l| !l.is_empty()) {
        Some(l) => format!("{who}/{l}"),
        None => format!("{who}#{index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_from_separator_positions() {
        // L→R: [A1][AH sep 100..112][H1][H sep 200..212][chevron 212..230][V1]
        let ah = Rect::new(100.0, 0.0, 12.0, 24.0);
        let h = Rect::new(200.0, 0.0, 12.0, 24.0);
        let at = |x: f64| Rect::new(x, 0.0, 20.0, 24.0);
        let s = |r: Rect| assign_section(&r, h.right(), Some(ah.right()), true);
        assert_eq!(s(at(60.0)), Some(Section::AlwaysHidden));
        assert_eq!(s(at(150.0)), Some(Section::Hidden));
        assert_eq!(s(at(260.0)), Some(Section::Visible));
        // Exactly on the separator boundary: center decides.
        assert_eq!(s(at(205.0)), Some(Section::Visible));
        assert_eq!(s(at(190.0)), Some(Section::Hidden));
        // Collapsed in classic mode: everything left of H is off-screen
        // (negative x) but still ordered.
        let h2 = Rect::new(-9800.0, 0.0, 10_000.0, 24.0);
        let ah2 = Rect::new(-9900.0, 0.0, 12.0, 24.0);
        assert_eq!(assign_section(&at(-9850.0), h2.right(), Some(ah2.right()), true), Some(Section::Hidden));
        assert_eq!(assign_section(&at(-9990.0), h2.right(), Some(ah2.right()), true), Some(Section::AlwaysHidden));
        assert_eq!(assign_section(&at(260.0), h2.right(), Some(ah2.right()), true), Some(Section::Visible));
        // Always-hidden separator unknown → can't split the left side.
        assert_eq!(assign_section(&at(60.0), h.right(), None, true), None);
        // No always-hidden section: left of H is just hidden.
        assert_eq!(assign_section(&at(60.0), h.right(), None, false), Some(Section::Hidden));
    }

    #[test]
    fn control_item_order() {
        let ah = Rect::new(100.0, 0.0, 12.0, 24.0);
        let h = Rect::new(200.0, 0.0, 12.0, 24.0);
        let t = Rect::new(212.0, 0.0, 20.0, 24.0);
        assert!(order_problem(t, h, Some(ah)).is_none());
        assert!(order_problem(Rect::new(150.0, 0.0, 20.0, 24.0), h, Some(ah)).is_some());
        assert!(order_problem(t, h, Some(Rect::new(300.0, 0.0, 12.0, 24.0))).is_some());
        assert!(order_problem(t, h, None).is_none());
    }

    #[test]
    fn expansion_shows() {
        assert!(Expansion::Collapsed.shows(Section::Visible));
        assert!(!Expansion::Collapsed.shows(Section::Hidden));
        assert!(Expansion::Hidden.shows(Section::Hidden) && !Expansion::Hidden.shows(Section::AlwaysHidden));
        assert!(Expansion::All.shows(Section::AlwaysHidden));
    }

    #[test]
    fn notch_math() {
        // 14" MacBook Pro: 1512 wide, notch 663.5..848.5, menu bar 32.
        let screen = Rect::new(0.0, 0.0, 1512.0, 982.0);
        let notch = notch_rect(screen, 32.0, 663.5, 848.5).unwrap();
        assert_eq!(notch, Rect::new(663.5, 0.0, 185.0, 32.0));
        assert!(notch_rect(screen, 32.0, 0.0, 0.0).is_none());
        assert!(notch_rect(screen, 32.0, 700.0, 700.0).is_none());
        let item = |x: f64, w: f64| Rect::new(x, 4.0, w, 24.0);
        assert!(!hidden_by_notch(&item(900.0, 30.0), &screen, &notch));
        assert!(!hidden_by_notch(&item(848.5, 30.0), &screen, &notch));
        assert!(hidden_by_notch(&item(830.0, 30.0), &screen, &notch), "straddles the right edge");
        assert!(hidden_by_notch(&item(700.0, 30.0), &screen, &notch), "inside");
        assert!(hidden_by_notch(&item(400.0, 30.0), &screen, &notch), "pushed left of the notch");
        // Another screen's menu bar (to the right) is unaffected.
        assert!(!hidden_by_notch(&Rect::new(1600.0, 0.0, 30.0, 24.0), &screen, &notch));
        // Lower down (not in the menu bar): ignored.
        assert!(!hidden_by_notch(&Rect::new(700.0, 300.0, 30.0, 24.0), &screen, &notch));
        // Expanding three 30pt items from 1000: room is 1000-848.5 = 151.5.
        assert!(!expansion_overflows_notch(1000.0, &notch, &[30.0, 30.0, 30.0]));
        assert!(expansion_overflows_notch(1000.0, &notch, &[60.0, 60.0, 40.0]));
    }

    #[test]
    fn rehide_timer() {
        let mut t = RehideTimer::default();
        assert!(!t.tick(0.0, 10.0, false));
        t.start(0.0, 10.0);
        assert!(t.armed());
        assert!(!t.tick(5.0, 10.0, false));
        // Activity at 8 pushes the deadline to 18.
        assert!(!t.tick(8.0, 10.0, true));
        assert!(!t.tick(15.0, 10.0, false));
        assert!(t.tick(18.0, 10.0, false));
        assert!(!t.armed());
        assert!(!t.tick(30.0, 10.0, false), "fires once");
        t.start(40.0, 10.0);
        t.cancel();
        assert!(!t.tick(100.0, 10.0, false));
    }

    #[test]
    fn hover_dwell_and_leave() {
        let mut h = Hover::default();
        assert_eq!(h.tick(0.0, HoverInput { over_empty_bar: true, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6), HoverAction::None);
        assert_eq!(h.tick(0.2, HoverInput { over_empty_bar: true, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6), HoverAction::None);
        assert_eq!(h.tick(0.31, HoverInput { over_empty_bar: true, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6), HoverAction::Show);
        assert!(h.shown_by_hover);
        // In the menu bar: stays.
        assert_eq!(h.tick(1.0, HoverInput { over_empty_bar: false, in_menu_bar: true, shown: true, menu_open: false }, 0.3, 0.6), HoverAction::None);
        // Away, but a menu is open: stays.
        assert_eq!(h.tick(2.0, HoverInput { over_empty_bar: false, in_menu_bar: false, shown: true, menu_open: true }, 0.3, 0.6), HoverAction::None);
        assert_eq!(h.tick(2.1, HoverInput { over_empty_bar: false, in_menu_bar: false, shown: true, menu_open: false }, 0.3, 0.6), HoverAction::None);
        assert_eq!(h.tick(2.8, HoverInput { over_empty_bar: false, in_menu_bar: false, shown: true, menu_open: false }, 0.3, 0.6), HoverAction::Hide);
        // Leaving the empty area resets the dwell.
        let mut h = Hover::default();
        h.tick(0.0, HoverInput { over_empty_bar: true, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6);
        h.tick(0.2, HoverInput { over_empty_bar: false, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6);
        assert_eq!(h.tick(0.4, HoverInput { over_empty_bar: true, in_menu_bar: true, shown: false, menu_open: false }, 0.3, 0.6), HoverAction::None);
        // Shown by a click (not hover): hover never hides it.
        let mut h = Hover::default();
        assert_eq!(h.tick(0.0, HoverInput { over_empty_bar: false, in_menu_bar: false, shown: true, menu_open: false }, 0.3, 0.6), HoverAction::None);
        assert_eq!(h.tick(5.0, HoverInput { over_empty_bar: false, in_menu_bar: false, shown: true, menu_open: false }, 0.3, 0.6), HoverAction::None);
    }

    #[test]
    fn fitted_lengths() {
        let screen = Rect::new(0.0, 0.0, 1512.0, 982.0);
        let notch = Rect::new(663.5, 0.0, 185.0, 32.0);
        // Measured on macOS 27: separator ending at 1000 spans to just right
        // of the notch.
        let l = fitted_length(1000.0, &screen, Some(&notch), None, 16.0);
        assert!((l - (1000.0 - 852.5 - 16.0)).abs() < 1e-9);
        // No room right of the notch: cross it.
        assert!(fitted_length(870.0, &screen, Some(&notch), None, 16.0) >= 60.0);
        // No notch: up to the app menus.
        assert_eq!(fitted_length(1000.0, &screen, None, Some(400.0), 16.0), 1000.0 - 412.0 - 16.0);
        // Nothing known: half the space.
        assert_eq!(fitted_length(1000.0, &screen, None, None, 16.0), 484.0);
        // A notch to the right of the separator (other screen layout) is ignored.
        assert_eq!(fitted_length(600.0, &screen, Some(&notch), Some(200.0), 16.0), 600.0 - 212.0 - 16.0);
    }

    #[test]
    fn animation_and_drag() {
        let s = animation_steps(400.0, 12.0, 8);
        assert_eq!(s.len(), 8);
        assert_eq!(*s.last().unwrap(), 12.0);
        assert!(s.windows(2).all(|w| w[1] < w[0]));
        // Ease-out: the first step covers more than 1/8 of the distance.
        assert!(400.0 - s[0] > (400.0 - 12.0) / 8.0);
        let item = Rect::new(1000.0, 4.0, 40.0, 24.0);
        let target = Rect::new(800.0, 0.0, 16.0, 33.0);
        let p = drag_path(&item, &target, DropSide::LeftOf, 10);
        assert_eq!(p.len(), 11);
        assert_eq!(p[0], (1020.0, 16.0));
        assert_eq!(p[10], (801.0, 16.5));
        let p = drag_path(&item, &target, DropSide::RightOf, 1);
        assert_eq!(p[1].0, 815.0);
        assert!(move_succeeded(&Rect::new(700.0, 0.0, 40.0, 24.0), &target, DropSide::LeftOf));
        assert!(!move_succeeded(&Rect::new(900.0, 0.0, 40.0, 24.0), &target, DropSide::LeftOf));
        assert!(move_succeeded(&Rect::new(900.0, 0.0, 40.0, 24.0), &target, DropSide::RightOf));
    }

    #[test]
    fn ids() {
        assert_eq!(item_id("Mos", Some("com.caldis.Mos"), None, 0), "com.caldis.Mos#0");
        assert_eq!(item_id("Mos", None, Some("Wi-Fi"), 2), "Mos/Wi-Fi");
        assert_eq!(item_id("Mos", Some(""), Some(""), 1), "Mos#1");
    }
}
