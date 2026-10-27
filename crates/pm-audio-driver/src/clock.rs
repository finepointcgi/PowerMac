//! The device clock: a zero-timestamp series derived from the host clock.
//!
//! The HAL drives IO from `GetZeroTimeStamp`: pairs (sample time, host
//! time) one period (`RING_FRAMES`) apart. A virtual device runs exactly at
//! its nominal rate, so the n-th timestamp is `(n·period, anchor +
//! n·period·ticks_per_frame)` where the anchor is the host time at StartIO.
//! There is no hardware, so latency and safety offset are zero.

/// Host ticks per second for a `mach_timebase_info` (numer/denom converts
/// ticks to nanoseconds).
pub fn host_ticks_per_second(numer: u32, denom: u32) -> f64 {
    1e9 * denom as f64 / numer.max(1) as f64
}

#[derive(Debug, Clone)]
pub struct Clock {
    ticks_per_frame: f64,
    period: u64,
    anchor: u64,
    count: u64,
    /// Bumped whenever the timeline is restarted (the HAL's "seed").
    seed: u64,
}

impl Clock {
    pub fn new(sample_rate: f64, period: u32, ticks_per_second: f64) -> Self {
        Clock { ticks_per_frame: ticks_per_second / sample_rate, period: period.max(1) as u64, anchor: 0, count: 0, seed: 1 }
    }

    pub fn ticks_per_frame(&self) -> f64 {
        self.ticks_per_frame
    }

    /// Change the rate (IO must be stopped).
    pub fn set_rate(&mut self, sample_rate: f64, ticks_per_second: f64) {
        self.ticks_per_frame = ticks_per_second / sample_rate;
    }

    /// Start a new timeline at host time `now`.
    pub fn start(&mut self, now: u64) {
        self.anchor = now;
        self.count = 0;
        self.seed = self.seed.wrapping_add(1).max(1);
    }

    fn ticks_per_period(&self) -> f64 {
        self.ticks_per_frame * self.period as f64
    }

    /// The latest zero timestamp at or before host time `now`:
    /// (sample time, host time, seed). Never goes backwards.
    pub fn zero_timestamp(&mut self, now: u64) -> (f64, u64, u64) {
        let elapsed = now.saturating_sub(self.anchor) as f64;
        let n = (elapsed / self.ticks_per_period()).floor() as u64;
        if n > self.count {
            self.count = n;
        }
        let sample = (self.count * self.period) as f64;
        let host = self.anchor + (self.count as f64 * self.ticks_per_period()).round() as u64;
        (sample, host, self.seed)
    }

    /// Host time of a sample time on the current timeline (for tests and
    /// diagnostics).
    pub fn host_time_of(&self, sample: f64) -> u64 {
        self.anchor + (sample * self.ticks_per_frame).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: f64 = 1e9; // 1 tick = 1 ns (Intel timebase 1/1)

    #[test]
    fn timebase_conversion() {
        assert_eq!(host_ticks_per_second(1, 1), 1e9);
        // Apple Silicon: 125/3 → 24 MHz.
        assert!((host_ticks_per_second(125, 3) - 24e6).abs() < 1e-6);
    }

    #[test]
    fn timestamps_advance_one_period_at_a_time() {
        let mut c = Clock::new(48_000.0, 16_384, NS);
        c.start(1_000);
        let period_ticks = 16_384.0 / 48_000.0 * NS;
        assert_eq!(c.zero_timestamp(1_000), (0.0, 1_000, c.seed));
        assert_eq!(c.zero_timestamp(1_000 + period_ticks as u64 - 1).0, 0.0);
        let (s, h, _) = c.zero_timestamp(1_000 + period_ticks.ceil() as u64);
        assert_eq!(s, 16_384.0);
        assert_eq!(h, 1_000 + period_ticks.round() as u64);
        // After a long stall the series catches up in one step.
        let (s, h, _) = c.zero_timestamp(1_000 + (period_ticks * 10.5) as u64);
        assert_eq!(s, 16_384.0 * 10.0);
        assert_eq!(h, 1_000 + (period_ticks * 10.0).round() as u64);
    }

    #[test]
    fn host_and_sample_time_agree_with_the_rate() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            let tps = host_ticks_per_second(125, 3);
            let mut c = Clock::new(rate, 16_384, tps);
            c.start(0);
            let one_second = tps as u64;
            let (s, h, _) = c.zero_timestamp(one_second * 60);
            // sample/host ratio is exactly the nominal rate.
            assert!(((s / (h as f64 / tps)) - rate).abs() < 0.01, "rate {rate}");
            assert!(s <= rate * 60.0 && s > rate * 60.0 - 16_384.0);
        }
    }

    #[test]
    fn never_goes_backwards_and_seed_changes_on_restart() {
        let mut c = Clock::new(48_000.0, 16_384, NS);
        c.start(0);
        let late = c.zero_timestamp(10 * NS as u64);
        let early = c.zero_timestamp(NS as u64);
        assert_eq!(late, early, "a call with an older host time keeps the newest stamp");
        let seed = late.2;
        c.start(20 * NS as u64);
        let (s, _, seed2) = c.zero_timestamp(20 * NS as u64);
        assert_eq!(s, 0.0);
        assert_ne!(seed, seed2);
    }

    #[test]
    fn host_time_of_sample() {
        let mut c = Clock::new(48_000.0, 16_384, NS);
        c.start(500);
        assert_eq!(c.host_time_of(48_000.0), 500 + NS as u64);
    }
}
