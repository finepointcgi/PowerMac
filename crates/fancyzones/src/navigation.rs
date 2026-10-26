//! Keyboard snapping ("override Windows Snap") — a port of
//! `WindowKeyboardSnap.cpp` and `FancyZonesUtils::ChooseNextZoneByPosition`.
//!
//! Everything here is pure: it takes work areas, the window rect and its
//! current zone assignment and returns *where* the window should go.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::layout::Layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn vector(self) -> (f64, f64) {
        match self {
            Direction::Up => (0.0, -1.0),
            Direction::Down => (0.0, 1.0),
            Direction::Left => (-1.0, 0.0),
            Direction::Right => (1.0, 0.0),
        }
    }

    pub fn is_horizontal(self) -> bool {
        matches!(self, Direction::Left | Direction::Right)
    }
}

impl std::str::FromStr for Direction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "left" | "l" => Ok(Direction::Left),
            "right" | "r" => Ok(Direction::Right),
            "up" | "u" => Ok(Direction::Up),
            "down" | "d" => Ok(Direction::Down),
            o => Err(format!("unknown direction '{o}'")),
        }
    }
}

/// `FancyZonesUtils::ChooseNextZoneByPosition`: index of the zone whose
/// center lies best in `dir` from the window's center, using the same
/// elliptical distance estimate (eccentricity 2, max tan 10).
pub fn choose_next_zone_by_position(dir: Direction, window_rect: Rect, zone_rects: &[Rect]) -> Option<usize> {
    const INF: f64 = 1e100;
    const ECC: f64 = 2.0;
    let center = |r: &Rect| (0.5 * r.left as f64 + 0.5 * r.right as f64, 0.5 * r.top as f64 + 0.5 * r.bottom as f64);
    let (ax, ay) = dir.vector();
    let distance = |zx: f64, zy: f64| -> f64 {
        // Re(arrow * conj(zone))
        let scalar = ax * zx + ay * zy;
        if scalar <= 0.0 {
            return INF;
        }
        let cos = scalar / (zx * zx + zy * zy).sqrt();
        let tan = cos.acos().tan().abs();
        if tan > 10.0 {
            return INF;
        }
        let intersect_y = 2.0 * ECC / (1.0 + ECC * ECC * tan * tan);
        let d = scalar / intersect_y;
        if d.is_finite() {
            d
        } else {
            INF
        }
    };
    let (wx, wy) = center(&window_rect);
    let mut best = None;
    let mut smallest = INF;
    for (i, z) in zone_rects.iter().enumerate() {
        let (mut cx, cy) = center(z);
        // `center += 0.001 * (i + 1)` adds to the real part only.
        cx += 0.001 * (i as f64 + 1.0);
        let d = distance(cx - wx, cy - wy);
        if d < smallest {
            smallest = d;
            best = Some(i);
        }
    }
    best
}

/// `FancyZonesUtils::PrepareRectForCycling`: move the window rect one work
/// area "behind" the arrow direction so the search wraps around.
pub fn prepare_rect_for_cycling(window: Rect, area: Rect, dir: Direction) -> Rect {
    let (dx, dy) = match dir {
        Direction::Up => (0, area.bottom - area.top),
        Direction::Down => (0, area.top - area.bottom),
        Direction::Left => (area.right - area.left, 0),
        Direction::Right => (area.left - area.right, 0),
    };
    window.offset(dx, dy)
}

/// `MonitorUtils::OrderMonitors`: a topological order where a monitor comes
/// before monitors it is above/left of, ties broken by (top, left).
pub fn order_monitors(rects: &[Rect]) -> Vec<usize> {
    let n = rects.len();
    let mut blocking = vec![vec![false; n]; n];
    let mut count = vec![0usize; n];
    for i in 0..n {
        for j in 0..n {
            let (a, b) = (rects[i], rects[j]);
            blocking[i][j] = a.top < b.bottom && a.left < b.right && i != j;
            if blocking[i][j] {
                count[j] += 1;
            }
        }
    }
    let mut used = vec![false; n];
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut cands: Vec<usize> = (0..n).filter(|&i| count[i] == 0 && !used[i]).collect();
        if cands.is_empty() {
            cands = (0..n).filter(|&i| !used[i]).collect();
        }
        let smallest = *cands
            .iter()
            .min_by_key(|&&i| (rects[i].top, rects[i].left))
            .expect("non-empty");
        used[smallest] = true;
        out.push(smallest);
        for j in 0..n {
            if blocking[smallest][j] {
                count[j] -= 1;
            }
        }
    }
    out
}

/// A planned snap: work area index + zone set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapTarget {
    pub work_area: usize,
    pub zones: Vec<usize>,
}

/// `MoveByDirectionAndIndex` on one work area. `assigned` are the zones the
/// window currently occupies *in this work area* (empty if none).
pub fn move_by_index(layout: &Layout, assigned: &[usize], dir: Direction, cycle: bool) -> Option<Vec<usize>> {
    let n = layout.zones.len();
    if n == 0 || !dir.is_horizontal() {
        return None;
    }
    let left = dir == Direction::Left;
    let Some(&old) = assigned.first() else {
        return Some(vec![if left { n - 1 } else { 0 }]);
    };
    if (left && old == 0) || (!left && old >= n - 1) {
        if !cycle {
            return None;
        }
        return Some(vec![if left { n - 1 } else { 0 }]);
    }
    Some(vec![if left { old - 1 } else { old + 1 }])
}

/// `SnapHotkeyBasedOnZoneNumber`: Left/Right cycle through zone indices;
/// with `across_monitors` continue on the next/previous monitor (in
/// `layouts` order) after the last/first zone.
pub fn snap_by_index(
    layouts: &[&Layout],
    current: usize,
    assigned: Option<&SnapTarget>,
    dir: Direction,
    across_monitors: bool,
) -> Option<SnapTarget> {
    if !dir.is_horizontal() || current >= layouts.len() {
        return None;
    }
    let assigned_on = |wa: usize| -> &[usize] {
        match assigned {
            Some(t) if t.work_area == wa => &t.zones,
            _ => &[],
        }
    };
    if across_monitors && layouts.len() > 1 {
        let n = layouts.len();
        let mut m = current;
        loop {
            if let Some(z) = move_by_index(layouts[m], assigned_on(m), dir, false) {
                return Some(SnapTarget { work_area: m, zones: z });
            }
            m = if dir == Direction::Right { (m + 1) % n } else { (m + n - 1) % n };
            if m == current {
                return None;
            }
        }
    }
    move_by_index(layouts[current], assigned_on(current), dir, true).map(|z| SnapTarget { work_area: current, zones: z })
}

/// `MoveByDirectionAndPosition` on one work area.
pub fn move_by_position(layout: &Layout, window_rect: Rect, assigned: &[usize], dir: Direction, cycle: bool) -> Option<Vec<usize>> {
    if layout.zones.is_empty() {
        return None;
    }
    let free: Vec<usize> = (0..layout.zones.len()).filter(|i| !assigned.contains(i)).collect();
    let rects: Vec<Rect> = free.iter().map(|&i| layout.zones[i]).collect();
    if let Some(i) = choose_next_zone_by_position(dir, window_rect, &rects) {
        return Some(vec![free[i]]);
    }
    if cycle {
        let r = prepare_rect_for_cycling(window_rect, layout.work_area, dir);
        return choose_next_zone_by_position(dir, r, &layout.zones).map(|i| vec![i]);
    }
    None
}

/// `WindowKeyboardSnap::Snap` (position based, all four directions).
pub fn snap_by_position(
    layouts: &[&Layout],
    current: usize,
    window_rect: Rect,
    assigned: Option<&SnapTarget>,
    dir: Direction,
    across_monitors: bool,
) -> Option<SnapTarget> {
    if current >= layouts.len() {
        return None;
    }
    let assigned_here: &[usize] = match assigned {
        Some(t) if t.work_area == current => &t.zones,
        _ => &[],
    };
    if !(across_monitors && layouts.len() > 1) {
        return move_by_position(layouts[current], window_rect, assigned_here, dir, true)
            .map(|z| SnapTarget { work_area: current, zones: z });
    }
    if let Some(z) = move_by_position(layouts[current], window_rect, assigned_here, dir, false) {
        return Some(SnapTarget { work_area: current, zones: z });
    }
    // Zones of all other monitors.
    let mut rects = Vec::new();
    let mut info = Vec::new();
    for (wa, l) in layouts.iter().enumerate() {
        if wa == current {
            continue;
        }
        for (zi, r) in l.zones.iter().enumerate() {
            rects.push(*r);
            info.push((wa, zi));
        }
    }
    if let Some(i) = choose_next_zone_by_position(dir, window_rect, &rects) {
        let (wa, zi) = info[i];
        return Some(SnapTarget { work_area: wa, zones: vec![zi] });
    }
    // Wrap around over the combined rect of all monitors.
    for (zi, r) in layouts[current].zones.iter().enumerate() {
        rects.push(*r);
        info.push((current, zi));
    }
    let combined = layouts.iter().map(|l| l.work_area).reduce(|a, b| a.union(&b))?;
    let r = prepare_rect_for_cycling(window_rect, combined, dir);
    choose_next_zone_by_position(dir, r, &rects).map(|i| {
        let (wa, zi) = info[i];
        SnapTarget { work_area: wa, zones: vec![zi] }
    })
}

/// State for keyboard zone extension (`m_extendData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendState {
    pub window: u64,
    pub initial: Vec<usize>,
    pub final_index: Option<usize>,
}

/// `WindowKeyboardSnap::Extend`: grow the window's zone set towards `dir`.
/// `state` must be reset (set to `None`) whenever a normal snap happens.
pub fn extend(
    layout: &Layout,
    window: u64,
    window_rect: Rect,
    applied: &[usize],
    state: &mut Option<ExtendState>,
    dir: Direction,
) -> Option<Vec<usize>> {
    let n = layout.zones.len();
    if n == 0 {
        return None;
    }
    let mut used = vec![false; n];
    let mut rect = window_rect;
    let extended = matches!(state, Some(s) if s.window == window && s.final_index.is_some());
    if extended {
        let f = state.as_ref().unwrap().final_index.unwrap().min(n - 1);
        used[f] = true;
        rect = layout.zones[f];
    } else {
        for &i in applied {
            if i < n {
                used[i] = true;
            }
        }
        *state = Some(ExtendState { window, initial: vec![], final_index: None });
    }
    let free: Vec<usize> = (0..n).filter(|&i| !used[i]).collect();
    let rects: Vec<Rect> = free.iter().map(|&i| layout.zones[i]).collect();
    let target = free[choose_next_zone_by_position(dir, rect, &rects)?];
    let st = state.as_mut().unwrap();
    let result = if st.final_index.is_none() {
        if !applied.is_empty() {
            st.initial = applied.to_vec();
            st.final_index = Some(target);
            layout.combined_zone_range(applied, &[target])
        } else {
            st.initial = vec![target];
            st.final_index = Some(target);
            vec![target]
        }
    } else {
        st.final_index = Some(target);
        layout.combined_zone_range(&st.initial, &[target])
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{AppliedLayout, LayoutType};

    fn layout(kind: LayoutType, n: i32, wa: Rect) -> Layout {
        Layout::new(&AppliedLayout::template(kind, n).with_spacing(false, 0), wa, &[]).unwrap()
    }

    #[test]
    fn choose_by_position_basic() {
        // (the window's own zone is excluded by callers, like FancyZones does)
        let zones = [Rect::new(100, 0, 200, 100), Rect::new(0, 100, 100, 200)];
        let w = Rect::new(0, 0, 100, 100);
        assert_eq!(choose_next_zone_by_position(Direction::Right, w, &zones), Some(0));
        assert_eq!(choose_next_zone_by_position(Direction::Down, w, &zones), Some(1));
        assert_eq!(choose_next_zone_by_position(Direction::Left, w, &zones), None);
        assert_eq!(choose_next_zone_by_position(Direction::Up, w, &zones), None);
        // diagonal zone is not chosen when a straight one exists
        let zones = [Rect::new(100, 100, 200, 200), Rect::new(300, 0, 400, 100)];
        assert_eq!(choose_next_zone_by_position(Direction::Right, w, &zones), Some(1));
        assert_eq!(choose_next_zone_by_position(Direction::Right, w, &[]), None);
    }

    #[test]
    fn cycling_rect() {
        let wa = Rect::new(0, 0, 1000, 500);
        let w = Rect::new(800, 0, 1000, 100);
        assert_eq!(prepare_rect_for_cycling(w, wa, Direction::Right), Rect::new(-200, 0, 0, 100));
        assert_eq!(prepare_rect_for_cycling(w, wa, Direction::Up), Rect::new(800, 500, 1000, 600));
    }

    #[test]
    fn index_navigation_single_monitor() {
        let l = layout(LayoutType::Columns, 3, Rect::new(0, 0, 1200, 800));
        let ls = [&l];
        let t = |z: Vec<usize>| SnapTarget { work_area: 0, zones: z };
        assert_eq!(snap_by_index(&ls, 0, None, Direction::Right, false), Some(t(vec![0])));
        assert_eq!(snap_by_index(&ls, 0, None, Direction::Left, false), Some(t(vec![2])));
        assert_eq!(snap_by_index(&ls, 0, Some(&t(vec![0])), Direction::Right, false), Some(t(vec![1])));
        assert_eq!(snap_by_index(&ls, 0, Some(&t(vec![2])), Direction::Right, false), Some(t(vec![0])));
        assert_eq!(snap_by_index(&ls, 0, Some(&t(vec![0])), Direction::Left, false), Some(t(vec![2])));
        assert_eq!(snap_by_index(&ls, 0, None, Direction::Up, false), None);
    }

    #[test]
    fn index_navigation_across_monitors() {
        let a = layout(LayoutType::Columns, 2, Rect::new(0, 0, 1000, 800));
        let b = layout(LayoutType::Columns, 2, Rect::new(1000, 0, 2000, 800));
        let ls = [&a, &b];
        let at = |wa, z| SnapTarget { work_area: wa, zones: vec![z] };
        // last zone of A -> first zone of B
        assert_eq!(snap_by_index(&ls, 0, Some(&at(0, 1)), Direction::Right, true), Some(at(1, 0)));
        // last zone of B -> wraps to A's first
        assert_eq!(snap_by_index(&ls, 1, Some(&at(1, 1)), Direction::Right, true), Some(at(0, 0)));
        // first zone of A going left -> B's last
        assert_eq!(snap_by_index(&ls, 0, Some(&at(0, 0)), Direction::Left, true), Some(at(1, 1)));
        // without crossing: cycles on the same monitor
        assert_eq!(snap_by_index(&ls, 0, Some(&at(0, 1)), Direction::Right, false), Some(at(0, 0)));
        // blank monitor is skipped
        let blank = Layout::blank(Rect::new(2000, 0, 3000, 800));
        let ls = [&a, &blank, &b];
        assert_eq!(snap_by_index(&ls, 0, Some(&at(0, 1)), Direction::Right, true), Some(at(2, 0)));
    }

    #[test]
    fn position_navigation_grid() {
        // 2x2 grid: 0 1 / 2 3
        let l = layout(LayoutType::Grid, 4, Rect::new(0, 0, 1000, 1000));
        let ls = [&l];
        let at = |z| SnapTarget { work_area: 0, zones: vec![z] };
        let in0 = l.zones[0];
        assert_eq!(snap_by_position(&ls, 0, in0, Some(&at(0)), Direction::Right, false), Some(at(1)));
        assert_eq!(snap_by_position(&ls, 0, in0, Some(&at(0)), Direction::Down, false), Some(at(2)));
        // cycling: left from zone 0 wraps to zone 1 (rightmost in same row)
        assert_eq!(snap_by_position(&ls, 0, in0, Some(&at(0)), Direction::Left, false), Some(at(1)));
        assert_eq!(snap_by_position(&ls, 0, in0, Some(&at(0)), Direction::Up, false), Some(at(2)));
        // unsnapped window: zone 0's center is to the right of the window's center
        let w = Rect::new(0, 100, 300, 400);
        assert_eq!(snap_by_position(&ls, 0, w, None, Direction::Right, false), Some(at(0)));
        let w = Rect::new(400, 100, 600, 400);
        assert_eq!(snap_by_position(&ls, 0, w, None, Direction::Right, false), Some(at(1)));
    }

    #[test]
    fn position_navigation_across_monitors() {
        let a = layout(LayoutType::Columns, 2, Rect::new(0, 0, 1000, 800));
        let b = layout(LayoutType::Columns, 2, Rect::new(1000, 0, 2000, 800));
        let ls = [&a, &b];
        let at = |wa, z| SnapTarget { work_area: wa, zones: vec![z] };
        assert_eq!(snap_by_position(&ls, 0, a.zones[1], Some(&at(0, 1)), Direction::Right, true), Some(at(1, 0)));
        assert_eq!(snap_by_position(&ls, 1, b.zones[0], Some(&at(1, 0)), Direction::Left, true), Some(at(0, 1)));
        // rightmost zone of the rightmost monitor wraps to the leftmost zone overall
        assert_eq!(snap_by_position(&ls, 1, b.zones[1], Some(&at(1, 1)), Direction::Right, true), Some(at(0, 0)));
        // monitor below
        let c = layout(LayoutType::Rows, 1, Rect::new(0, 800, 1000, 1600));
        let ls = [&a, &b, &c];
        assert_eq!(snap_by_position(&ls, 0, a.zones[0], Some(&at(0, 0)), Direction::Down, true), Some(at(2, 0)));
    }

    #[test]
    fn extend_spans_zones() {
        let l = layout(LayoutType::Columns, 4, Rect::new(0, 0, 1600, 800));
        let mut st = None;
        let r = extend(&l, 7, l.zones[0], &[0], &mut st, Direction::Right).unwrap();
        assert_eq!(r, vec![0, 1]);
        let r = extend(&l, 7, l.zones[0], &[0, 1], &mut st, Direction::Right).unwrap();
        assert_eq!(r, vec![0, 1, 2]);
        let r = extend(&l, 7, l.zones[0], &[0, 1, 2], &mut st, Direction::Left).unwrap();
        assert_eq!(r, vec![0, 1]);
        // unsnapped window: first extension just picks a zone
        let mut st = None;
        let r = extend(&l, 9, Rect::new(0, 300, 100, 500), &[], &mut st, Direction::Right).unwrap();
        assert_eq!(r, vec![0]);
        let r = extend(&l, 9, l.zones[0], &[0], &mut st, Direction::Right).unwrap();
        assert_eq!(r, vec![0, 1]);
    }

    #[test]
    fn monitor_order() {
        let rects = [Rect::new(1920, 0, 3840, 1080), Rect::new(0, 0, 1920, 1080), Rect::new(0, -1080, 1920, 0)];
        assert_eq!(order_monitors(&rects), vec![2, 1, 0]);
        let side = [Rect::new(1920, 0, 3840, 1080), Rect::new(0, 200, 1920, 1280)];
        assert_eq!(order_monitors(&side), vec![1, 0]);
    }
}
