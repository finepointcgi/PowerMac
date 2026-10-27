//! The real-time mixing graph: what runs inside the IOProc.
//!
//! Signal flow per block (stereo internally):
//!
//! ```text
//! source:  read → trim → EQ → limiter → fader (gain·mute·solo·app level) → meter
//! output:  Σ sources × send → trim → EQ → master → limiter → meter → device channels
//! ```
//!
//! A [`Graph`] is built on the control thread with everything preallocated
//! and then owned by the audio thread, which only calls [`Graph::process`].
//! `process` does no allocation, takes no locks and makes no system calls
//! (checked by the counting-allocator test in `mixer::tests`); parameters
//! arrive through the atomics in [`Params`], and every gain is ramped
//! linearly across a block so changes never click.

use super::dsp::biquad::{Coeffs, ParametricEq, MAX_BANDS};
use super::dsp::limiter::Limiter;
use super::dsp::meter::MeterState;
use super::dsp::{sanitize, Frame};
use super::params::{Params, StripShared};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Frames processed per inner block (≈5 ms at 48 kHz; also the gain-ramp
/// length).
pub const CHUNK: usize = 256;

/// A channel of an IO stream: `stream` indexes the IOProc's buffer list
/// for that direction, `channel` the interleaved channel in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChanLoc {
    pub stream: u32,
    pub channel: u32,
}

impl ChanLoc {
    pub const fn new(stream: u32, channel: u32) -> Self {
        ChanLoc { stream, channel }
    }
}

/// Where each strip reads / writes. A missing right channel means mono: a
/// source copies left to both sides; an output writes `(L+R)/2` to left.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub sources: Vec<[Option<ChanLoc>; 2]>,
    pub outputs: Vec<[Option<ChanLoc>; 2]>,
}

/// The IO buffers of one callback: interleaved `f32` streams.
pub trait IoBuffers {
    /// Input stream `i`: samples and channel count.
    fn input(&self, i: usize) -> Option<(&[f32], usize)>;
    fn output_streams(&self) -> usize;
    fn output(&mut self, i: usize) -> Option<(&mut [f32], usize)>;
}

/// `Vec`-backed buffers (tests, offline rendering, the fake device layer).
#[derive(Debug, Clone, Default)]
pub struct VecIo {
    pub inputs: Vec<(Vec<f32>, usize)>,
    pub outputs: Vec<(Vec<f32>, usize)>,
}

impl VecIo {
    /// Streams of `frames` frames with the given channel counts.
    pub fn new(frames: usize, inputs: &[usize], outputs: &[usize]) -> Self {
        VecIo {
            inputs: inputs.iter().map(|&c| (vec![0.0; frames * c], c)).collect(),
            outputs: outputs.iter().map(|&c| (vec![0.0; frames * c], c)).collect(),
        }
    }

    pub fn input_mut(&mut self, stream: usize, channel: usize, frame: usize) -> &mut f32 {
        let (b, c) = &mut self.inputs[stream];
        &mut b[frame * *c + channel]
    }

    pub fn out(&self, stream: usize, channel: usize, frame: usize) -> f32 {
        let (b, c) = &self.outputs[stream];
        b[frame * *c + channel]
    }
}

impl IoBuffers for VecIo {
    fn input(&self, i: usize) -> Option<(&[f32], usize)> {
        self.inputs.get(i).map(|(b, c)| (b.as_slice(), *c))
    }
    fn output_streams(&self) -> usize {
        self.outputs.len()
    }
    fn output(&mut self, i: usize) -> Option<(&mut [f32], usize)> {
        self.outputs.get_mut(i).map(|(b, c)| (b.as_mut_slice(), *c))
    }
}

/// Audio-thread state of one strip.
struct Strip {
    gain: f32,
    trim: f32,
    eq: ParametricEq,
    eq_seen: u32,
    eq_scratch: [Option<Coeffs>; MAX_BANDS],
    limiter: Limiter,
    limiter_on: bool,
    limiter_cfg: [u32; 3],
    meter: MeterState,
}

impl Strip {
    fn new(sample_rate: f32) -> Self {
        Strip {
            gain: 0.0,
            trim: 1.0,
            eq: ParametricEq::default(),
            eq_seen: 0,
            eq_scratch: [None; MAX_BANDS],
            limiter: Limiter::new(sample_rate),
            limiter_on: false,
            limiter_cfg: [u32::MAX; 3],
            meter: MeterState::default(),
        }
    }

    /// Pick up EQ / limiter changes (once per callback).
    fn refresh(&mut self, p: &StripShared) {
        if let Some(v) = p.eq.try_read(self.eq_seen, &mut self.eq_scratch) {
            self.eq_seen = v;
            self.eq.set(&self.eq_scratch);
        }
        let on = p.limiter.enabled.load(Ordering::Relaxed);
        if on {
            let c = p.limiter.ceiling_db.load();
            let r = p.limiter.release_ms.load();
            let l = p.limiter.lookahead_ms.load();
            let bits = [c.to_bits(), r.to_bits(), l.to_bits()];
            if bits != self.limiter_cfg {
                self.limiter.configure(c, r, l);
                self.limiter_cfg = bits;
            }
            if !self.limiter_on {
                self.limiter.reset();
            }
        }
        self.limiter_on = on;
    }

    fn run(&mut self, buf: &mut [Frame], p: &StripShared, sample_rate: f32, limiter_last: bool) {
        ramp(buf, &mut self.trim, p.trim.load());
        if self.eq.is_active() {
            self.eq.process(buf);
        }
        let gain = p.gain.load();
        self.limiter.block_min_gain = 1.0;
        if limiter_last {
            ramp(buf, &mut self.gain, gain);
            if self.limiter_on {
                self.limiter.process(buf);
            }
        } else {
            if self.limiter_on {
                self.limiter.process(buf);
            }
            ramp(buf, &mut self.gain, gain);
        }
        let lg = if self.limiter_on { self.limiter.block_min_gain } else { 1.0 };
        self.meter.update(buf, sample_rate, lg, &p.meter);
    }
}

/// Multiply by a gain ramping linearly from `*cur` to `target`.
#[inline]
fn ramp(buf: &mut [Frame], cur: &mut f32, target: f32) {
    let target = sanitize(target);
    let from = *cur;
    if (from - target).abs() < 1e-7 {
        if target != 1.0 {
            for f in buf.iter_mut() {
                f[0] *= target;
                f[1] *= target;
            }
        }
    } else {
        let step = (target - from) / buf.len().max(1) as f32;
        for (i, f) in buf.iter_mut().enumerate() {
            let g = from + step * (i + 1) as f32;
            f[0] *= g;
            f[1] *= g;
        }
    }
    *cur = target;
}

pub struct Graph {
    params: Arc<Params>,
    layout: Layout,
    sample_rate: f32,
    src: Box<[Strip]>,
    out: Box<[Strip]>,
    send_cur: Box<[f32]>,
    src_buf: Box<[[Frame; CHUNK]]>,
    bus: Box<[Frame; CHUNK]>,
}

impl Graph {
    /// Build (control thread). `layout` must have one entry per strip in
    /// `params`.
    pub fn new(params: Arc<Params>, layout: Layout, sample_rate: f64) -> Self {
        let sr = sample_rate as f32;
        let ns = params.sources.len();
        let no = params.outputs.len();
        assert_eq!(layout.sources.len(), ns, "layout/params source count");
        assert_eq!(layout.outputs.len(), no, "layout/params output count");
        Graph {
            layout,
            sample_rate: sr,
            src: (0..ns).map(|_| Strip::new(sr)).collect(),
            out: (0..no).map(|_| Strip::new(sr)).collect(),
            send_cur: vec![0.0; ns * no].into_boxed_slice(),
            src_buf: vec![[[0.0; 2]; CHUNK]; ns].into_boxed_slice(),
            bus: Box::new([[0.0; 2]; CHUNK]),
            params,
        }
    }

    pub fn params(&self) -> &Arc<Params> {
        &self.params
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Render one callback. Zeroes every output stream first, then adds the
    /// mix into the mapped channels. Real-time safe.
    pub fn process(&mut self, io: &mut dyn IoBuffers, frames: usize) {
        let Graph { params, layout, sample_rate, src, out, send_cur, src_buf, bus } = self;
        let params: &Params = params;
        let stats = &params.stats;
        stats.callbacks.fetch_add(1, Ordering::Relaxed);
        stats.frames.fetch_add(frames as u64, Ordering::Relaxed);
        stats.max_frames.fetch_max(frames.min(u32::MAX as usize) as u32, Ordering::Relaxed);

        for i in 0..io.output_streams() {
            if let Some((b, _)) = io.output(i) {
                b.fill(0.0);
            }
        }
        for (s, p) in src.iter_mut().zip(params.sources.iter()) {
            s.refresh(p);
        }
        for (s, p) in out.iter_mut().zip(params.outputs.iter()) {
            s.refresh(p);
        }

        let no = out.len();
        let mut mismatch = false;
        let mut off = 0;
        while off < frames {
            let n = CHUNK.min(frames - off);

            // Sources.
            for (i, strip) in src.iter_mut().enumerate() {
                let buf = &mut src_buf[i][..n];
                let [l, r] = layout.sources[i];
                mismatch |= !read_channel(io, l, off, n, buf, 0);
                match r {
                    Some(_) => mismatch |= !read_channel(io, r, off, n, buf, 1),
                    None => {
                        for f in buf.iter_mut() {
                            f[1] = f[0];
                        }
                    }
                }
                strip.run(buf, &params.sources[i], *sample_rate, false);
            }

            // Outputs.
            for (o, strip) in out.iter_mut().enumerate() {
                let bus = &mut bus[..n];
                bus.fill([0.0; 2]);
                for (s, sbuf) in src_buf.iter().enumerate() {
                    let cur = &mut send_cur[s * no + o];
                    let target = sanitize(params.send(s, o).load());
                    if *cur == 0.0 && target == 0.0 {
                        continue;
                    }
                    let from = *cur;
                    let step = (target - from) / n as f32;
                    for (i, (b, x)) in bus.iter_mut().zip(sbuf[..n].iter()).enumerate() {
                        let g = from + step * (i + 1) as f32;
                        b[0] += x[0] * g;
                        b[1] += x[1] * g;
                    }
                    *cur = target;
                }
                strip.run(bus, &params.outputs[o], *sample_rate, true);
                let [l, r] = layout.outputs[o];
                match r {
                    Some(_) => {
                        mismatch |= !write_channel(io, l, off, bus, |f| f[0]);
                        mismatch |= !write_channel(io, r, off, bus, |f| f[1]);
                    }
                    None => mismatch |= !write_channel(io, l, off, bus, |f| 0.5 * (f[0] + f[1])),
                }
            }
            off += n;
        }
        if mismatch {
            stats.layout_mismatches.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Copy channel `loc` of frames `off..off+n` into `buf[..][side]`; silence
/// (and `false`) when the stream is missing or short.
#[inline]
fn read_channel(io: &dyn IoBuffers, loc: Option<ChanLoc>, off: usize, n: usize, buf: &mut [Frame], side: usize) -> bool {
    let Some(loc) = loc else {
        for f in buf.iter_mut() {
            f[side] = 0.0;
        }
        return true;
    };
    let Some((data, chs)) = io.input(loc.stream as usize) else {
        for f in buf.iter_mut() {
            f[side] = 0.0;
        }
        return false;
    };
    let ch = loc.channel as usize;
    let mut ok = ch < chs;
    for (i, f) in buf.iter_mut().enumerate().take(n) {
        let idx = (off + i) * chs + ch;
        f[side] = match data.get(idx) {
            Some(v) if ch < chs => sanitize(*v),
            _ => {
                ok = false;
                0.0
            }
        };
    }
    ok
}

#[inline]
fn write_channel(io: &mut dyn IoBuffers, loc: Option<ChanLoc>, off: usize, bus: &[Frame], pick: impl Fn(&Frame) -> f32) -> bool {
    let Some(loc) = loc else { return true };
    let Some((data, chs)) = io.output(loc.stream as usize) else { return false };
    let ch = loc.channel as usize;
    if ch >= chs {
        return false;
    }
    let mut ok = true;
    for (i, f) in bus.iter().enumerate() {
        match data.get_mut((off + i) * chs + ch) {
            Some(d) => *d += pick(f),
            None => ok = false,
        }
    }
    ok
}
