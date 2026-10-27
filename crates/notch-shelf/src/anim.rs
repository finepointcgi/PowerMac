//! Animation helpers: re-exported from `pm_shelf_core::anim` (shared with
//! the Drag Shelf).

#[allow(unused_imports)]
pub use pm_shelf_core::anim::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mut s: Spring, fps: f64, secs: f64) -> (Spring, f64) {
        let mut peak = s.value;
        let frames = (secs * fps) as usize;
        for _ in 0..frames {
            s.step(1.0 / fps);
            peak = peak.max(s.value);
        }
        (s, peak)
    }

    #[test]
    fn underdamped_spring_overshoots_then_settles() {
        let k = 380.0;
        let mut s = Spring::new(k, Spring::damping_for_ratio(k, 0.72), 0.0);
        s.target = 1.0;
        let (s, peak) = run(s, 60.0, 2.0);
        assert!(peak > 1.01 && peak < 1.1, "peak {peak}");
        assert!(s.settled());
        assert_eq!(s.value, 1.0);
    }

    #[test]
    fn critically_damped_spring_never_overshoots() {
        let k = 420.0;
        let mut s = Spring::new(k, Spring::damping_for_ratio(k, 1.0), 1.0);
        s.target = 0.0;
        let mut min = 1.0f64;
        for _ in 0..240 {
            s.step(1.0 / 120.0);
            min = min.min(s.value);
        }
        assert!(min >= -1e-6, "undershoot {min}");
        assert!(s.settled());
    }

    #[test]
    fn spring_is_frame_rate_independent() {
        let k = 300.0;
        let mk = || {
            let mut s = Spring::new(k, Spring::damping_for_ratio(k, 0.8), 0.0);
            s.target = 1.0;
            s
        };
        let (a, _) = run(mk(), 60.0, 0.15);
        let (b, _) = run(mk(), 120.0, 0.15);
        assert!((a.value - b.value).abs() < 0.01, "{} vs {}", a.value, b.value);
    }

    #[test]
    fn spring_settles_in_reasonable_time() {
        let k = 380.0;
        let mut s = Spring::new(k, Spring::damping_for_ratio(k, 0.72), 0.0);
        s.target = 1.0;
        let mut t = 0.0;
        while !s.settled() && t < 5.0 {
            s.step(1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert!(t < 1.0, "took {t}s");
    }

    #[test]
    fn easing_curves() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-9);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-9);
        assert_eq!(lerp(2.0, 4.0, 0.25), 2.5);
        let mut v = 0.0;
        for _ in 0..60 {
            v = approach(v, 1.0, 1.0 / 60.0, 0.05);
        }
        assert_eq!(v, 1.0);
        assert_eq!(approach(0.3, 1.0, 0.016, 0.0), 1.0);
    }
}
