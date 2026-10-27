//! Shake detection: a few quick left-right reversals of the pointer.
//!
//! Adapted from Mouse Utilities' Find My Mouse detector (which compares the
//! distance travelled with the box the pointer moved in): here the pointer
//! position is followed horizontally with hysteresis, so jitter smaller than
//! `min_segment_px` never counts as a change of direction. A shake is
//!
//! * at least `reversals` changes of horizontal direction, each after a
//!   stroke of at least `min_segment_px`, all within the last `window_ms`;
//! * those strokes add up to at least `min_distance_px`;
//! * the movement in the window is mostly horizontal (vertical travel at
//!   most half the horizontal travel), so circles and scribbles don't count;
//! * and, as in Find My Mouse, the path is much longer than the box it
//!   stays in (travelled ≥ 2 × the box's width), so a slow back-and-forth
//!   across the screen isn't one.
//!
//! The detector is pure (time is passed in, in seconds) and fed with
//! absolute pointer positions from the input hook.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// How sensitive shake detection is (`custom` uses
/// [`Config::custom_shake`](crate::Config::custom_shake)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sensitivity {
    /// A vigorous shake: more reversals over a longer distance.
    Low,
    #[default]
    Medium,
    /// A small wiggle is enough.
    High,
    Custom,
}

/// The thresholds behind a [`Sensitivity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShakeThresholds {
    /// Changes of horizontal direction needed (1–10).
    pub reversals: u32,
    /// They must all happen within this time (100–3000 ms).
    pub window_ms: u32,
    /// A stroke shorter than this isn't a change of direction (points).
    pub min_segment_px: u32,
    /// The strokes must add up to at least this (points).
    pub min_distance_px: u32,
}

impl Default for ShakeThresholds {
    fn default() -> Self {
        ShakeThresholds { reversals: 3, window_ms: 650, min_segment_px: 35, min_distance_px: 240 }
    }
}

impl Sensitivity {
    /// The thresholds for this level (`custom` for [`Sensitivity::Custom`]).
    pub fn thresholds(self, custom: &ShakeThresholds) -> ShakeThresholds {
        match self {
            Sensitivity::Low => ShakeThresholds { reversals: 4, window_ms: 700, min_segment_px: 50, min_distance_px: 400 },
            Sensitivity::Medium => ShakeThresholds::default(),
            Sensitivity::High => ShakeThresholds { reversals: 2, window_ms: 600, min_segment_px: 22, min_distance_px: 120 },
            Sensitivity::Custom => custom.clamped(),
        }
    }
}

impl ShakeThresholds {
    /// Values in their allowed ranges.
    pub fn clamped(&self) -> ShakeThresholds {
        ShakeThresholds {
            reversals: self.reversals.clamp(1, 10),
            window_ms: self.window_ms.clamp(100, 3000),
            min_segment_px: self.min_segment_px.clamp(4, 400),
            min_distance_px: self.min_distance_px.clamp(10, 4000),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Stroke {
    /// When the stroke ended (the direction changed).
    at: f64,
    len: f64,
}

#[derive(Clone, Copy, Debug)]
struct Step {
    at: f64,
    x: f64,
    dx: f64,
    dy: f64,
}

/// Feeds on pointer positions; [`ShakeDetector::feed`] returns true once
/// per detected shake.
#[derive(Clone, Debug, Default)]
pub struct ShakeDetector {
    last: Option<(f64, f64)>,
    /// -1 / +1 once a first stroke is long enough, 0 before.
    dir: i8,
    /// Where the current stroke started and its furthest point so far.
    stroke_start: f64,
    extreme: f64,
    strokes: VecDeque<Stroke>,
    steps: VecDeque<Step>,
}

impl ShakeDetector {
    pub fn reset(&mut self) {
        *self = ShakeDetector::default();
    }

    /// Feed the pointer position `(x, y)` at time `t` (seconds). Returns true
    /// when this movement completes a shake (the detector then starts over).
    pub fn feed(&mut self, x: f64, y: f64, t: f64, th: &ShakeThresholds) -> bool {
        let th = th.clamped();
        let window = th.window_ms as f64 / 1000.0;
        let seg = th.min_segment_px as f64;
        let Some((lx, ly)) = self.last.replace((x, y)) else {
            self.stroke_start = x;
            self.extreme = x;
            return false;
        };
        let (dx, dy) = (x - lx, y - ly);
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        self.steps.push_back(Step { at: t, x, dx, dy });
        while self.steps.front().is_some_and(|s| t - s.at > window) {
            self.steps.pop_front();
        }
        let mut reversed = false;
        match self.dir {
            0 => {
                // No direction yet: `stroke_start` / `extreme` hold the
                // lowest / highest x seen; the first stroke starts at the
                // opposite end once it is long enough.
                let (lo, hi) = (self.stroke_start.min(x), self.extreme.max(x));
                if x - lo >= seg && x >= hi {
                    self.dir = 1;
                    (self.stroke_start, self.extreme) = (lo, x);
                } else if hi - x >= seg && x <= lo {
                    self.dir = -1;
                    (self.stroke_start, self.extreme) = (hi, x);
                } else {
                    (self.stroke_start, self.extreme) = (lo, hi);
                }
            }
            d => {
                let further = if d > 0 { x >= self.extreme } else { x <= self.extreme };
                if further {
                    self.extreme = x;
                } else if (self.extreme - x).abs() >= seg {
                    let len = (self.extreme - self.stroke_start).abs();
                    self.strokes.push_back(Stroke { at: t, len });
                    self.stroke_start = self.extreme;
                    self.extreme = x;
                    self.dir = -d;
                    reversed = true;
                }
            }
        }
        while self.strokes.front().is_some_and(|s| t - s.at > window) {
            self.strokes.pop_front();
        }
        if !reversed || (self.strokes.len() as u32) < th.reversals {
            return false;
        }
        // The last `reversals` strokes, plus the stroke in progress.
        let n = th.reversals as usize;
        let recent: Vec<&Stroke> = self.strokes.iter().rev().take(n).collect();
        let distance: f64 = recent.iter().map(|s| s.len).sum::<f64>() + (self.extreme - self.stroke_start).abs();
        if distance < th.min_distance_px as f64 {
            return false;
        }
        let (h, v) = self.steps.iter().fold((0.0, 0.0), |(h, v), s| (h + s.dx.abs(), v + s.dy.abs()));
        if v > 0.5 * h {
            return false;
        }
        let (min_x, max_x) = self.steps.iter().fold((f64::MAX, f64::MIN), |(a, b), s| (a.min(s.x), b.max(s.x)));
        let width = (max_x - min_x).max(1.0);
        if h < 2.0 * width {
            return false;
        }
        self.reset();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Move from `from` to `to` in `steps` events over `secs`, starting at
    /// `t0`, with `wobble` vertical noise; returns the end time.
    #[allow(clippy::too_many_arguments)]
    fn sweep(d: &mut ShakeDetector, th: &ShakeThresholds, from: f64, to: f64, y: f64, t0: f64, secs: f64, steps: usize, wobble: f64, hits: &mut usize) -> f64 {
        for i in 1..=steps {
            let k = i as f64 / steps as f64;
            let x = from + (to - from) * k;
            let yy = y + if i % 2 == 0 { wobble } else { -wobble };
            if d.feed(x, yy, t0 + secs * k, th) {
                *hits += 1;
            }
        }
        t0 + secs
    }

    /// Back and forth between `a` and `b`, `strokes` times, `secs` each.
    fn shake(th: &ShakeThresholds, a: f64, b: f64, strokes: usize, secs: f64) -> usize {
        let mut d = ShakeDetector::default();
        let mut hits = 0;
        d.feed(a, 300.0, 0.0, th);
        let mut t = 0.0;
        for i in 0..strokes {
            let (from, to) = if i % 2 == 0 { (a, b) } else { (b, a) };
            t = sweep(&mut d, th, from, to, 300.0, t, secs, 8, 1.0, &mut hits);
        }
        hits
    }

    #[test]
    fn a_quick_shake_is_detected_once() {
        let th = Sensitivity::Medium.thresholds(&ShakeThresholds::default());
        // 4 strokes of 120 pt in 80 ms each: 3 reversals, 480 pt.
        assert_eq!(shake(&th, 500.0, 620.0, 4, 0.08), 1);
        // Keeps going: the detector starts over and can fire again.
        assert!(shake(&th, 500.0, 620.0, 9, 0.08) >= 2);
    }

    #[test]
    fn near_misses_are_not_shakes() {
        let th = Sensitivity::Medium.thresholds(&ShakeThresholds::default());
        // One reversal too few.
        assert_eq!(shake(&th, 500.0, 620.0, 3, 0.08), 0);
        // Strokes too short (jitter): never a change of direction.
        assert_eq!(shake(&th, 500.0, 530.0, 12, 0.03), 0);
        // Too slow: the reversals spread over more than the window.
        assert_eq!(shake(&th, 500.0, 620.0, 6, 0.4), 0);
        // Fast but too little ground covered (3 × 40 + 40 < 240).
        assert_eq!(shake(&th, 500.0, 540.0, 4, 0.05), 0);
        // A long straight drag.
        let mut d = ShakeDetector::default();
        let mut hits = 0;
        sweep(&mut d, &th, 0.0, 1500.0, 300.0, 0.0, 0.5, 60, 2.0, &mut hits);
        assert_eq!(hits, 0);
    }

    #[test]
    fn vertical_and_circular_movement_is_not_a_shake() {
        let th = Sensitivity::High.thresholds(&ShakeThresholds::default());
        // Up and down: no horizontal reversals at all.
        let mut d = ShakeDetector::default();
        let mut hits = 0;
        for i in 0..40 {
            let y = if (i / 5) % 2 == 0 { 300.0 + (i % 5) as f64 * 40.0 } else { 500.0 - (i % 5) as f64 * 40.0 };
            hits += d.feed(400.0 + (i % 2) as f64, y, i as f64 * 0.01, &th) as usize;
        }
        assert_eq!(hits, 0);
        // Fast circles of radius 80: horizontal reversals, but as much
        // vertical movement.
        let mut d = ShakeDetector::default();
        let mut hits = 0;
        for i in 0..120 {
            let a = i as f64 * std::f64::consts::TAU / 24.0;
            hits += d.feed(400.0 + 80.0 * a.cos(), 400.0 + 80.0 * a.sin(), i as f64 * 0.005, &th) as usize;
        }
        assert_eq!(hits, 0);
    }

    #[test]
    fn sensitivity_levels_differ() {
        let c = ShakeThresholds::default();
        let (low, med, high) = (Sensitivity::Low.thresholds(&c), Sensitivity::Medium.thresholds(&c), Sensitivity::High.thresholds(&c));
        assert!(low.reversals > med.reversals && med.reversals > high.reversals);
        assert!(low.min_distance_px > med.min_distance_px && med.min_distance_px > high.min_distance_px);
        // A small wiggle: only High takes it.
        let wiggle = |th: &ShakeThresholds| shake(th, 500.0, 570.0, 3, 0.06);
        assert_eq!((wiggle(&low), wiggle(&med), wiggle(&high)), (0, 0, 1));
        // A big shake: all of them.
        let big = |th: &ShakeThresholds| shake(th, 400.0, 560.0, 6, 0.07);
        assert!(big(&low) >= 1 && big(&med) >= 1 && big(&high) >= 1);
        // Custom thresholds are clamped.
        let wild = ShakeThresholds { reversals: 0, window_ms: 10, min_segment_px: 0, min_distance_px: 999_999 };
        let t = Sensitivity::Custom.thresholds(&wild);
        assert_eq!((t.reversals, t.window_ms, t.min_segment_px, t.min_distance_px), (1, 100, 4, 4000));
        assert_eq!(ShakeThresholds::default(), med);
    }

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_value(Sensitivity::High).unwrap(), "high");
        let t: ShakeThresholds = serde_json::from_str(r#"{"reversals": 5}"#).unwrap();
        assert_eq!(t.reversals, 5);
        assert_eq!(t.window_ms, ShakeThresholds::default().window_ms);
        assert_eq!(serde_json::to_value(t).unwrap()["minSegmentPx"], 35);
    }
}
