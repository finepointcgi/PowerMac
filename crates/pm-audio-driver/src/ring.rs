//! The loopback ring: what is written to a device's output at sample time
//! `t` can be read from its input at sample time `t`.
//!
//! Interleaved f32 frames, indexed by absolute sample time modulo the ring
//! size. The writer keeps a contiguous history: a jump forward zeroes the
//! skipped frames, so a reader never hears audio from before an output
//! pause. Reads outside the last `frames` written frames are silence, which
//! keeps multiple readers independent (reading never consumes).
//!
//! Allocation happens only in [`LoopbackRing::new`] / [`LoopbackRing::reset`]
//! (outside IO); `read` and `write` are real-time safe.

pub struct LoopbackRing {
    frames: usize,
    channels: usize,
    data: Vec<f32>,
    /// One past the newest written frame (sample time), if anything was
    /// written since the last clear.
    end: Option<i64>,
}

impl LoopbackRing {
    pub fn new(frames: usize, channels: usize) -> Self {
        let frames = frames.max(1);
        let channels = channels.max(1);
        LoopbackRing { frames, channels, data: vec![0.0; frames * channels], end: None }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// New geometry (reallocates; not for the IO thread).
    pub fn reset(&mut self, frames: usize, channels: usize) {
        *self = LoopbackRing::new(frames, channels);
    }

    /// Forget everything written (no allocation).
    pub fn clear(&mut self) {
        self.data.fill(0.0);
        self.end = None;
    }

    fn pos(&self, t: i64) -> usize {
        t.rem_euclid(self.frames as i64) as usize
    }

    /// Zero frames `[from, to)` (at most one ring's worth).
    fn zero(&mut self, from: i64, to: i64) {
        let from = from.max(to - self.frames as i64);
        let mut t = from;
        while t < to {
            let p = self.pos(t);
            let n = ((self.frames - p) as i64).min(to - t) as usize;
            self.data[p * self.channels..(p + n) * self.channels].fill(0.0);
            t += n as i64;
        }
    }

    /// Write interleaved `src` starting at sample time `t`, scaled by `gain`.
    /// A trailing partial frame is ignored.
    pub fn write(&mut self, t: i64, src: &[f32], gain: f32) {
        let ch = self.channels;
        let n = src.len() / ch;
        if n == 0 {
            return;
        }
        if let Some(end) = self.end {
            if t > end {
                self.zero(end, t);
            }
        }
        // Only the newest `frames` frames fit.
        let skip = n.saturating_sub(self.frames);
        let mut t0 = t + skip as i64;
        let mut src = &src[skip * ch..n * ch];
        while !src.is_empty() {
            let p = self.pos(t0);
            let k = (self.frames - p).min(src.len() / ch);
            let dst = &mut self.data[p * ch..(p + k) * ch];
            if gain == 1.0 {
                dst.copy_from_slice(&src[..k * ch]);
            } else {
                for (d, s) in dst.iter_mut().zip(&src[..k * ch]) {
                    *d = s * gain;
                }
            }
            src = &src[k * ch..];
            t0 += k as i64;
        }
        self.end = Some(t + n as i64);
    }

    /// Fill interleaved `dst` with the frames at sample time `t`; frames
    /// never written (or overwritten since) are silence.
    pub fn read(&self, t: i64, dst: &mut [f32]) {
        let ch = self.channels;
        let n = (dst.len() / ch) as i64;
        dst.fill(0.0);
        let Some(end) = self.end else { return };
        let lo = t.max(end - self.frames as i64);
        let hi = (t + n).min(end);
        let mut s = lo;
        while s < hi {
            let p = self.pos(s);
            let k = ((self.frames - p) as i64).min(hi - s) as usize;
            let o = (s - t) as usize * ch;
            dst[o..o + k * ch].copy_from_slice(&self.data[p * ch..(p + k) * ch]);
            s += k as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(start: f32, frames: usize, ch: usize) -> Vec<f32> {
        (0..frames * ch).map(|i| start + (i / ch) as f32 + (i % ch) as f32 * 0.25).collect()
    }

    #[test]
    fn write_then_read_same_time() {
        let mut r = LoopbackRing::new(64, 2);
        let src = ramp(1.0, 16, 2);
        r.write(10, &src, 1.0);
        let mut out = vec![9.0; 32];
        r.read(10, &mut out);
        assert_eq!(out, src);
    }

    #[test]
    fn wraps_around_the_end() {
        let mut r = LoopbackRing::new(16, 2);
        let src = ramp(1.0, 10, 2);
        r.write(12, &src, 1.0); // positions 12..16, 0..6
        let mut out = vec![0.0; 20];
        r.read(12, &mut out);
        assert_eq!(out, src);
        // Negative sample times map correctly too.
        let mut r = LoopbackRing::new(16, 1);
        r.write(-3, &[1.0, 2.0, 3.0, 4.0], 1.0);
        let mut out = [0.0; 4];
        r.read(-3, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn unwritten_and_future_frames_are_silent() {
        let mut r = LoopbackRing::new(32, 1);
        let mut out = [5.0; 4];
        r.read(0, &mut out);
        assert_eq!(out, [0.0; 4], "nothing written yet");
        r.write(8, &[1.0, 2.0, 3.0, 4.0], 1.0);
        let mut out = [5.0; 8];
        r.read(6, &mut out); // 6,7 before the write; 12,13 after it
        assert_eq!(out, [0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 0.0, 0.0]);
    }

    #[test]
    fn frames_older_than_one_ring_are_silent() {
        let mut r = LoopbackRing::new(8, 1);
        r.write(0, &[1.0; 8], 1.0);
        r.write(8, &[2.0; 4], 1.0); // overwrites positions 0..4
        let mut out = [9.0; 4];
        r.read(0, &mut out);
        assert_eq!(out, [0.0; 4], "overwritten frames must not alias newer data");
        let mut out = [9.0; 4];
        r.read(4, &mut out);
        assert_eq!(out, [1.0; 4]);
    }

    #[test]
    fn a_gap_in_writing_reads_as_silence() {
        let mut r = LoopbackRing::new(16, 1);
        r.write(0, &[1.0; 16], 1.0);
        r.write(20, &[2.0; 2], 1.0); // frames 16..20 were never written
        let mut out = [9.0; 6];
        r.read(16, &mut out);
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0, 2.0, 2.0]);
        // A gap longer than the ring clears everything.
        r.write(100, &[3.0], 1.0);
        let mut out = [9.0; 4];
        r.read(97, &mut out);
        assert_eq!(out, [0.0, 0.0, 0.0, 3.0]);
    }

    #[test]
    fn gain_scales_and_clear_forgets() {
        let mut r = LoopbackRing::new(8, 2);
        r.write(0, &[1.0, -1.0, 0.5, -0.5], 0.5);
        let mut out = [0.0; 4];
        r.read(0, &mut out);
        assert_eq!(out, [0.5, -0.5, 0.25, -0.25]);
        r.clear();
        r.read(0, &mut out);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn oversized_writes_keep_the_newest_frames() {
        let mut r = LoopbackRing::new(4, 1);
        r.write(0, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 1.0);
        let mut out = [0.0; 6];
        r.read(0, &mut out);
        assert_eq!(out, [0.0, 0.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn continuous_streaming_round_trips() {
        // Writer ahead of the reader by two buffers, like the HAL's output
        // and input times.
        let (ch, buf) = (8, 512usize);
        let mut r = LoopbackRing::new(16_384, ch);
        let mut written = Vec::new();
        let mut read = Vec::new();
        for cycle in 0..200i64 {
            let src = ramp(cycle as f32 * 1000.0, buf, ch);
            r.write(cycle * buf as i64, &src, 1.0);
            written.extend_from_slice(&src);
            if cycle >= 2 {
                let mut out = vec![0.0; buf * ch];
                r.read((cycle - 2) * buf as i64, &mut out);
                read.extend_from_slice(&out);
            }
        }
        assert_eq!(&written[..read.len()], &read[..]);
    }
}
