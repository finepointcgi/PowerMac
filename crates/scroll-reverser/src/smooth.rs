//! Mos-style smooth scrolling: turn discrete wheel notches into a stream of
//! small, eased deltas.
//!
//! Every notch becomes an *impulse* (`amount` output units spread over
//! `duration` along an easing curve). Impulses on the same axis overlap and
//! add up, so a fast spin keeps gliding instead of stuttering. Changing
//! direction drops whatever is still in flight on that axis, so the content
//! stops and turns right away. Optional acceleration grows the amount of each
//! notch while notches keep arriving quickly in the same direction.
//!
//! [`Smoother`] is pure (time is passed in as nanoseconds) and unit-tested;
//! [`Animator`] runs it on a frame thread and hands integer deltas to a
//! platform-specific poster.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Easing curve of one notch's glide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Easing {
    /// Fast start, long soft stop (the default; close to Mos).
    #[default]
    EaseOutCubic,
    /// Even more front-loaded than cubic: snappy, with a long tail.
    EaseOutQuint,
    /// Gentler than cubic.
    EaseOutQuad,
    /// Slow start and stop.
    EaseInOutCubic,
    /// Constant speed.
    Linear,
}

impl Easing {
    pub const ALL: [Easing; 5] = [
        Easing::EaseOutCubic,
        Easing::EaseOutQuint,
        Easing::EaseOutQuad,
        Easing::EaseInOutCubic,
        Easing::Linear,
    ];

    /// Progress (0..=1) at normalised time `t` (clamped to 0..=1).
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOutQuad => 1.0 - (1.0 - t).powi(2),
            Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::EaseOutQuint => 1.0 - (1.0 - t).powi(5),
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }

    pub(crate) fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|&e| e == self).unwrap_or(0) as u8
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or_default()
    }
}

/// Smoothing parameters in output units (pixels on macOS, wheel units on Windows).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmoothParams {
    /// Output units per notch (before acceleration).
    pub step: f64,
    pub duration_ns: u64,
    /// 0 = off, 1 = default strength.
    pub acceleration: f64,
    pub easing: Easing,
}

/// Notches closer together than this (same direction) build up acceleration.
pub const ACCEL_WINDOW_NS: u64 = 60_000_000;
/// Acceleration multiplier added per consecutive fast notch, at strength 1.
pub const ACCEL_PER_NOTCH: f64 = 0.25;
/// Cap on the consecutive-notch count used for acceleration.
pub const ACCEL_MAX_STREAK: u32 = 12;

#[derive(Clone, Copy, Debug)]
struct Impulse {
    start_ns: u64,
    amount: f64,
    /// Eased progress (0..=1) already emitted.
    done: f64,
}

#[derive(Clone, Debug, Default)]
struct Axis {
    impulses: Vec<Impulse>,
    last_ns: Option<u64>,
    last_sign: f64,
    streak: u32,
    rem: f64,
}

/// Two axes (0 = vertical, 1 = horizontal) of overlapping eased impulses.
#[derive(Clone, Debug, Default)]
pub struct Smoother {
    axes: [Axis; 2],
}

impl Smoother {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acceleration multiplier for a notch that continues a streak of `streak`
    /// fast notches.
    pub fn accel_multiplier(acceleration: f64, streak: u32) -> f64 {
        1.0 + acceleration.max(0.0) * streak.min(ACCEL_MAX_STREAK) as f64 * ACCEL_PER_NOTCH
    }

    /// Add one notch (`notches` may be fractional, e.g. a high-resolution
    /// Windows wheel; its sign is the direction) on `axis` at `now_ns`.
    /// Returns the amount added.
    pub fn push(&mut self, axis: usize, notches: f64, now_ns: u64, p: &SmoothParams) -> f64 {
        if notches == 0.0 || !notches.is_finite() {
            return 0.0;
        }
        let a = &mut self.axes[axis & 1];
        let sign = notches.signum();
        if a.last_sign != sign {
            // Direction change: stop what is still gliding the other way.
            a.impulses.clear();
            a.rem = 0.0;
            a.streak = 0;
        } else if a.last_ns.is_some_and(|t| now_ns.saturating_sub(t) <= ACCEL_WINDOW_NS) {
            a.streak = a.streak.saturating_add(1);
        } else {
            a.streak = 0;
        }
        a.last_sign = sign;
        a.last_ns = Some(now_ns);
        let amount = notches * p.step * Self::accel_multiplier(p.acceleration, a.streak);
        a.impulses.push(Impulse { start_ns: now_ns, amount, done: 0.0 });
        amount
    }

    /// Advance to `now_ns`; returns the whole output units to emit per axis.
    /// Fractions carry over; the last frame of a glide flushes the remainder,
    /// so the sum of all outputs equals the sum of pushed amounts (rounded).
    pub fn tick(&mut self, now_ns: u64, p: &SmoothParams) -> [i64; 2] {
        let mut out = [0i64; 2];
        let dur = p.duration_ns.max(1) as f64;
        for (i, a) in self.axes.iter_mut().enumerate() {
            if a.impulses.is_empty() && a.rem == 0.0 {
                continue;
            }
            let mut total = 0.0;
            for imp in &mut a.impulses {
                let t = now_ns.saturating_sub(imp.start_ns) as f64 / dur;
                let prog = if t >= 1.0 { 1.0 } else { p.easing.apply(t) };
                total += imp.amount * (prog - imp.done);
                imp.done = prog;
            }
            a.impulses.retain(|imp| imp.done < 1.0);
            a.rem += total;
            let whole = if a.impulses.is_empty() { a.rem.round() } else { a.rem.trunc() };
            a.rem -= whole;
            if a.impulses.is_empty() {
                a.rem = 0.0;
            }
            out[i] = whole as i64;
        }
        out
    }

    /// True while anything is still gliding.
    pub fn is_active(&self) -> bool {
        self.axes.iter().any(|a| !a.impulses.is_empty() || a.rem != 0.0)
    }

    /// Drop everything in flight.
    pub fn clear(&mut self) {
        for a in &mut self.axes {
            a.impulses.clear();
            a.rem = 0.0;
        }
    }
}

// ---------------------------------------------------------------------------
// Frame thread
// ---------------------------------------------------------------------------

/// Receives each frame's whole deltas: `[vertical, horizontal]`.
pub(crate) type Poster = Box<dyn FnMut([i64; 2]) + Send>;

struct AnimState {
    smoother: Smoother,
    params: SmoothParams,
    stop: bool,
}

struct AnimShared {
    state: Mutex<AnimState>,
    cv: Condvar,
    epoch: Instant,
}

/// Runs a [`Smoother`] on its own thread, waking only while something glides.
pub(crate) struct Animator {
    shared: Arc<AnimShared>,
    thread: Option<JoinHandle<()>>,
}

impl Animator {
    /// `frame`: interval between output events (e.g. 1/120 s).
    /// `on_start` / `on_idle` run on the frame thread when a glide starts and
    /// when it finishes (Windows uses them for the timer resolution).
    pub(crate) fn start(
        mut post: Poster,
        frame: Duration,
        on_start: fn(),
        on_idle: fn(),
    ) -> std::io::Result<Self> {
        let shared = Arc::new(AnimShared {
            state: Mutex::new(AnimState {
                smoother: Smoother::new(),
                params: SmoothParams {
                    step: 0.0,
                    duration_ns: 1,
                    acceleration: 0.0,
                    easing: Easing::default(),
                },
                stop: false,
            }),
            cv: Condvar::new(),
            epoch: Instant::now(),
        });
        let sh = shared.clone();
        let thread = std::thread::Builder::new().name("scroll-smoother".into()).spawn(move || {
            let mut guard = sh.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                while !guard.stop && !guard.smoother.is_active() {
                    guard = sh.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                }
                if guard.stop {
                    break;
                }
                on_start();
                while !guard.stop && guard.smoother.is_active() {
                    drop(guard);
                    std::thread::sleep(frame);
                    guard = sh.state.lock().unwrap_or_else(|e| e.into_inner());
                    let now = sh.epoch.elapsed().as_nanos() as u64;
                    let p = guard.params;
                    let out = guard.smoother.tick(now, &p);
                    if out != [0, 0] {
                        drop(guard);
                        post(out);
                        guard = sh.state.lock().unwrap_or_else(|e| e.into_inner());
                    }
                }
                on_idle();
            }
        })?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// Queue notches (`[vertical, horizontal]`, signed) with the current params.
    pub(crate) fn push(&self, notches: [f64; 2], params: SmoothParams) {
        let mut g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.shared.epoch.elapsed().as_nanos() as u64;
        g.params = params;
        for (axis, n) in notches.into_iter().enumerate() {
            g.smoother.push(axis, n, now, &params);
        }
        drop(g);
        self.shared.cv.notify_all();
    }

    pub(crate) fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            {
                let mut g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                g.stop = true;
                g.smoother.clear();
            }
            self.shared.cv.notify_all();
            let _ = t.join();
        }
    }
}

impl Drop for Animator {
    fn drop(&mut self) {
        self.stop();
    }
}
