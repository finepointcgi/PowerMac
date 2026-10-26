//! Pure helpers ported from `FancyZones.cpp` (monitor rotation, quick
//! layout cycling) and `MonitorUtils.cpp` (open window on active monitor).

use crate::geometry::Rect;
use crate::layout::guid_eq;

/// `MonitorRotation::GetRotatedMonitorIndex`.
pub fn rotated_monitor_index(source: usize, count: usize, reverse: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if reverse {
        if source == 0 {
            count - 1
        } else {
            source - 1
        }
    } else {
        (source + 1) % count
    }
}

/// `MulDiv` with rounding like Win32 (round half away from zero).
fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    if c == 0 {
        return -1;
    }
    let num = a as i64 * b as i64;
    let c = c as i64;
    let q = (num.abs() * 2 + c.abs()) / (c.abs() * 2);
    (if (num < 0) != (c < 0) { -q } else { q }) as i32
}

fn scale(v: i32, src_start: i32, src_size: i32, dst_start: i32, dst_size: i32) -> i32 {
    if src_size == 0 {
        return dst_start;
    }
    dst_start + mul_div(v - src_start, dst_size, src_size)
}

/// `MonitorRotation::MapRectBetweenMonitorWorkAreas`: proportional mapping.
pub fn map_rect_between_work_areas(r: Rect, src: Rect, dst: Rect) -> Rect {
    Rect::new(
        scale(r.left, src.left, src.width(), dst.left, dst.width()),
        scale(r.top, src.top, src.height(), dst.top, dst.height()),
        scale(r.right, src.left, src.width(), dst.left, dst.width()),
        scale(r.bottom, src.top, src.height(), dst.top, dst.height()),
    )
}

/// `CUSTOM_POSITIONING_LEFT_TOP_PADDING`.
pub const OPEN_ON_ACTIVE_PADDING: i32 = 16;

/// `MonitorUtils::FitOnScreen`: keep the window's offset/size, clamping into
/// the destination work area.
pub fn fit_on_screen(window: Rect, origin: Rect, dest: Rect) -> Rect {
    let mut left = dest.left + (window.left - origin.left);
    let mut top = dest.top + (window.top - origin.top);
    let mut w = window.width();
    let mut h = window.height();
    if left < dest.left || left + w > dest.right {
        left = dest.left + OPEN_ON_ACTIVE_PADDING;
        w = w.min(dest.width() - OPEN_ON_ACTIVE_PADDING);
    }
    if top < dest.top || top + h > dest.bottom {
        top = dest.top + OPEN_ON_ACTIVE_PADDING;
        h = h.min(dest.height() - OPEN_ON_ACTIVE_PADDING);
    }
    Rect::from_xywh(left, top, w, h)
}

/// `FancyZonesUtils::PickNextLayoutId`: the id after (or before) `current`
/// in `ids`; starts at the beginning (end) when `current` is not in the list.
pub fn pick_next_layout_id<'a>(ids: &'a [String], current: Option<&str>, reverse: bool) -> Option<&'a str> {
    let n = ids.len();
    if n < 2 {
        return None;
    }
    let mut next = if reverse { n - 1 } else { 0 };
    if let Some(cur) = current {
        if let Some(i) = ids.iter().position(|id| guid_eq(id, cur)) {
            next = (i + if reverse { n - 1 } else { 1 }) % n;
        }
    }
    Some(ids[next].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_index() {
        assert_eq!(rotated_monitor_index(0, 3, false), 1);
        assert_eq!(rotated_monitor_index(2, 3, false), 0);
        assert_eq!(rotated_monitor_index(0, 3, true), 2);
        assert_eq!(rotated_monitor_index(1, 3, true), 0);
        assert_eq!(rotated_monitor_index(0, 0, true), 0);
    }

    #[test]
    fn rect_mapping() {
        let src = Rect::new(0, 0, 1000, 800);
        let dst = Rect::new(1000, 0, 3000, 1600);
        assert_eq!(map_rect_between_work_areas(Rect::new(0, 0, 500, 400), src, dst), Rect::new(1000, 0, 2000, 800));
        assert_eq!(map_rect_between_work_areas(Rect::new(250, 200, 750, 600), src, dst), Rect::new(1500, 400, 2500, 1200));
        // rounding like MulDiv
        assert_eq!(map_rect_between_work_areas(Rect::new(1, 1, 3, 3), Rect::new(0, 0, 3, 3), Rect::new(0, 0, 2, 2)), Rect::new(1, 1, 2, 2));
        assert_eq!(mul_div(-3, 1, 2), -2);
    }

    #[test]
    fn fit() {
        let origin = Rect::new(0, 0, 1000, 800);
        let dest = Rect::new(1000, 0, 1800, 600);
        // fits: same offset
        assert_eq!(fit_on_screen(Rect::from_xywh(100, 50, 300, 200), origin, dest), Rect::from_xywh(1100, 50, 300, 200));
        // too wide/tall: clamp to the padded origin and shrink
        assert_eq!(fit_on_screen(Rect::from_xywh(100, 100, 900, 700), origin, dest), Rect::from_xywh(1016, 16, 784, 584));
    }

    #[test]
    fn next_layout() {
        let ids: Vec<String> = ["{A}", "{B}", "{C}"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_next_layout_id(&ids, Some("{a}"), false), Some("{B}"));
        assert_eq!(pick_next_layout_id(&ids, Some("{C}"), false), Some("{A}"));
        assert_eq!(pick_next_layout_id(&ids, Some("{A}"), true), Some("{C}"));
        assert_eq!(pick_next_layout_id(&ids, Some("{X}"), false), Some("{A}"));
        assert_eq!(pick_next_layout_id(&ids, None, true), Some("{C}"));
        assert_eq!(pick_next_layout_id(&ids[..1], None, true), None);
    }
}
