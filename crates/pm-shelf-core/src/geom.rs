//! Geometry primitives shared by the shelves: points, rectangles and path
//! elements. Coordinates are global, top-left origin, y down, in points.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
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
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    pub fn inflate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x - dx, self.y - dy, self.w + 2.0 * dx, self.h + 2.0 * dy)
    }
    pub fn offset(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn lerp(&self, o: &Rect, t: f64) -> Rect {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Rect::new(l(self.x, o.x), l(self.y, o.y), l(self.w, o.w), l(self.h, o.h))
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x.min(o.x), self.y.min(o.y));
        Rect::new(x0, y0, self.right().max(o.right()) - x0, self.bottom().max(o.bottom()) - y0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEl {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!((r.right(), r.bottom(), r.center_x(), r.center_y()), (110.0, 70.0, 60.0, 45.0));
        assert!(r.contains(Point::new(10.0, 20.0)) && !r.contains(Point::new(110.0, 20.0)));
        assert_eq!(r.inflate(5.0, 5.0), Rect::new(5.0, 15.0, 110.0, 60.0));
        assert_eq!(r.offset(1.0, -1.0), Rect::new(11.0, 19.0, 100.0, 50.0));
        assert!(r.intersects(&Rect::new(100.0, 60.0, 50.0, 50.0)) && !r.intersects(&Rect::new(110.0, 0.0, 5.0, 5.0)));
        assert_eq!(r.lerp(&Rect::new(20.0, 20.0, 200.0, 50.0), 0.5), Rect::new(15.0, 20.0, 150.0, 50.0));
        assert_eq!(r.union(&Rect::new(0.0, 0.0, 5.0, 5.0)), Rect::new(0.0, 0.0, 110.0, 70.0));
    }
}
