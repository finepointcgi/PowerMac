//! Animation helpers: a damped spring integrator and a few easing curves.
//! Pure and deterministic so they can be unit tested.

/// A damped harmonic oscillator `x'' = -k (x - target) - c x'`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
    pub value: f64,
    pub velocity: f64,
    pub target: f64,
}

/// Largest integration step; longer frames are split so the spring stays
/// stable (and identical) at 30, 60 or 120 Hz.
const MAX_STEP: f64 = 1.0 / 240.0;

impl Spring {
    pub fn new(stiffness: f64, damping: f64, value: f64) -> Self {
        Spring { stiffness, damping, value, velocity: 0.0, target: value }
    }

    /// Damping for a damping ratio `zeta` (1.0 = critically damped).
    pub fn damping_for_ratio(stiffness: f64, zeta: f64) -> f64 {
        2.0 * zeta * stiffness.sqrt()
    }

    /// Advance by `dt` seconds (semi-implicit Euler, sub-stepped).
    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.25);
        let n = (dt / MAX_STEP).ceil().max(1.0) as usize;
        let h = dt / n as f64;
        for _ in 0..n {
            let a = -self.stiffness * (self.value - self.target) - self.damping * self.velocity;
            self.velocity += a * h;
            self.value += self.velocity * h;
        }
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 5e-4 && self.velocity.abs() < 5e-3
    }
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Hermite smoothstep of `x` between `e0` and `e1`, clamped to [0, 1].
pub fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 == e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Frame-rate independent exponential approach of `cur` towards `target`
/// with time constant `tau` seconds.
pub fn approach(cur: f64, target: f64, dt: f64, tau: f64) -> f64 {
    if tau <= 0.0 {
        return target;
    }
    let next = cur + (target - cur) * (1.0 - (-dt / tau).exp());
    if (next - target).abs() < 1e-3 {
        target
    } else {
        next
    }
}

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
