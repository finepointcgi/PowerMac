//! Parameters shared between the control thread and the audio thread.
//!
//! Everything the audio thread reads is an atomic: gains and limiter
//! settings are single `f32`s (ramped on the audio side, so torn updates
//! across fields are harmless), and each EQ's coefficient set is published
//! through a seqlock the audio thread only ever *tries* to read — if a
//! write is in progress it keeps the coefficients it has and looks again
//! next block. The audio thread never waits, locks or allocates.

use super::dsp::biquad::{Coeffs, MAX_BANDS};
use super::dsp::meter::MeterShared;
use super::dsp::AtomicF32;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};

/// A seqlock-published set of up to [`MAX_BANDS`] biquad coefficient sets.
#[derive(Debug, Default)]
pub struct EqShared {
    seq: AtomicU32,
    active: [AtomicBool; MAX_BANDS],
    coeffs: [[AtomicU64; 5]; MAX_BANDS],
}

impl EqShared {
    /// Publish (control thread; callers serialise writers).
    pub fn publish(&self, bands: &[Option<Coeffs>; MAX_BANDS]) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (i, b) in bands.iter().enumerate() {
            self.active[i].store(b.is_some(), Ordering::Relaxed);
            let c = b.unwrap_or(Coeffs::IDENTITY);
            for (slot, v) in self.coeffs[i].iter().zip([c.b0, c.b1, c.b2, c.a1, c.a2]) {
                slot.store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Audio thread: if a newer, complete set than `seen` is available,
    /// copy it into `out` and return its version. Never blocks.
    pub fn try_read(&self, seen: u32, out: &mut [Option<Coeffs>; MAX_BANDS]) -> Option<u32> {
        let s1 = self.seq.load(Ordering::Acquire);
        if s1 & 1 == 1 || s1 == seen {
            return None;
        }
        let mut tmp = [None; MAX_BANDS];
        for (i, t) in tmp.iter_mut().enumerate() {
            let on = self.active[i].load(Ordering::Relaxed);
            let v: [f64; 5] = std::array::from_fn(|k| f64::from_bits(self.coeffs[i][k].load(Ordering::Relaxed)));
            *t = on.then_some(Coeffs { b0: v[0], b1: v[1], b2: v[2], a1: v[3], a2: v[4] });
        }
        fence(Ordering::Acquire);
        let s2 = self.seq.load(Ordering::Relaxed);
        if s1 != s2 {
            return None;
        }
        *out = tmp;
        Some(s1)
    }

    pub fn version(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }
}

/// Limiter settings of one strip.
#[derive(Debug)]
pub struct LimiterShared {
    pub enabled: AtomicBool,
    pub ceiling_db: AtomicF32,
    pub release_ms: AtomicF32,
    pub lookahead_ms: AtomicF32,
}

impl Default for LimiterShared {
    fn default() -> Self {
        LimiterShared {
            enabled: AtomicBool::new(false),
            ceiling_db: AtomicF32::new(-1.0),
            release_ms: AtomicF32::new(100.0),
            lookahead_ms: AtomicF32::new(5.0),
        }
    }
}

/// One source or output strip.
#[derive(Debug)]
pub struct StripShared {
    /// Effective linear fader gain (mute, solo, app level and master
    /// already folded in by the control thread).
    pub gain: AtomicF32,
    /// Effects input trim (linear).
    pub trim: AtomicF32,
    pub eq: EqShared,
    pub limiter: LimiterShared,
    pub meter: MeterShared,
}

impl Default for StripShared {
    fn default() -> Self {
        StripShared { gain: AtomicF32::new(0.0), trim: AtomicF32::new(1.0), eq: EqShared::default(), limiter: LimiterShared::default(), meter: MeterShared::new() }
    }
}

/// Counters for diagnostics.
#[derive(Debug, Default)]
pub struct Stats {
    pub callbacks: AtomicU64,
    pub frames: AtomicU64,
    pub max_frames: AtomicU32,
    /// Callbacks whose buffers did not match the expected layout.
    pub layout_mismatches: AtomicU64,
}

/// Everything for one running graph (fixed size once built).
#[derive(Debug)]
pub struct Params {
    pub sources: Box<[StripShared]>,
    pub outputs: Box<[StripShared]>,
    /// Row-major `[source][output]` linear send gains (mute folded in).
    pub sends: Box<[AtomicF32]>,
    pub stats: Stats,
}

impl Params {
    pub fn new(sources: usize, outputs: usize) -> Self {
        Params {
            sources: (0..sources).map(|_| StripShared::default()).collect(),
            outputs: (0..outputs).map(|_| StripShared::default()).collect(),
            sends: (0..sources * outputs).map(|_| AtomicF32::zero()).collect(),
            stats: Stats::default(),
        }
    }

    pub fn send(&self, s: usize, o: usize) -> &AtomicF32 {
        &self.sends[s * self.outputs.len() + o]
    }
}

/// Plain values for one strip (computed by the planner, stored into
/// [`StripShared`]).
#[derive(Debug, Clone, PartialEq)]
pub struct StripValues {
    pub gain: f32,
    pub trim: f32,
    pub eq: [Option<Coeffs>; MAX_BANDS],
    pub limiter_enabled: bool,
    pub ceiling_db: f32,
    pub release_ms: f32,
    pub lookahead_ms: f32,
}

impl Default for StripValues {
    fn default() -> Self {
        StripValues { gain: 1.0, trim: 1.0, eq: [None; MAX_BANDS], limiter_enabled: false, ceiling_db: -1.0, release_ms: 100.0, lookahead_ms: 5.0 }
    }
}

/// All parameter values of a graph.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamValues {
    pub sources: Vec<StripValues>,
    pub outputs: Vec<StripValues>,
    /// Row-major `[source][output]`.
    pub sends: Vec<f32>,
}

impl ParamValues {
    pub fn send(&self, s: usize, o: usize) -> f32 {
        self.sends.get(s * self.outputs.len() + o).copied().unwrap_or(0.0)
    }

    /// Linear gain from source `s` into output `o` before effects: source
    /// fader × send × output master.
    pub fn path_gain(&self, s: usize, o: usize) -> f32 {
        self.sources.get(s).map_or(0.0, |v| v.gain) * self.send(s, o) * self.outputs.get(o).map_or(0.0, |v| v.gain)
    }
}

fn store_strip(dst: &StripShared, v: &StripValues, last: Option<&StripValues>) {
    dst.gain.store(v.gain);
    dst.trim.store(v.trim);
    if last.is_none_or(|l| l.eq != v.eq) {
        dst.eq.publish(&v.eq);
    }
    dst.limiter.ceiling_db.store(v.ceiling_db);
    dst.limiter.release_ms.store(v.release_ms);
    dst.limiter.lookahead_ms.store(v.lookahead_ms);
    dst.limiter.enabled.store(v.limiter_enabled, Ordering::Relaxed);
}

impl Params {
    /// Store new values (control thread). `last` skips republishing
    /// unchanged EQs. Sizes must match the graph; extra values are ignored.
    pub fn store(&self, v: &ParamValues, last: Option<&ParamValues>) {
        for (i, s) in self.sources.iter().enumerate() {
            if let Some(sv) = v.sources.get(i) {
                store_strip(s, sv, last.and_then(|l| l.sources.get(i)));
            }
        }
        for (i, o) in self.outputs.iter().enumerate() {
            if let Some(ov) = v.outputs.get(i) {
                store_strip(o, ov, last.and_then(|l| l.outputs.get(i)));
            }
        }
        for (i, g) in self.sends.iter().enumerate() {
            g.store(v.sends.get(i).copied().unwrap_or(0.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::dsp::biquad::BandKind;
    use std::sync::Arc;

    #[test]
    fn seqlock_publishes_complete_sets() {
        let eq = EqShared::default();
        let mut out = [None; MAX_BANDS];
        assert_eq!(eq.try_read(0, &mut out), None, "nothing published yet");
        let mut set = [None; MAX_BANDS];
        set[1] = Some(Coeffs::design(BandKind::Peak, 48_000.0, 500.0, 3.0, 1.0));
        eq.publish(&set);
        let v = eq.try_read(0, &mut out).unwrap();
        assert_eq!(out, set);
        assert_eq!(eq.try_read(v, &mut out), None, "already seen");
    }

    /// A reader racing a writer only ever sees whole coefficient sets.
    #[test]
    fn seqlock_never_tears() {
        let eq = Arc::new(EqShared::default());
        let w = eq.clone();
        let writer = std::thread::spawn(move || {
            for i in 0..20_000 {
                let g = (i % 2) as f64 * 6.0 - 3.0;
                let c = Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, g, 1.0);
                w.publish(&[Some(c); MAX_BANDS]);
            }
        });
        let a = Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, -3.0, 1.0);
        let b = Coeffs::design(BandKind::Peak, 48_000.0, 1000.0, 3.0, 1.0);
        let mut seen = 0;
        let mut reads = 0;
        let mut out = [None; MAX_BANDS];
        while !writer.is_finished() || reads == 0 {
            if let Some(v) = eq.try_read(seen, &mut out) {
                seen = v;
                reads += 1;
                let first = out[0].unwrap();
                assert!(first == a || first == b);
                assert!(out.iter().all(|c| c.unwrap() == first), "torn read");
            }
        }
        writer.join().unwrap();
        assert!(reads > 0);
    }

    #[test]
    fn store_and_path_gain() {
        let p = Params::new(2, 3);
        let mut v = ParamValues {
            sources: vec![StripValues { gain: 0.5, ..Default::default() }, StripValues { gain: 1.0, ..Default::default() }],
            outputs: vec![StripValues::default(); 3],
            sends: vec![0.0; 6],
        };
        v.sends[3 + 2] = 0.25; // [1][2]
        v.outputs[2].gain = 2.0;
        p.store(&v, None);
        assert_eq!(p.send(1, 2).load(), 0.25);
        assert_eq!(p.outputs[2].gain.load(), 2.0);
        assert_eq!(v.path_gain(1, 2), 0.5);
        assert_eq!(v.path_gain(0, 2), 0.0);
        let ver = p.sources[0].eq.version();
        p.store(&v, Some(&v));
        assert_eq!(p.sources[0].eq.version(), ver, "unchanged EQ is not republished");
    }
}
