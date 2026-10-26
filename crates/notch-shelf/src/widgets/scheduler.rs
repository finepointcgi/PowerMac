//! Refresh scheduling for data-driven widget instances (scripts, plugins,
//! web requests, Bluetooth, Reminders): one [`Schedule`] per instance.
//!
//! * Shown on screen: refresh every `interval`.
//! * Panel open on another page, or closed for less than [`PAUSE_AFTER`]:
//!   refresh at most every [`HIDDEN_MIN`] (or `interval` if longer), so the
//!   data is current when the page is opened.
//! * Closed for longer: paused, except instances that feed a live activity
//!   on the closed notch. Opening the panel makes stale instances due
//!   right away.
//! * Failures back off exponentially (from `max(interval, 5 s)` up to
//!   [`MAX_BACKOFF`]); a success resets it.
//! * Never more than one run in flight; a manual refresh makes it due now.

/// Pause refreshing hidden instances after the panel was closed this long.
pub const PAUSE_AFTER: f64 = 120.0;
/// Hidden instances refresh at most this often.
pub const HIDDEN_MIN: f64 = 30.0;
/// Longest wait after repeated failures.
pub const MAX_BACKOFF: f64 = 600.0;
/// Shortest refresh interval anything gets.
pub const MIN_INTERVAL: f64 = 1.0;

/// Where an instance is, from the scheduler's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Visibility {
    /// Its card is on screen (panel open, Home view, its page).
    pub shown: bool,
    /// The panel is open (any view / page).
    pub panel_open: bool,
    /// Seconds since the panel closed (0 while open).
    pub closed_for: f64,
    /// Feeds a live activity on the closed notch: never paused.
    pub live: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub interval: f64,
    pub in_flight: bool,
    /// When the last run started / finished (seconds, any clock).
    pub last_start: Option<f64>,
    pub last_finish: Option<f64>,
    pub last_ok: Option<f64>,
    /// Consecutive failures.
    pub failures: u32,
    forced: bool,
}

impl Schedule {
    pub fn new(interval: f64) -> Self {
        Schedule { interval: interval.max(MIN_INTERVAL), in_flight: false, last_start: None, last_finish: None, last_ok: None, failures: 0, forced: false }
    }

    /// Change the interval (takes effect from the last run).
    pub fn set_interval(&mut self, interval: f64) {
        self.interval = interval.max(MIN_INTERVAL);
    }

    /// Run as soon as possible (manual refresh, settings changed).
    pub fn force(&mut self) {
        self.forced = true;
    }

    pub fn paused(&self, v: &Visibility) -> bool {
        !v.live && !v.panel_open && v.closed_for >= PAUSE_AFTER
    }

    /// Delay after `failures` consecutive failures.
    pub fn backoff(&self) -> f64 {
        if self.failures == 0 {
            return self.interval;
        }
        let base = self.interval.max(5.0);
        let cap = MAX_BACKOFF.max(self.interval);
        (base * 2f64.powi(self.failures.min(16) as i32 - 1)).min(cap)
    }

    /// When the next run is due, or `None` while running / paused.
    pub fn due_at(&self, v: &Visibility) -> Option<f64> {
        if self.in_flight {
            return None;
        }
        if self.forced {
            return Some(f64::NEG_INFINITY);
        }
        if self.paused(v) {
            return None;
        }
        let Some(fin) = self.last_finish else { return Some(f64::NEG_INFINITY) };
        let wait = if self.failures > 0 {
            self.backoff()
        } else if v.shown || v.live {
            self.interval
        } else {
            self.interval.max(HIDDEN_MIN)
        };
        Some(fin + wait)
    }

    pub fn due(&self, now: f64, v: &Visibility) -> bool {
        self.due_at(v).is_some_and(|t| now >= t)
    }

    pub fn start(&mut self, now: f64) {
        self.in_flight = true;
        self.forced = false;
        self.last_start = Some(now);
    }

    pub fn finish(&mut self, now: f64, ok: bool) {
        self.in_flight = false;
        self.last_finish = Some(now);
        if ok {
            self.failures = 0;
            self.last_ok = Some(now);
        } else {
            self.failures = self.failures.saturating_add(1);
        }
    }

    /// Data older than one interval (shown as stale / refetched on open).
    pub fn stale(&self, now: f64) -> bool {
        self.last_ok.is_none_or(|t| now - t > self.interval)
    }
}

/// Seconds until the earliest due schedule (for the runner's sleep),
/// clamped to `[0.05, max]`.
pub fn next_wake<'a>(it: impl IntoIterator<Item = (&'a Schedule, Visibility)>, now: f64, max: f64) -> f64 {
    let mut best = max;
    for (s, v) in it {
        if let Some(t) = s.due_at(&v) {
            best = best.min(t - now);
        }
    }
    best.clamp(0.05, max.max(0.05))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown() -> Visibility {
        Visibility { shown: true, panel_open: true, closed_for: 0.0, live: false }
    }

    #[test]
    fn runs_at_interval_while_shown() {
        let mut s = Schedule::new(10.0);
        let v = shown();
        assert!(s.due(0.0, &v), "first run is immediate");
        s.start(0.0);
        assert!(!s.due(100.0, &v), "one at a time");
        s.finish(1.0, true);
        assert!(!s.due(10.9, &v));
        assert!(s.due(11.0, &v));
        assert!(!s.stale(5.0));
        assert!(s.stale(12.0));
        assert_eq!(Schedule::new(0.1).interval, MIN_INTERVAL);
    }

    #[test]
    fn hidden_and_paused() {
        let mut s = Schedule::new(2.0);
        s.start(0.0);
        s.finish(0.0, true);
        let hidden = Visibility { shown: false, panel_open: true, ..Default::default() };
        assert!(!s.due(10.0, &hidden));
        assert!(s.due(HIDDEN_MIN, &hidden));
        let recently_closed = Visibility { closed_for: 30.0, ..Default::default() };
        assert!(s.due(HIDDEN_MIN, &recently_closed));
        let long_closed = Visibility { closed_for: PAUSE_AFTER + 1.0, ..Default::default() };
        assert!(s.paused(&long_closed));
        assert!(!s.due(1e9, &long_closed));
        assert_eq!(s.due_at(&long_closed), None);
        // Live activities keep updating at their interval.
        let live = Visibility { live: true, ..long_closed };
        assert!(s.due(2.0, &live));
        // A long interval is respected when hidden too.
        let mut slow = Schedule::new(300.0);
        slow.start(0.0);
        slow.finish(0.0, true);
        assert!(!slow.due(100.0, &hidden) && slow.due(300.0, &hidden));
        // Reopening after a pause: due immediately (it's long overdue).
        assert!(s.due(1000.0, &shown()));
    }

    #[test]
    fn exponential_backoff_and_reset() {
        let mut s = Schedule::new(1.0);
        let v = shown();
        let mut t = 0.0;
        let mut waits = vec![];
        for _ in 0..10 {
            s.start(t);
            s.finish(t, false);
            let due = s.due_at(&v).unwrap();
            waits.push(due - t);
            t = due;
        }
        assert_eq!(&waits[..5], &[5.0, 10.0, 20.0, 40.0, 80.0]);
        assert_eq!(*waits.last().unwrap(), MAX_BACKOFF);
        s.start(t);
        s.finish(t, true);
        assert_eq!(s.failures, 0);
        assert_eq!(s.due_at(&v).unwrap() - t, 1.0);
        // Long intervals back off from the interval and cap at it.
        let mut h = Schedule::new(3600.0);
        h.start(0.0);
        h.finish(0.0, false);
        assert_eq!(h.backoff(), 3600.0);
    }

    #[test]
    fn forcing_and_wake() {
        let mut s = Schedule::new(60.0);
        s.start(0.0);
        s.finish(0.0, true);
        let paused = Visibility { closed_for: 1e6, ..Default::default() };
        assert!(!s.due(1.0, &shown()));
        s.force();
        assert!(s.due(1.0, &paused), "a manual refresh runs even when paused");
        s.start(1.0);
        assert!(!s.due(2.0, &shown()));
        s.finish(2.0, true);
        let a = Schedule::new(5.0);
        let mut b = Schedule::new(5.0);
        b.start(0.0);
        b.finish(0.0, true);
        assert_eq!(next_wake([(&b, shown())], 1.0, 10.0), 4.0);
        assert_eq!(next_wake([(&b, shown()), (&a, shown())], 1.0, 10.0), 0.05);
        assert_eq!(next_wake([(&b, paused)], 1.0, 10.0), 10.0);
    }
}
