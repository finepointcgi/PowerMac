//! Timer (countdown) and stopwatch: a small state machine on a monotonic
//! clock (seconds), independent of the UI.

/// Quick presets, in seconds.
pub const PRESETS: [u32; 3] = [5 * 60, 10 * 60, 25 * 60];
/// How long the "time's up" state lasts before it clears by itself.
pub const FINISHED_LINGER: f64 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimerState {
    Idle,
    /// Counting down to `ends_at`.
    Running { total: f64, ends_at: f64 },
    Paused { total: f64, remaining: f64 },
    /// Reached zero at `at`.
    Finished { total: f64, at: f64 },
    /// Counting up from `started_at` (plus time banked before a pause).
    Stopwatch { started_at: f64, banked: f64 },
    StopwatchPaused { elapsed: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerEvent {
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timer {
    pub state: TimerState,
}

impl Default for Timer {
    fn default() -> Self {
        Timer { state: TimerState::Idle }
    }
}

impl Timer {
    pub fn start(&mut self, secs: f64, now: f64) {
        let total = secs.max(1.0);
        self.state = TimerState::Running { total, ends_at: now + total };
    }

    pub fn start_stopwatch(&mut self, now: f64) {
        self.state = TimerState::Stopwatch { started_at: now, banked: 0.0 };
    }

    /// Pause a running timer / stopwatch, or resume a paused one.
    pub fn toggle_pause(&mut self, now: f64) {
        self.state = match self.state {
            TimerState::Running { total, ends_at } => TimerState::Paused { total, remaining: (ends_at - now).max(0.0) },
            TimerState::Paused { total, remaining } => TimerState::Running { total, ends_at: now + remaining },
            TimerState::Stopwatch { started_at, banked } => TimerState::StopwatchPaused { elapsed: banked + (now - started_at).max(0.0) },
            TimerState::StopwatchPaused { elapsed } => TimerState::Stopwatch { started_at: now, banked: elapsed },
            s => s,
        };
    }

    /// Add time to a running / paused / just finished countdown.
    pub fn add(&mut self, secs: f64, now: f64) {
        self.state = match self.state {
            TimerState::Running { total, ends_at } => TimerState::Running { total: total + secs, ends_at: ends_at + secs },
            TimerState::Paused { total, remaining } => TimerState::Paused { total: total + secs, remaining: remaining + secs },
            TimerState::Finished { .. } => TimerState::Running { total: secs, ends_at: now + secs },
            s => s,
        };
    }

    pub fn reset(&mut self) {
        self.state = TimerState::Idle;
    }

    /// Advance; returns `Finished` exactly once when a countdown hits zero.
    pub fn tick(&mut self, now: f64) -> Option<TimerEvent> {
        match self.state {
            TimerState::Running { total, ends_at } if now >= ends_at => {
                self.state = TimerState::Finished { total, at: ends_at };
                Some(TimerEvent::Finished)
            }
            TimerState::Finished { at, .. } if now - at > FINISHED_LINGER => {
                self.state = TimerState::Idle;
                None
            }
            _ => None,
        }
    }

    /// Seconds left of a countdown.
    pub fn remaining(&self, now: f64) -> Option<f64> {
        match self.state {
            TimerState::Running { ends_at, .. } => Some((ends_at - now).max(0.0)),
            TimerState::Paused { remaining, .. } => Some(remaining),
            TimerState::Finished { .. } => Some(0.0),
            _ => None,
        }
    }

    /// Seconds on the stopwatch.
    pub fn elapsed(&self, now: f64) -> Option<f64> {
        match self.state {
            TimerState::Stopwatch { started_at, banked } => Some(banked + (now - started_at).max(0.0)),
            TimerState::StopwatchPaused { elapsed } => Some(elapsed),
            _ => None,
        }
    }

    /// Fraction of a countdown still left, 1 → 0.
    pub fn fraction_left(&self, now: f64) -> Option<f64> {
        let total = match self.state {
            TimerState::Running { total, .. } | TimerState::Paused { total, .. } | TimerState::Finished { total, .. } => total,
            _ => return None,
        };
        self.remaining(now).map(|r| if total > 0.0 { (r / total).clamp(0.0, 1.0) } else { 0.0 })
    }

    pub fn is_idle(&self) -> bool {
        self.state == TimerState::Idle
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state, TimerState::Paused { .. } | TimerState::StopwatchPaused { .. })
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, TimerState::Finished { .. })
    }

    pub fn is_stopwatch(&self) -> bool {
        matches!(self.state, TimerState::Stopwatch { .. } | TimerState::StopwatchPaused { .. })
    }

    /// Something worth showing next to the collapsed notch.
    pub fn is_active(&self) -> bool {
        !self.is_idle()
    }

    /// The big readout: countdown "24:13" (rounded up, so it shows 0:00
    /// only at the end), stopwatch "1:05.3" under an hour.
    pub fn readout(&self, now: f64) -> String {
        if let Some(r) = self.remaining(now) {
            return super::format_clock(r.ceil());
        }
        if let Some(e) = self.elapsed(now) {
            if e < 3600.0 {
                let tenths = ((e * 10.0).floor() as u64) % 10;
                return format!("{}.{tenths}", super::format_clock(e));
            }
            return super::format_clock(e);
        }
        String::new()
    }

    /// Readout without tenths (for the collapsed live activity).
    pub fn short_readout(&self, now: f64) -> String {
        match (self.remaining(now), self.elapsed(now)) {
            (Some(r), _) => super::format_clock(r.ceil()),
            (_, Some(e)) => super::format_clock(e),
            _ => String::new(),
        }
    }
}

/// "5m", "1h", "1h30".
pub fn preset_label(secs: u32) -> String {
    let m = secs / 60;
    if m >= 60 {
        if m.is_multiple_of(60) {
            format!("{}h", m / 60)
        } else {
            format!("{}h{:02}", m / 60, m % 60)
        }
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_runs_pauses_and_finishes_once() {
        let mut t = Timer::default();
        assert!(t.is_idle() && t.remaining(0.0).is_none());
        t.start(300.0, 10.0);
        assert_eq!(t.remaining(70.0), Some(240.0));
        assert_eq!(t.readout(70.0), "4:00");
        assert_eq!(t.readout(70.5), "4:00");
        assert_eq!(t.readout(71.0), "3:59");
        assert!((t.fraction_left(160.0).unwrap() - 0.5).abs() < 1e-9);
        t.toggle_pause(160.0);
        assert!(t.is_paused());
        assert_eq!(t.remaining(1000.0), Some(150.0));
        assert_eq!(t.tick(1000.0), None);
        t.toggle_pause(1000.0);
        assert_eq!(t.remaining(1100.0), Some(50.0));
        assert_eq!(t.tick(1149.9), None);
        assert_eq!(t.tick(1150.0), Some(TimerEvent::Finished));
        assert!(t.is_finished());
        assert_eq!(t.tick(1151.0), None);
        assert_eq!(t.readout(1151.0), "0:00");
        assert_eq!(t.fraction_left(1151.0), Some(0.0));
        // Finished clears itself after a while.
        assert_eq!(t.tick(1150.0 + FINISHED_LINGER + 1.0), None);
        assert!(t.is_idle());
    }

    #[test]
    fn add_time_and_snooze() {
        let mut t = Timer::default();
        t.start(60.0, 0.0);
        t.add(60.0, 30.0);
        assert_eq!(t.remaining(30.0), Some(90.0));
        assert!((t.fraction_left(30.0).unwrap() - 0.75).abs() < 1e-9);
        t.tick(200.0);
        assert!(t.is_finished());
        t.add(60.0, 200.0);
        assert_eq!(t.remaining(230.0), Some(30.0));
        t.reset();
        assert!(t.is_idle());
        // Adding to an idle timer does nothing.
        t.add(60.0, 0.0);
        assert!(t.is_idle());
    }

    #[test]
    fn stopwatch_counts_up_across_pauses() {
        let mut t = Timer::default();
        t.start_stopwatch(100.0);
        assert!(t.is_stopwatch() && t.is_active());
        assert_eq!(t.elapsed(105.25), Some(5.25));
        assert_eq!(t.readout(105.25), "0:05.2");
        t.toggle_pause(110.0);
        assert_eq!(t.elapsed(500.0), Some(10.0));
        t.toggle_pause(500.0);
        assert_eq!(t.elapsed(502.0), Some(12.0));
        assert_eq!(t.short_readout(502.0), "0:12");
        assert_eq!(t.tick(1e6), None);
        assert!(t.remaining(0.0).is_none() && t.fraction_left(0.0).is_none());
        t.start_stopwatch(0.0);
        assert_eq!(t.readout(3725.0), "1:02:05");
    }

    #[test]
    fn preset_labels() {
        assert_eq!(PRESETS.map(preset_label), ["5m".to_string(), "10m".into(), "25m".into()]);
        assert_eq!(preset_label(3600), "1h");
        assert_eq!(preset_label(5400), "1h30");
    }
}
