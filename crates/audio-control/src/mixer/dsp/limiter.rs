//! A stereo-linked look-ahead brick-wall limiter.
//!
//! Per frame the gain needed to keep the louder channel at the ceiling is
//! `r = min(1, ceiling / |x|)`. The applied gain is
//!
//! 1. the minimum of `r` over the last `L` frames (a sliding-window minimum,
//!    so a peak is "seen" `L − 1` frames before it leaves the delay line),
//! 2. smoothed upward by a one-pole release (downward moves are instant),
//! 3. averaged over the last `L` frames (a boxcar, so the gain ramps down
//!    linearly over the look-ahead instead of stepping).
//!
//! The audio is delayed by `L − 1` frames. Every value averaged in step 3
//! for the frame leaving the delay line is ≤ that frame's `r`, so the output
//! never exceeds the ceiling (a final clamp only catches float rounding).
//! All buffers are allocated in [`Limiter::new`] for the longest look-ahead.

use super::Frame;

/// Longest look-ahead.
pub const MAX_LOOKAHEAD_MS: f32 = 20.0;

#[derive(Debug, Clone)]
pub struct Limiter {
    sample_rate: f32,
    /// Capacity of every ring (max window length).
    cap: usize,
    /// Window length L (look-ahead frames + 1).
    window: usize,
    delay: Vec<Frame>,
    /// Sliding-minimum deque (monotonic, increasing from the front):
    /// (frame index, required gain).
    dq: Vec<(u64, f32)>,
    dq_head: usize,
    dq_len: usize,
    boxcar: Vec<f32>,
    box_sum: f64,
    pos: usize,
    n: u64,
    smooth: f32,
    ceiling: f32,
    release_coef: f32,
    /// Lowest gain applied in the last block (for a gain-reduction meter).
    pub block_min_gain: f32,
    /// Samples the final safety clamp had to touch (should stay 0).
    pub clamped: u64,
}

impl Limiter {
    /// `max_sample_rate` sizes the buffers (look-ahead up to
    /// [`MAX_LOOKAHEAD_MS`] at that rate).
    pub fn new(sample_rate: f32) -> Self {
        let sr = if sample_rate.is_finite() && sample_rate >= 8000.0 { sample_rate } else { 48_000.0 };
        let cap = (MAX_LOOKAHEAD_MS / 1000.0 * sr).ceil() as usize + 1;
        let mut l = Limiter {
            sample_rate: sr,
            cap,
            window: 1,
            delay: vec![[0.0; 2]; cap],
            dq: vec![(0, 1.0); cap],
            dq_head: 0,
            dq_len: 0,
            boxcar: vec![1.0; cap],
            box_sum: 0.0,
            pos: 0,
            n: 0,
            smooth: 1.0,
            ceiling: 1.0,
            release_coef: 1.0,
            block_min_gain: 1.0,
            clamped: 0,
        };
        l.configure(-1.0, 100.0, 5.0);
        l
    }

    /// Look-ahead in frames for `ms` (clamped to 0 … [`MAX_LOOKAHEAD_MS`]).
    pub fn lookahead_frames(&self, ms: f32) -> usize {
        let ms = if ms.is_finite() { ms.clamp(0.0, MAX_LOOKAHEAD_MS) } else { 5.0 };
        ((ms / 1000.0 * self.sample_rate).round() as usize).min(self.cap - 1)
    }

    /// Set ceiling (dBFS, clamped to −60 … 0), release (ms) and look-ahead
    /// (ms). Changing the look-ahead resets the delay line (a short gap);
    /// the other two change smoothly. Real-time safe.
    pub fn configure(&mut self, ceiling_db: f32, release_ms: f32, lookahead_ms: f32) {
        let c = if ceiling_db.is_finite() { ceiling_db.clamp(-60.0, 0.0) } else { -1.0 };
        self.ceiling = 10f32.powf(c / 20.0);
        let rel = if release_ms.is_finite() { release_ms.clamp(1.0, 5000.0) } else { 100.0 };
        self.release_coef = 1.0 - (-1.0 / (rel / 1000.0 * self.sample_rate)).exp();
        let w = self.lookahead_frames(lookahead_ms) + 1;
        if w != self.window {
            self.window = w;
            self.reset();
        }
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    /// Delay in frames.
    pub fn latency(&self) -> usize {
        self.window - 1
    }

    pub fn reset(&mut self) {
        for f in self.delay.iter_mut() {
            *f = [0.0; 2];
        }
        for b in self.boxcar.iter_mut() {
            *b = 1.0;
        }
        self.box_sum = self.window as f64;
        self.dq_head = 0;
        self.dq_len = 0;
        self.pos = 0;
        self.smooth = 1.0;
    }

    #[inline]
    fn dq_at(&self, i: usize) -> usize {
        (self.dq_head + i) % self.cap
    }

    #[inline]
    pub fn tick(&mut self, x: Frame) -> Frame {
        let w = self.window;
        let peak = x[0].abs().max(x[1].abs());
        // A hair under the exact ratio so float rounding in the average
        // cannot push the product over the ceiling.
        let r = if peak > self.ceiling { self.ceiling / peak * 0.999_99 } else { 1.0 };

        // 1. sliding minimum over the last `w` required gains.
        while self.dq_len > 0 {
            let back = self.dq_at(self.dq_len - 1);
            if self.dq[back].1 >= r {
                self.dq_len -= 1;
            } else {
                break;
            }
        }
        let slot = self.dq_at(self.dq_len);
        self.dq[slot] = (self.n, r);
        self.dq_len += 1;
        while self.dq_len > 0 && self.dq[self.dq_head].0 + (w as u64) <= self.n {
            self.dq_head = (self.dq_head + 1) % self.cap;
            self.dq_len -= 1;
        }
        let hold = self.dq[self.dq_head].1;

        // 2. instant attack, one-pole release.
        if hold < self.smooth {
            self.smooth = hold;
        } else {
            self.smooth += (hold - self.smooth) * self.release_coef;
        }

        // 3. boxcar over the window; the ring position is shared with the
        // delay line.
        let p = self.pos;
        self.box_sum += self.smooth as f64 - self.boxcar[p] as f64;
        self.boxcar[p] = self.smooth;
        let delayed_idx = (p + 1) % w;
        // The frame written `w - 1` frames ago (or this one when w == 1).
        let out_src = if w == 1 { x } else { self.delay[delayed_idx] };
        self.delay[p] = x;
        self.pos = delayed_idx;
        if self.pos == 0 {
            // Re-sum once per window so rounding never accumulates.
            self.box_sum = self.boxcar[..w].iter().map(|v| *v as f64).sum();
        }
        self.n += 1;

        let g = ((self.box_sum / w as f64) as f32).min(1.0);
        if g < self.block_min_gain {
            self.block_min_gain = g;
        }
        let mut y = [out_src[0] * g, out_src[1] * g];
        for s in y.iter_mut() {
            if s.abs() > self.ceiling {
                *s = self.ceiling.copysign(*s);
                self.clamped += 1;
            }
        }
        y
    }

    pub fn process(&mut self, buf: &mut [Frame]) {
        for f in buf.iter_mut() {
            *f = self.tick(*f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(n: usize, freq: f32, amp: f32, fs: f32) -> Vec<Frame> {
        (0..n)
            .map(|i| {
                let v = (2.0 * PI * freq * i as f32 / fs).sin() * amp;
                [v, v * 0.5]
            })
            .collect()
    }

    #[test]
    fn output_never_exceeds_the_ceiling() {
        let fs = 48_000.0;
        for (ceiling_db, la_ms) in [(-1.0, 5.0), (-6.0, 1.0), (0.0, 0.0), (-12.0, 20.0)] {
            let mut l = Limiter::new(fs);
            l.configure(ceiling_db, 50.0, la_ms);
            let ceiling = 10f32.powf(ceiling_db / 20.0);
            // Loud sine with impulses and a full-scale square burst on top.
            let mut buf = sine(24_000, 220.0, 3.0, fs);
            for i in (1000..24_000).step_by(3001) {
                buf[i] = [8.0, -9.0];
            }
            for f in buf[12_000..12_500].iter_mut() {
                *f = [1.0, -1.0];
            }
            l.process(&mut buf);
            let max = buf.iter().flat_map(|f| f.iter()).fold(0f32, |m, s| m.max(s.abs()));
            assert!(max <= ceiling * (1.0 + 1e-6), "ceiling {ceiling_db} dB, la {la_ms}: peak {max}");
            if la_ms > 0.0 {
                assert_eq!(l.clamped, 0, "the look-ahead alone holds the ceiling");
            }
        }
    }

    #[test]
    fn quiet_audio_passes_unchanged_but_delayed() {
        let fs = 48_000.0;
        let mut l = Limiter::new(fs);
        l.configure(-1.0, 100.0, 5.0);
        let d = l.latency();
        assert_eq!(d, 240);
        let src = sine(4800, 1000.0, 0.5, fs);
        let mut buf = src.clone();
        l.process(&mut buf);
        for i in d..buf.len() {
            assert!((buf[i][0] - src[i - d][0]).abs() < 1e-6);
            assert!((buf[i][1] - src[i - d][1]).abs() < 1e-6);
        }
        assert!(buf[..d].iter().all(|f| *f == [0.0, 0.0]));
    }

    #[test]
    fn gain_ramps_smoothly_and_recovers() {
        let fs = 48_000.0;
        let mut l = Limiter::new(fs);
        l.configure(-6.0, 20.0, 2.0);
        // Silence-ish, a 2x step, then back.
        let mut buf: Vec<Frame> = (0..20_000).map(|i| if (5000..10_000).contains(&i) { [2.0, 2.0] } else { [0.25, 0.25] }).collect();
        l.process(&mut buf);
        // Gain changes per frame are bounded by the boxcar (no step).
        let gains: Vec<f32> = buf.iter().enumerate().map(|(i, f)| {
            let x = if i >= l.latency() && (5000..10_000).contains(&(i - l.latency())) { 2.0 } else { 0.25 };
            f[0] / x
        }).collect();
        let max_step = gains.windows(2).skip(l.latency()).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_step < 0.01, "max per-frame gain change {max_step}");
        // Released back to (nearly) unity well after the burst.
        assert!((buf[19_999][0] - 0.25).abs() < 1e-3, "{}", buf[19_999][0]);
        assert!(l.block_min_gain < 0.26 && l.block_min_gain > 0.24, "{}", l.block_min_gain);
    }

    #[test]
    fn changing_lookahead_resets_without_allocating() {
        let mut l = Limiter::new(96_000.0);
        let cap = l.delay.capacity();
        l.configure(-1.0, 100.0, 20.0);
        assert_eq!(l.latency(), 1920);
        l.configure(-1.0, 100.0, 50.0); // clamped to the max
        assert_eq!(l.latency(), 1920);
        l.configure(-1.0, 100.0, 0.0);
        assert_eq!(l.latency(), 0);
        assert_eq!(l.delay.capacity(), cap);
        let mut buf = vec![[4.0f32, 0.1]; 16];
        l.process(&mut buf);
        assert!(buf.iter().all(|f| f[0].abs() <= l.ceiling() * 1.000001));
    }
}
