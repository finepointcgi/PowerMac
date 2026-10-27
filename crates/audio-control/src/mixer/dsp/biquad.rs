//! Biquad filters (RBJ "Audio EQ Cookbook" designs) and a parametric EQ.
//!
//! Coefficients are designed in `f64` on the control thread
//! ([`Coeffs::design`]) and run in a transposed direct form II with `f64`
//! state, which stays accurate for low bands (20–60 Hz at 96 kHz) where
//! `f32` direct forms get noisy.

use super::{Frame, MIN_DB};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Filter shape of one EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BandKind {
    /// Bell: boost/cut `gain_db` around `freq_hz`, width from `q`.
    #[default]
    Peak,
    /// Boost/cut below `freq_hz` (`q` sets the slope; 0.707 = no overshoot).
    LowShelf,
    /// Boost/cut above `freq_hz`.
    HighShelf,
    /// 12 dB/octave low-pass (`gain_db` unused).
    LowPass,
    /// 12 dB/octave high-pass (`gain_db` unused).
    HighPass,
    /// Narrow cut at `freq_hz` (`gain_db` unused).
    Notch,
}

/// Normalised biquad coefficients (`a0 == 1`):
/// `y = b0·x + b1·x[-1] + b2·x[-2] − a1·y[-1] − a2·y[-2]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

pub const MIN_FREQ: f64 = 10.0;
pub const MIN_Q: f64 = 0.1;
pub const MAX_Q: f64 = 24.0;
pub const MAX_BAND_DB: f64 = 24.0;

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// RBJ cookbook design. `freq` is clamped to 10 Hz … 0.49·fs, `q` to
    /// 0.1 … 24 and `gain_db` to ±24 dB.
    pub fn design(kind: BandKind, sample_rate: f64, freq: f64, gain_db: f64, q: f64) -> Coeffs {
        let fs = if sample_rate.is_finite() && sample_rate > 1000.0 { sample_rate } else { 48_000.0 };
        let f0 = if freq.is_finite() { freq.clamp(MIN_FREQ, 0.49 * fs) } else { 1000.0 };
        let q = if q.is_finite() { q.clamp(MIN_Q, MAX_Q) } else { std::f64::consts::FRAC_1_SQRT_2 };
        let g = if gain_db.is_finite() { gain_db.clamp(-MAX_BAND_DB, MAX_BAND_DB) } else { 0.0 };
        let a = 10f64.powf(g / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let sa = 2.0 * a.sqrt() * alpha;
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BandKind::Peak => (1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a),
            BandKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cs + sa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                a * ((a + 1.0) - (a - 1.0) * cs - sa),
                (a + 1.0) + (a - 1.0) * cs + sa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                (a + 1.0) + (a - 1.0) * cs - sa,
            ),
            BandKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cs + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                a * ((a + 1.0) + (a - 1.0) * cs - sa),
                (a + 1.0) - (a - 1.0) * cs + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cs),
                (a + 1.0) - (a - 1.0) * cs - sa,
            ),
            BandKind::LowPass => ((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BandKind::HighPass => ((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BandKind::Notch => (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
        };
        Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// Whether this band does nothing (a 0 dB bell or shelf).
    pub fn is_identity(&self) -> bool {
        let e = 1e-12;
        (self.b0 - 1.0).abs() < e && (self.b1 - self.a1).abs() < e && (self.b2 - self.a2).abs() < e
    }

    /// |H(e^jω)| at `freq`.
    pub fn magnitude(&self, freq: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // e^{-jw} = c1 - j s1
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        20.0 * self.magnitude(freq, sample_rate).log10()
    }

    /// Both poles strictly inside the unit circle.
    pub fn is_stable(&self) -> bool {
        self.a2.abs() < 1.0 && self.a1.abs() < 1.0 + self.a2
    }
}

/// Stereo transposed-direct-form-II biquad.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    pub c: Coeffs,
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Default for Biquad {
    fn default() -> Self {
        Biquad { c: Coeffs::IDENTITY, z1: [0.0; 2], z2: [0.0; 2] }
    }
}

const DENORMAL: f64 = 1e-30;

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Biquad { c, ..Default::default() }
    }

    pub fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }

    #[inline]
    pub fn tick(&mut self, ch: usize, x: f64) -> f64 {
        let c = &self.c;
        let y = c.b0 * x + self.z1[ch];
        self.z1[ch] = c.b1 * x - c.a1 * y + self.z2[ch];
        self.z2[ch] = c.b2 * x - c.a2 * y;
        y
    }

    /// Filter a block of stereo frames in place.
    pub fn process(&mut self, buf: &mut [Frame]) {
        for f in buf.iter_mut() {
            for (ch, s) in f.iter_mut().enumerate() {
                *s = self.tick(ch, *s as f64) as f32;
            }
        }
        for ch in 0..2 {
            if !(self.z1[ch].is_finite() && self.z2[ch].is_finite()) {
                self.z1[ch] = 0.0;
                self.z2[ch] = 0.0;
            }
            if self.z1[ch].abs() < DENORMAL {
                self.z1[ch] = 0.0;
            }
            if self.z2[ch].abs() < DENORMAL {
                self.z2[ch] = 0.0;
            }
        }
    }
}

/// Most bands one EQ has.
pub const MAX_BANDS: usize = 10;

/// A cascade of up to [`MAX_BANDS`] biquads; inactive bands are skipped.
#[derive(Debug, Clone)]
pub struct ParametricEq {
    bands: [Biquad; MAX_BANDS],
    active: [bool; MAX_BANDS],
}

impl Default for ParametricEq {
    fn default() -> Self {
        ParametricEq { bands: [Biquad::default(); MAX_BANDS], active: [false; MAX_BANDS] }
    }
}

impl ParametricEq {
    /// Install new coefficients (`None` = band off). A band that turns on
    /// starts from silence; a band whose coefficients change keeps its state
    /// (no click on small moves).
    pub fn set(&mut self, coeffs: &[Option<Coeffs>; MAX_BANDS]) {
        for (i, c) in coeffs.iter().enumerate() {
            match c {
                Some(c) => {
                    if !self.active[i] {
                        self.bands[i].reset();
                    }
                    self.bands[i].c = *c;
                    self.active[i] = true;
                }
                None => self.active[i] = false,
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.iter().any(|a| *a)
    }

    pub fn process(&mut self, buf: &mut [Frame]) {
        for (b, on) in self.bands.iter_mut().zip(self.active.iter()) {
            if *on {
                b.process(buf);
            }
        }
    }

    pub fn reset(&mut self) {
        for b in self.bands.iter_mut() {
            b.reset();
        }
    }
}

/// Response of a whole band set in dB at `freq` (for the UI's curve).
pub fn response_db(coeffs: &[Coeffs], freq: f64, sample_rate: f64) -> f64 {
    let db: f64 = coeffs.iter().map(|c| c.magnitude_db(freq, sample_rate)).sum();
    db.max(MIN_DB as f64)
}

#[cfg(test)]
#[allow(clippy::approx_constant, clippy::excessive_precision)] // Q = 0.7071 on purpose; full-precision references
mod tests {
    use super::*;

    fn close(a: &Coeffs, want: [f64; 5]) {
        let have = [a.b0, a.b1, a.b2, a.a1, a.a2];
        for (h, w) in have.iter().zip(want.iter()) {
            assert!((h - w).abs() < 1e-12, "{have:?} vs {want:?}");
        }
    }

    /// Reference values from an independent implementation of the RBJ
    /// cookbook formulas (Python, double precision; script in the report).
    #[test]
    fn coefficients_match_reference() {
        close(
            &Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, 6.0, 1.0),
            [1.043953086990335e+00, -1.895320723936596e+00, 8.677222847598566e-01, -1.895320723936596e+00, 9.116753717501915e-01],
        );
        close(
            &Coeffs::design(BandKind::LowShelf, 48_000.0, 200.0, -4.5, 0.7071),
            [9.952032634492692e-01, -1.958038387410481e+00, 9.633531308257923e-01, -1.957862574839934e+00, 9.587322068456080e-01],
        );
        close(
            &Coeffs::design(BandKind::HighShelf, 44_100.0, 8000.0, 3.0, 0.7071),
            [1.241824796300133e+00, -7.467087697981489e-01, 2.933480625946877e-01, -4.134921952000633e-01, 2.019562842967350e-01],
        );
        close(
            &Coeffs::design(BandKind::LowPass, 48_000.0, 5000.0, 0.0, 0.7071),
            [7.223066687518030e-02, 1.444613337503606e-01, 7.223066687518030e-02, -1.109225591502890e+00, 3.981482590036111e-01],
        );
        close(
            &Coeffs::design(BandKind::HighPass, 96_000.0, 80.0, 0.0, 0.5),
            [9.947844905113388e-01, -1.989568981022678e+00, 9.947844905113388e-01, -1.989555344669556e+00, 9.895826173757991e-01],
        );
        close(
            &Coeffs::design(BandKind::Notch, 48_000.0, 60.0, 0.0, 10.0),
            [9.996074591044289e-01, -1.999153257712209e+00, 9.996074591044289e-01, -1.999153257712209e+00, 9.992149182088578e-01],
        );
    }

    #[test]
    fn responses_have_the_designed_shape() {
        let fs = 48_000.0;
        let p = Coeffs::design(BandKind::Peak, fs, 1000.0, 6.0, 1.0);
        assert!((p.magnitude_db(1000.0, fs) - 6.0).abs() < 1e-9);
        assert!(p.magnitude_db(20.0, fs).abs() < 0.05);
        assert!(p.magnitude_db(20_000.0, fs).abs() < 0.1);

        let ls = Coeffs::design(BandKind::LowShelf, fs, 200.0, -4.5, 0.7071);
        assert!((ls.magnitude_db(1.0, fs) + 4.5).abs() < 1e-3);
        assert!(ls.magnitude_db(20_000.0, fs).abs() < 1e-3);
        assert!((ls.magnitude_db(200.0, fs) + 2.25).abs() < 1e-6, "half the gain at the corner");

        let hs = Coeffs::design(BandKind::HighShelf, fs, 8000.0, 3.0, 0.7071);
        assert!((hs.magnitude_db(fs / 2.0 - 1.0, fs) - 3.0).abs() < 0.01);
        assert!(hs.magnitude_db(10.0, fs).abs() < 1e-3);

        let lp = Coeffs::design(BandKind::LowPass, fs, 5000.0, 0.0, std::f64::consts::FRAC_1_SQRT_2);
        assert!((lp.magnitude_db(5000.0, fs) + 3.0103).abs() < 1e-3, "−3 dB at the corner (Butterworth Q)");
        assert!(lp.magnitude_db(10.0, fs).abs() < 1e-6);
        assert!(lp.magnitude_db(20_000.0, fs) < -20.0);

        let hp = Coeffs::design(BandKind::HighPass, fs, 80.0, 0.0, std::f64::consts::FRAC_1_SQRT_2);
        assert!((hp.magnitude_db(80.0, fs) + 3.0103).abs() < 1e-3);
        assert!(hp.magnitude_db(10_000.0, fs).abs() < 1e-3);

        let n = Coeffs::design(BandKind::Notch, fs, 60.0, 0.0, 10.0);
        assert!(n.magnitude(60.0, fs) < 1e-6);
        assert!(n.magnitude_db(1000.0, fs).abs() < 0.01);
        for c in [p, ls, hs, lp, hp, n] {
            assert!(c.is_stable(), "{c:?}");
        }
    }

    #[test]
    fn zero_gain_bell_is_identity_and_inputs_are_clamped() {
        assert!(Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, 0.0, 1.0).is_identity());
        assert!(!Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, 1.0, 1.0).is_identity());
        // Out-of-range / NaN parameters still give a stable filter.
        for c in [
            Coeffs::design(BandKind::Peak, 48_000.0, 1e9, 100.0, 0.0),
            Coeffs::design(BandKind::LowPass, 48_000.0, -5.0, 0.0, 1e6),
            Coeffs::design(BandKind::HighShelf, f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        ] {
            assert!(c.is_stable(), "{c:?}");
            assert!([c.b0, c.b1, c.b2, c.a1, c.a2].iter().all(|v| v.is_finite()));
        }
        let c = Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, 100.0, 1.0);
        assert!((c.magnitude_db(1000.0, 48_000.0) - MAX_BAND_DB).abs() < 1e-6);
    }

    /// The filter in the time domain matches the analytic response.
    #[test]
    fn time_domain_gain_matches_magnitude() {
        let fs = 48_000.0;
        let c = Coeffs::design(BandKind::Peak, fs, 1000.0, 6.0, 1.0);
        let mut bq = Biquad::new(c);
        let n = 48_000;
        let mut buf: Vec<Frame> = (0..n)
            .map(|i| {
                let v = (2.0 * PI * 1000.0 * i as f64 / fs).sin() as f32 * 0.25;
                [v, -v]
            })
            .collect();
        bq.process(&mut buf);
        let peak = buf[n / 2..].iter().map(|f| f[0].abs()).fold(0f32, f32::max);
        let want = 0.25 * 10f32.powf(6.0 / 20.0);
        assert!((peak - want).abs() < 2e-3, "{peak} vs {want}");
        assert!((buf[n - 1][0] + buf[n - 1][1]).abs() < 1e-6, "channels are independent");
    }

    #[test]
    fn eq_cascade_and_band_switching() {
        let fs = 48_000.0;
        let mut eq = ParametricEq::default();
        assert!(!eq.is_active());
        let mut set = [None; MAX_BANDS];
        set[0] = Some(Coeffs::design(BandKind::Peak, fs, 1000.0, 6.0, 1.0));
        set[3] = Some(Coeffs::design(BandKind::Peak, fs, 1000.0, -6.0, 1.0));
        eq.set(&set);
        assert!(eq.is_active());
        // +6 then −6 at the same place cancel exactly in magnitude.
        let cs: Vec<Coeffs> = set.iter().flatten().copied().collect();
        assert!(response_db(&cs, 1000.0, fs).abs() < 1e-9);
        let mut buf = vec![[0.1f32, 0.1]; 512];
        eq.process(&mut buf);
        assert!(buf.iter().all(|f| f[0].is_finite()));
        eq.set(&[None; MAX_BANDS]);
        assert!(!eq.is_active());
        let mut buf2 = vec![[0.3f32, -0.3]; 8];
        eq.process(&mut buf2);
        assert_eq!(buf2[7], [0.3, -0.3], "all bands off = bypass");
    }

    #[test]
    fn nan_input_does_not_stick() {
        let mut bq = Biquad::new(Coeffs::design(BandKind::LowPass, 48_000.0, 100.0, 0.0, 0.7));
        let mut buf = vec![[f32::NAN, 0.0]; 4];
        bq.process(&mut buf);
        let mut ok = vec![[0.5f32, 0.5]; 64];
        bq.process(&mut ok);
        assert!(ok.iter().all(|f| f[0].is_finite() && f[1].is_finite()));
    }
}
