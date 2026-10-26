//! Tiny software rasterizer for the Windows layered overlays: draws a
//! [`Scene`] into a premultiplied 32-bit BGRA buffer (what
//! `UpdateLayeredWindow` with `AC_SRC_ALPHA` expects). Anti-aliased circles,
//! axis-aligned rects.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::config::Color;
use crate::engine::{Point, RectF, Scene};

/// Premultiplied pixel `0xAARRGGBB` (little-endian BGRA in memory).
fn premul(c: Color, coverage: f64) -> u32 {
    let a = (c.a as f64 * coverage.clamp(0.0, 1.0)).round();
    if a <= 0.0 {
        return 0;
    }
    let m = |v: u8| ((v as f64 * a) / 255.0).round() as u32;
    (a as u32) << 24 | m(c.r) << 16 | m(c.g) << 8 | m(c.b)
}

/// `src` over `dst`, both premultiplied.
fn over(dst: u32, src: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    let ch = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let d = (dst >> shift) & 0xFF;
        (s + (d * inv + 127) / 255).min(255) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// Coverage of a pixel centered at distance `d` from a circle edge of radius `r`
/// (1 inside, 0 outside, linear across one pixel).
fn circle_coverage(d: f64, r: f64) -> f64 {
    (r - d + 0.5).clamp(0.0, 1.0)
}

pub struct Canvas<'a> {
    pub width: usize,
    pub height: usize,
    pub px: &'a mut [u32],
}

impl<'a> Canvas<'a> {
    pub fn new(width: usize, height: usize, px: &'a mut [u32]) -> Self {
        assert!(px.len() >= width * height);
        Self { width, height, px }
    }

    pub fn clear(&mut self) {
        self.px[..self.width * self.height].fill(0);
    }

    /// Fill everything with `color` except an anti-aliased circular hole.
    /// Overwrites (meant as the first layer).
    pub fn fill_backdrop(&mut self, color: Color, center: Point, radius: f64) {
        let full = premul(color, 1.0);
        let (w, h) = (self.width, self.height);
        for y in 0..h {
            let row = &mut self.px[y * w..(y + 1) * w];
            let py = y as f64 + 0.5;
            let dy = (py - center.y).abs();
            if dy > radius + 1.0 {
                row.fill(full);
                continue;
            }
            let half = (radius + 1.0).powi(2) - dy * dy;
            let half = if half > 0.0 { half.sqrt() } else { 0.0 };
            let x0 = ((center.x - half).floor().max(0.0) as usize).min(w);
            let x1 = ((center.x + half).ceil().max(0.0) as usize).min(w);
            row[..x0].fill(full);
            row[x1..].fill(full);
            for (x, p) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let d = Point::new(x as f64 + 0.5, py).dist(center);
                *p = premul(color, 1.0 - circle_coverage(d, radius));
            }
        }
    }

    pub fn fill_circle(&mut self, center: Point, radius: f64, color: Color) {
        if color.a == 0 || radius <= 0.0 {
            return;
        }
        let w = self.width as i64;
        let h = self.height as i64;
        let y0 = ((center.y - radius - 1.0).floor() as i64).clamp(0, h);
        let y1 = ((center.y + radius + 1.0).ceil() as i64).clamp(0, h);
        let x0 = ((center.x - radius - 1.0).floor() as i64).clamp(0, w);
        let x1 = ((center.x + radius + 1.0).ceil() as i64).clamp(0, w);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = Point::new(x as f64 + 0.5, y as f64 + 0.5).dist(center);
                let cov = circle_coverage(d, radius);
                if cov > 0.0 {
                    let i = (y * w + x) as usize;
                    self.px[i] = over(self.px[i], premul(color, cov));
                }
            }
        }
    }

    pub fn fill_rect(&mut self, r: RectF, color: Color) {
        if color.a == 0 {
            return;
        }
        let w = self.width as i64;
        let h = self.height as i64;
        let x0 = (r.x0.round() as i64).clamp(0, w) as usize;
        let x1 = (r.x1.round() as i64).clamp(0, w) as usize;
        let y0 = (r.y0.round() as i64).clamp(0, h) as usize;
        let y1 = (r.y1.round() as i64).clamp(0, h) as usize;
        let src = premul(color, 1.0);
        for y in y0..y1 {
            let row = &mut self.px[y * self.width + x0..y * self.width + x1];
            if src >> 24 == 255 {
                row.fill(src);
            } else {
                for p in row {
                    *p = over(*p, src);
                }
            }
        }
    }

    /// Draw `scene` for a screen whose top-left is `origin` (global coords)
    /// with `size` pixels and `scale` (logical → pixels).
    pub fn draw_scene(&mut self, scene: &Scene, origin: Point, scale: f64) {
        let local = |p: Point| Point::new(p.x - origin.x, p.y - origin.y);
        match &scene.spotlight {
            Some(s) => self.fill_backdrop(s.color, local(s.center), s.radius * scale),
            None => self.clear(),
        }
        for c in &scene.circles {
            self.fill_circle(local(c.center), c.radius * scale, c.color);
        }
        if let Some(x) = &scene.crosshairs {
            let bounds = RectF::new(origin.x, origin.y, origin.x + self.width as f64, origin.y + self.height as f64);
            let border = x.border_color.scaled_alpha(x.opacity);
            let color = x.color.scaled_alpha(x.opacity);
            for arm in x.arms(bounds, scale) {
                if x.border > 0.0 {
                    // Border ring only (so translucent lines don't double up).
                    let o = arm.outer.offset(-origin.x, -origin.y);
                    let i = arm.inner.offset(-origin.x, -origin.y);
                    self.fill_rect(RectF::new(o.x0, o.y0, o.x1, i.y0), border);
                    self.fill_rect(RectF::new(o.x0, i.y1, o.x1, o.y1), border);
                    self.fill_rect(RectF::new(o.x0, i.y0, i.x0, i.y1), border);
                    self.fill_rect(RectF::new(i.x1, i.y0, o.x1, i.y1), border);
                }
                self.fill_rect(arm.inner.offset(-origin.x, -origin.y), color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CircleShape, SpotlightShape};

    #[test]
    fn premultiply_and_blend() {
        assert_eq!(premul(Color::rgb(255, 0, 0), 1.0), 0xFFFF0000);
        assert_eq!(premul(Color::argb(128, 255, 255, 255), 1.0), 0x80808080);
        assert_eq!(premul(Color::rgb(0, 0, 0), 0.0), 0);
        assert_eq!(over(0xFF0000FF, 0x80800000), 0xFF80007F);
        assert_eq!(over(0x12345678, 0), 0x12345678);
    }

    #[test]
    fn backdrop_hole_and_circle() {
        let mut buf = vec![0u32; 100 * 100];
        let mut c = Canvas::new(100, 100, &mut buf);
        let scene = Scene {
            spotlight: Some(SpotlightShape { center: Point::new(50.0, 50.0), radius: 20.3, color: Color::argb(128, 0, 0, 0) }),
            circles: vec![CircleShape { center: Point::new(90.0, 10.0), radius: 5.0, color: Color::rgb(0, 255, 0) }],
            crosshairs: None,
        };
        c.draw_scene(&scene, Point::new(0.0, 0.0), 1.0);
        assert_eq!(buf[50 * 100 + 50], 0, "center of the hole is clear");
        assert_eq!(buf[0], 0x80000000, "corner dimmed");
        assert_eq!(buf[50 * 100 + 75] >> 24, 128, "outside the radius");
        let edge = buf[50 * 100 + 70] >> 24; // ~ on the edge
        assert!(edge > 0 && edge < 128, "{edge}");
        assert_eq!(buf[10 * 100 + 90], 0xFF00FF00, "circle drawn over the backdrop");

        // Origin offset (second monitor): circle in global coords.
        let mut buf2 = vec![0u32; 50 * 50];
        let mut c = Canvas::new(50, 50, &mut buf2);
        let s = Scene { circles: vec![CircleShape { center: Point::new(1010.0, 10.0), radius: 3.0, color: Color::rgb(1, 2, 3) }], ..Default::default() };
        c.draw_scene(&s, Point::new(1000.0, 0.0), 1.0);
        assert_eq!(buf2[10 * 50 + 10], 0xFF010203);
        assert_eq!(buf2[0], 0);
    }

    #[test]
    fn crosshairs_raster() {
        let mut buf = vec![0u32; 100 * 100];
        let mut c = Canvas::new(100, 100, &mut buf);
        let cfg = crate::config::CrosshairsConfig { opacity: 1.0, ..Default::default() };
        let mut e = crate::engine::Engine::new(crate::config::Config { crosshairs: cfg, ..Default::default() }, std::time::Duration::from_millis(500));
        e.toggle_crosshairs();
        let scene = e.frame(std::time::Instant::now(), Point::new(50.0, 50.0));
        c.draw_scene(&scene, Point::default(), 1.0);
        assert_eq!(buf[50 * 100 + 5], 0xFFFF0000, "line color");
        assert_eq!(buf[47 * 100 + 5], 0xFFFFFFFF, "border");
        assert_eq!(buf[40 * 100 + 5], 0, "away from the line");
        assert_eq!(buf[50 * 100 + 50], 0, "center gap");
    }
}
