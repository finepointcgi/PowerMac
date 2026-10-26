//! Basic integer geometry (Win32 `RECT` semantics: `right`/`bottom` are exclusive edges).
//!
//! All coordinates used by this crate are *global, top-left origin, y grows down*:
//! on Windows these are physical virtual-screen pixels, on macOS they are the
//! CoreGraphics / Accessibility global coordinate space (points, origin at the
//! top-left of the primary display).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    /// Construct from origin + size.
    pub const fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { left: x, top: y, right: x + w, bottom: y + h }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// Area clamped at 0 for inverted rects (like `Zone::GetZoneArea`).
    pub fn area(&self) -> i64 {
        (self.height().max(0) as i64) * (self.width().max(0) as i64)
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// Half-open containment test (`left <= x < right`).
    pub fn contains(&self, p: Point) -> bool {
        self.left <= p.x && p.x < self.right && self.top <= p.y && p.y < self.bottom
    }

    pub fn center(&self) -> Point {
        Point::new((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.left + dx, self.top + dy, self.right + dx, self.bottom + dy)
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(
            self.left.min(o.left),
            self.top.min(o.top),
            self.right.max(o.right),
            self.bottom.max(o.bottom),
        )
    }

    pub fn intersection(&self, o: &Rect) -> Option<Rect> {
        let r = Rect::new(
            self.left.max(o.left),
            self.top.max(o.top),
            self.right.min(o.right),
            self.bottom.min(o.bottom),
        );
        if r.is_empty() {
            None
        } else {
            Some(r)
        }
    }

    /// `true` when `o` lies completely inside `self` (inclusive edges).
    pub fn contains_rect(&self, o: &Rect) -> bool {
        self.left <= o.left && o.right <= self.right && self.top <= o.top && o.bottom <= self.bottom
    }

    /// Squared distance from `p` to the closest point of the rect (0 if inside).
    pub fn distance_sq(&self, p: Point) -> i64 {
        let dx = if p.x < self.left {
            self.left - p.x
        } else if p.x >= self.right {
            p.x - self.right + 1
        } else {
            0
        } as i64;
        let dy = if p.y < self.top {
            self.top - p.y
        } else if p.y >= self.bottom {
            p.y - self.bottom + 1
        } else {
            0
        } as i64;
        dx * dx + dy * dy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let r = Rect::from_xywh(10, 20, 100, 50);
        assert_eq!(r.width(), 100);
        assert_eq!(r.height(), 50);
        assert_eq!(r.area(), 5000);
        assert!(r.contains(Point::new(10, 20)));
        assert!(!r.contains(Point::new(110, 20)));
        assert_eq!(r.center(), Point::new(60, 45));
        assert_eq!(r.offset(5, 5), Rect::new(15, 25, 115, 75));
        let o = Rect::new(50, 0, 200, 30);
        assert_eq!(r.union(&o), Rect::new(10, 0, 200, 70));
        assert_eq!(r.intersection(&o), Some(Rect::new(50, 20, 110, 30)));
        assert_eq!(r.intersection(&Rect::new(500, 500, 600, 600)), None);
        assert_eq!(Rect::new(10, 10, 5, 5).area(), 0);
        assert_eq!(r.distance_sq(Point::new(15, 25)), 0);
        assert_eq!(r.distance_sq(Point::new(7, 16)), 9 + 16);
    }
}
