//! Platform-independent Awake logic: deadline calculation and state
//! transitions, parameterised over a [`PowerBackend`], a [`ProcessProbe`] and
//! a time source so it can be unit-tested with fakes.

use std::time::Duration;

use chrono::{DateTime, Local};

use crate::mode::{AwakeConfig, AwakeError, AwakeMode, AwakeStatus, EndReason, PowerRequest};

/// Source of "now". The real implementation is [`SystemClock`].
pub trait Clock: Send + 'static {
    fn now(&self) -> DateTime<Local>;
}

/// Wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }
}

/// Holds / releases the OS "stay awake" request.
///
/// Implementations are *not* required to be `Send`: the controller creates the
/// backend on its dedicated worker thread and only ever calls it from there
/// (required on Windows, where `SetThreadExecutionState` is per-thread).
pub trait PowerBackend {
    /// Replace whatever is currently held with `req`. `Release` drops everything.
    fn apply(&mut self, req: PowerRequest) -> Result<(), AwakeError>;
}

/// Checks whether a bound process is still alive.
pub trait ProcessProbe {
    /// Start watching `pid`. Returns false if it is not running.
    fn bind(&mut self, pid: u32) -> bool;
    /// Whether the bound process is still alive (true when nothing is bound).
    fn is_alive(&mut self) -> bool;
    /// Stop watching.
    fn unbind(&mut self);
}

/// Compute the absolute deadline for a mode set at `now`.
/// `Ok(None)` means "no deadline" (Passive / Indefinite).
pub fn compute_deadline(
    mode: &AwakeMode,
    now: DateTime<Local>,
) -> Result<Option<DateTime<Local>>, AwakeError> {
    match *mode {
        AwakeMode::Passive | AwakeMode::Indefinite => Ok(None),
        AwakeMode::Timed(d) => {
            let cd = chrono::Duration::from_std(d).map_err(|_| AwakeError::DurationOverflow)?;
            now.checked_add_signed(cd).map(Some).ok_or(AwakeError::DurationOverflow)
        }
        AwakeMode::Expirable(at) => {
            if at <= now {
                Err(AwakeError::ExpiryInPast(at))
            } else {
                Ok(Some(at))
            }
        }
    }
}

/// Time left until `deadline`, saturating at zero.
pub fn remaining(deadline: DateTime<Local>, now: DateTime<Local>) -> Duration {
    (deadline - now).to_std().unwrap_or(Duration::ZERO)
}

/// The power request a config needs.
pub fn request_for(cfg: &AwakeConfig) -> PowerRequest {
    match (cfg.mode.is_passive(), cfg.keep_display_on) {
        (true, _) => PowerRequest::Release,
        (false, false) => PowerRequest::System,
        (false, true) => PowerRequest::SystemAndDisplay,
    }
}

/// State machine driving a backend. Not thread-safe by itself; the
/// [`crate::Awake`] controller owns one on its worker thread.
pub struct Engine<B: PowerBackend, P: ProcessProbe> {
    backend: B,
    probe: P,
    config: AwakeConfig,
    started_at: Option<DateTime<Local>>,
    deadline: Option<DateTime<Local>>,
    held: PowerRequest,
    last_end: Option<EndReason>,
    /// Max time between ticks while a pid is bound.
    pub pid_poll_interval: Duration,
}

impl<B: PowerBackend, P: ProcessProbe> Engine<B, P> {
    pub fn new(backend: B, probe: P) -> Self {
        Self {
            backend,
            probe,
            config: AwakeConfig::passive(),
            started_at: None,
            deadline: None,
            held: PowerRequest::Release,
            last_end: None,
            pid_poll_interval: Duration::from_secs(1),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
    pub fn probe_mut(&mut self) -> &mut P {
        &mut self.probe
    }

    pub fn is_active(&self) -> bool {
        !self.config.mode.is_passive()
    }

    /// Apply a new configuration, replacing the previous one.
    ///
    /// On error the engine is left passive (any previous assertion released).
    pub fn set(&mut self, config: AwakeConfig, now: DateTime<Local>) -> Result<(), AwakeError> {
        if config.mode.is_passive() {
            self.end(EndReason::Stopped);
            // Remember the display preference even when passive.
            self.config.keep_display_on = config.keep_display_on;
            return Ok(());
        }

        let deadline = match compute_deadline(&config.mode, now) {
            Ok(d) => d,
            Err(e) => {
                self.end(EndReason::Stopped);
                return Err(e);
            }
        };

        self.probe.unbind();
        if let Some(pid) = config.bound_pid {
            if !self.probe.bind(pid) {
                self.end(EndReason::Stopped);
                return Err(AwakeError::ProcessNotFound(pid));
            }
        }

        let req = request_for(&config);
        if let Err(e) = self.backend.apply(req) {
            self.probe.unbind();
            let _ = self.backend.apply(PowerRequest::Release);
            self.reset_state();
            self.held = PowerRequest::Release;
            return Err(e);
        }
        self.held = req;
        self.config = config;
        self.started_at = Some(now);
        self.deadline = deadline;
        self.last_end = None;
        log::info!("awake: mode set to {:?} (display={})", config.mode, config.keep_display_on);
        Ok(())
    }

    /// Revert to passive.
    pub fn stop(&mut self) {
        self.end(EndReason::Stopped);
    }

    /// Check deadline / bound process. Returns the reason if the mode ended now.
    pub fn tick(&mut self, now: DateTime<Local>) -> Option<EndReason> {
        if !self.is_active() {
            return None;
        }
        if let Some(dl) = self.deadline {
            if now >= dl {
                let reason = match self.config.mode {
                    AwakeMode::Timed(_) => EndReason::TimerElapsed,
                    _ => EndReason::Expired,
                };
                self.end(reason);
                return Some(reason);
            }
        }
        if self.config.bound_pid.is_some() && !self.probe.is_alive() {
            self.end(EndReason::ProcessExited);
            return Some(EndReason::ProcessExited);
        }
        None
    }

    /// How long the driver may sleep before the next `tick` is needed.
    /// `None` means "until the next command" (nothing to watch).
    pub fn next_wake(&self, now: DateTime<Local>) -> Option<Duration> {
        if !self.is_active() {
            return None;
        }
        let mut wait: Option<Duration> = self.deadline.map(|d| remaining(d, now));
        if self.config.bound_pid.is_some() {
            wait = Some(wait.map_or(self.pid_poll_interval, |w| w.min(self.pid_poll_interval)));
        }
        // Re-check wall-clock deadlines at least once a minute so clock jumps
        // and machine sleep don't overshoot by much.
        wait.map(|w| w.min(Duration::from_secs(60)))
    }

    pub fn status(&self, now: DateTime<Local>) -> AwakeStatus {
        AwakeStatus {
            mode: self.config.mode,
            keep_display_on: self.config.keep_display_on,
            bound_pid: if self.is_active() { self.config.bound_pid } else { None },
            active: self.held != PowerRequest::Release,
            started_at: self.started_at,
            expires_at: self.deadline,
            remaining: self.deadline.map(|d| remaining(d, now)),
            last_end: self.last_end,
        }
    }

    fn reset_state(&mut self) {
        let keep = self.config.keep_display_on;
        self.config = AwakeConfig { keep_display_on: keep, ..AwakeConfig::passive() };
        self.started_at = None;
        self.deadline = None;
    }

    fn end(&mut self, reason: EndReason) {
        let was_active = self.is_active();
        self.probe.unbind();
        if self.held != PowerRequest::Release {
            if let Err(e) = self.backend.apply(PowerRequest::Release) {
                log::warn!("awake: failed to release power request: {e}");
            }
            self.held = PowerRequest::Release;
        }
        self.reset_state();
        if was_active {
            log::info!("awake: reverted to passive ({reason:?})");
            self.last_end = Some(reason);
        }
    }
}

impl<B: PowerBackend, P: ProcessProbe> Drop for Engine<B, P> {
    fn drop(&mut self) {
        self.end(EndReason::Stopped);
    }
}
