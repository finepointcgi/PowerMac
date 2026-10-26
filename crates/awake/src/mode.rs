//! Configuration types: [`AwakeMode`], [`AwakeConfig`], status/end-reason types,
//! and a compatibility shim for the PowerToys `settings.json` "properties" shape.

use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

/// How the machine should be kept awake.
///
/// JSON shape (adjacently tagged):
/// `{"type":"passive"}`, `{"type":"indefinite"}`,
/// `{"type":"timed","value":3600}` (seconds, may be fractional),
/// `{"type":"expirable","value":"2026-10-25T18:00:00-07:00"}` (RFC 3339).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AwakeMode {
    /// No assertion is held; normal power plan applies.
    #[default]
    Passive,
    /// Stay awake until told otherwise.
    Indefinite,
    /// Stay awake for the given duration, starting when the mode is set.
    Timed(#[serde(with = "duration_secs")] Duration),
    /// Stay awake until the given local date/time.
    Expirable(DateTime<Local>),
}

impl AwakeMode {
    pub fn is_passive(&self) -> bool {
        matches!(self, AwakeMode::Passive)
    }
}

/// Full Awake configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AwakeConfig {
    pub mode: AwakeMode,
    /// Also keep the display from sleeping (PowerToys "Keep screen on").
    pub keep_display_on: bool,
    /// If set, revert to passive as soon as this process exits (PowerToys `--pid`).
    /// Combines with any non-passive mode; whichever ends first wins.
    pub bound_pid: Option<u32>,
}

impl AwakeConfig {
    pub fn passive() -> Self {
        Self::default()
    }
    pub fn indefinite(keep_display_on: bool) -> Self {
        Self { mode: AwakeMode::Indefinite, keep_display_on, bound_pid: None }
    }
    pub fn timed(d: Duration, keep_display_on: bool) -> Self {
        Self { mode: AwakeMode::Timed(d), keep_display_on, bound_pid: None }
    }
    pub fn expirable(at: DateTime<Local>, keep_display_on: bool) -> Self {
        Self { mode: AwakeMode::Expirable(at), keep_display_on, bound_pid: None }
    }
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.bound_pid = Some(pid);
        self
    }
}

/// What the power backend is asked to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerRequest {
    /// Release everything (passive).
    Release,
    /// Prevent idle system sleep.
    System,
    /// Prevent idle system sleep and idle display sleep.
    SystemAndDisplay,
}

/// Why an active mode ended and reverted to passive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// `stop()` or `set(passive)` was called.
    Stopped,
    /// A `Timed` duration elapsed.
    TimerElapsed,
    /// An `Expirable` date/time was reached.
    Expired,
    /// The bound process exited.
    ProcessExited,
}

/// Snapshot of the controller state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AwakeStatus {
    /// The mode currently in effect (Passive once a mode ended).
    pub mode: AwakeMode,
    pub keep_display_on: bool,
    pub bound_pid: Option<u32>,
    /// Whether a power assertion is currently held.
    pub active: bool,
    /// When the mode started (None when passive).
    pub started_at: Option<DateTime<Local>>,
    /// Deadline for Timed/Expirable modes.
    pub expires_at: Option<DateTime<Local>>,
    /// Time left until `expires_at` (None for Passive/Indefinite).
    #[serde(with = "opt_duration_secs")]
    pub remaining: Option<Duration>,
    /// Why the last active mode ended, if it did.
    pub last_end: Option<EndReason>,
}

/// Errors from configuring Awake.
#[derive(Debug, thiserror::Error)]
pub enum AwakeError {
    #[error("expiration time {0} is not in the future")]
    ExpiryInPast(DateTime<Local>),
    #[error("duration is too large")]
    DurationOverflow,
    #[error("process {0} is not running")]
    ProcessNotFound(u32),
    #[error("power backend error: {0}")]
    Backend(String),
    #[error("awake worker thread is not running")]
    WorkerGone,
    #[error("could not parse time {0:?}: expected RFC 3339 or HH:MM[:SS]")]
    BadTime(String),
}

/// Parse a `--expire-at` value relative to `now`.
///
/// Accepts RFC 3339 (`2026-10-25T18:30:00-07:00`), `YYYY-MM-DD HH:MM[:SS]`
/// (local), or a bare `HH:MM[:SS]` (local; today, or tomorrow if that time
/// has already passed today).
pub fn parse_expire_at(s: &str, now: DateTime<Local>) -> Result<DateTime<Local>, AwakeError> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Local));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .ok_or_else(|| AwakeError::BadTime(s.to_string()));
        }
    }
    for fmt in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, fmt) {
            let today = now.date_naive().and_time(t);
            let mut dt = Local
                .from_local_datetime(&today)
                .earliest()
                .ok_or_else(|| AwakeError::BadTime(s.to_string()))?;
            if dt <= now {
                let tomorrow = today + chrono::Duration::days(1);
                dt = Local
                    .from_local_datetime(&tomorrow)
                    .earliest()
                    .ok_or_else(|| AwakeError::BadTime(s.to_string()))?;
            }
            return Ok(dt);
        }
    }
    Err(AwakeError::BadTime(s.to_string()))
}

/// The `properties` object of PowerToys' Awake `settings.json`:
/// `{"keepDisplayOn":false,"mode":2,"intervalHours":1,"intervalMinutes":30,
///   "expirationDateTime":"2026-10-25T18:00:00-07:00","customTrayTimes":{}}`
/// where mode is 0=PASSIVE, 1=INDEFINITE, 2=TIMED, 3=EXPIRABLE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerToysAwakeProperties {
    #[serde(default)]
    pub keep_display_on: bool,
    #[serde(default)]
    pub mode: u8,
    #[serde(default)]
    pub interval_hours: u32,
    #[serde(default)]
    pub interval_minutes: u32,
    #[serde(default = "now_fixed")]
    pub expiration_date_time: DateTime<FixedOffset>,
    #[serde(default)]
    pub custom_tray_times: std::collections::BTreeMap<String, u32>,
}

fn now_fixed() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

impl Default for PowerToysAwakeProperties {
    fn default() -> Self {
        Self {
            keep_display_on: false,
            mode: 0,
            interval_hours: 0,
            interval_minutes: 1,
            expiration_date_time: now_fixed(),
            custom_tray_times: Default::default(),
        }
    }
}

impl PowerToysAwakeProperties {
    pub fn to_config(&self) -> AwakeConfig {
        let mode = match self.mode {
            1 => AwakeMode::Indefinite,
            2 => AwakeMode::Timed(Duration::from_secs(
                u64::from(self.interval_hours) * 3600 + u64::from(self.interval_minutes) * 60,
            )),
            3 => AwakeMode::Expirable(self.expiration_date_time.with_timezone(&Local)),
            _ => AwakeMode::Passive,
        };
        AwakeConfig { mode, keep_display_on: self.keep_display_on, bound_pid: None }
    }

    /// Lossy for sub-minute Timed durations (PowerToys stores hours+minutes).
    pub fn from_config(cfg: &AwakeConfig) -> Self {
        let mut p = Self { keep_display_on: cfg.keep_display_on, ..Self::default() };
        match cfg.mode {
            AwakeMode::Passive => p.mode = 0,
            AwakeMode::Indefinite => p.mode = 1,
            AwakeMode::Timed(d) => {
                let mins = d.as_secs() / 60;
                p.mode = 2;
                p.interval_hours = u32::try_from(mins / 60).unwrap_or(u32::MAX);
                p.interval_minutes = (mins % 60) as u32;
            }
            AwakeMode::Expirable(at) => {
                p.mode = 3;
                p.expiration_date_time = at.fixed_offset();
            }
        }
        p
    }
}

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        if d.subsec_nanos() == 0 {
            s.serialize_u64(d.as_secs())
        } else {
            s.serialize_f64(d.as_secs_f64())
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let v = f64::deserialize(d)?;
        Duration::try_from_secs_f64(v).map_err(serde::de::Error::custom)
    }
}

mod opt_duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_some(&d.as_secs_f64()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let v = Option::<f64>::deserialize(d)?;
        v.map(|v| Duration::try_from_secs_f64(v).map_err(serde::de::Error::custom)).transpose()
    }
}
