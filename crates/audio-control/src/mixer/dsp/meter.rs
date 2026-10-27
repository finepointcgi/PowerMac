//! Peak / RMS metering. The audio thread keeps the ballistics
//! ([`MeterState`]) and publishes plain values into [`MeterShared`]; any
//! number of readers can poll them (the UI at ~20 Hz) without resetting
//! anything.

use super::{AtomicF32, Frame};
use std::sync::atomic::{AtomicU32, Ordering};

/// Peak fall-back time constant (≈ 17 dB/s).
pub const PEAK_DECAY_S: f32 = 0.5;
/// RMS integration time constant (VU-like).
pub const RMS_WINDOW_S: f32 = 0.3;

/// What the UI reads.
#[derive(Debug, Default)]
pub struct MeterShared {
    pub peak: [AtomicF32; 2],
    pub rms: [AtomicF32; 2],
    /// Lowest limiter gain in the last block (1.0 = no reduction).
    pub limiter_gain: AtomicF32,
    /// Blocks in which a sample reached full scale.
    pub clips: AtomicU32,
}

impl MeterShared {
    pub fn new() -> Self {
        let m = MeterShared::default();
        m.limiter_gain.store(1.0);
        m
    }

    pub fn read(&self) -> MeterValues {
        MeterValues {
            peak: [self.peak[0].load(), self.peak[1].load()],
            rms: [self.rms[0].load(), self.rms[1].load()],
            limiter_gain: self.limiter_gain.load(),
            clips: self.clips.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterValues {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
    pub limiter_gain: f32,
    pub clips: u32,
}

/// Audio-thread side.
#[derive(Debug, Clone, Default)]
pub struct MeterState {
    peak: [f32; 2],
    ms: [f32; 2],
}

impl MeterState {
    pub fn update(&mut self, buf: &[Frame], sample_rate: f32, limiter_gain: f32, out: &MeterShared) {
        let n = buf.len();
        if n == 0 {
            return;
        }
        let mut pk = [0f32; 2];
        let mut sq = [0f64; 2];
        for f in buf {
            for c in 0..2 {
                let a = f[c].abs();
                if a > pk[c] {
                    pk[c] = a;
                }
                sq[c] += (f[c] as f64) * (f[c] as f64);
            }
        }
        let t = n as f32 / sample_rate.max(1.0);
        let pk_decay = (-t / PEAK_DECAY_S).exp();
        let rms_k = (-t / RMS_WINDOW_S).exp();
        let mut clipped = false;
        for c in 0..2 {
            self.peak[c] = pk[c].max(self.peak[c] * pk_decay);
            let mean = (sq[c] / n as f64) as f32;
            self.ms[c] = self.ms[c] * rms_k + mean * (1.0 - rms_k);
            if self.ms[c] < 1e-20 {
                self.ms[c] = 0.0;
            }
            if self.peak[c] < 1e-9 {
                self.peak[c] = 0.0;
            }
            clipped |= pk[c] >= 1.0;
            out.peak[c].store(self.peak[c]);
            out.rms[c].store(self.ms[c].sqrt());
        }
        out.limiter_gain.store(limiter_gain);
        if clipped {
            out.clips.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_meters_read_peak_and_rms() {
        let fs = 48_000.0;
        let shared = MeterShared::new();
        let mut st = MeterState::default();
        let mut phase = 0f32;
        for _ in 0..400 {
            let block: Vec<Frame> = (0..256)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 1000.0 / fs;
                    let v = phase.sin() * 0.5;
                    [v, 0.0]
                })
                .collect();
            st.update(&block, fs, 1.0, &shared);
        }
        let v = shared.read();
        assert!((v.peak[0] - 0.5).abs() < 1e-3, "{v:?}");
        assert!((v.rms[0] - 0.5 / 2f32.sqrt()).abs() < 2e-3, "{v:?}");
        assert_eq!(v.peak[1], 0.0);
        assert_eq!(v.clips, 0);
        assert_eq!(v.limiter_gain, 1.0);
    }

    #[test]
    fn peak_falls_back_and_clips_count() {
        let fs = 48_000.0;
        let shared = MeterShared::new();
        let mut st = MeterState::default();
        st.update(&[[1.0, -1.0]; 64], fs, 0.5, &shared);
        assert_eq!(shared.read().clips, 1);
        assert_eq!(shared.read().limiter_gain, 0.5);
        // One second of silence: down by e^-2 ≈ −17 dB.
        for _ in 0..(48_000 / 480) {
            st.update(&[[0.0, 0.0]; 480], fs, 1.0, &shared);
        }
        let p = shared.read().peak[0];
        assert!((p - (-2f32).exp()).abs() < 0.01, "{p}");
    }
}
