//! Pure-Rust, real-time-safe DSP building blocks for the mixer.
//!
//! Nothing in here allocates, locks or calls the OS once constructed: every
//! processor preallocates its state in `new` (on the control thread) and
//! `process*` only does arithmetic. Parameters cross threads through the
//! atomics in [`AtomicF32`] and the seqlock in `crate::mixer::params`.

pub mod biquad;
pub mod limiter;
pub mod meter;

use std::sync::atomic::{AtomicU32, Ordering};

/// Gains at or below this are silence.
pub const MIN_DB: f32 = -96.0;
/// Highest gain anywhere in the mixer.
pub const MAX_DB: f32 = 24.0;

/// dB → linear amplitude. `<= MIN_DB` (and NaN) is 0; clamped at [`MAX_DB`].
pub fn db_to_gain(db: f32) -> f32 {
    if db.is_nan() || db <= MIN_DB {
        return 0.0;
    }
    10f32.powf(db.min(MAX_DB) / 20.0)
}

/// Linear amplitude → dB (`-inf` for 0).
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * g.log10()
    }
}

/// An `f32` that can be shared between the control and audio threads.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub const fn zero() -> Self {
        AtomicF32(AtomicU32::new(0))
    }

    pub fn new(v: f32) -> Self {
        AtomicF32(AtomicU32::new(v.to_bits()))
    }

    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// One stereo frame.
pub type Frame = [f32; 2];

/// Replace NaN / ±inf with silence (a broken source must not poison the
/// filters of everything it is mixed into).
#[inline]
pub fn sanitize(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversions() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((db_to_gain(6.0206) - 2.0).abs() < 1e-3);
        assert_eq!(db_to_gain(MIN_DB), 0.0);
        assert_eq!(db_to_gain(-200.0), 0.0);
        assert_eq!(db_to_gain(f32::NAN), 0.0);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert!((db_to_gain(100.0) - db_to_gain(MAX_DB)).abs() < 1e-6);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn atomic_f32_round_trips() {
        let a = AtomicF32::new(-0.25);
        assert_eq!(a.load(), -0.25);
        a.store(3.5);
        assert_eq!(a.load(), 3.5);
        assert_eq!(AtomicF32::zero().load(), 0.0);
    }
}
