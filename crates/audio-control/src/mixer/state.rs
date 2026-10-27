//! What the UI reads: strip / route status and meters.

use crate::CapturePermission;
use serde::{Deserialize, Serialize};

/// Why a strip is or isn't running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum StripState {
    /// Running in the mixer.
    Active,
    /// Turned off (mixer / route / module disabled).
    #[default]
    Off,
    /// Will start by itself when the app starts or the device connects.
    Waiting { reason: String },
    /// Can't run as configured.
    Unavailable { reason: String },
}

impl StripState {
    pub fn is_active(&self) -> bool {
        matches!(self, StripState::Active)
    }

    pub(crate) fn waiting(r: impl Into<String>) -> Self {
        StripState::Waiting { reason: r.into() }
    }

    pub(crate) fn unavailable(r: impl Into<String>) -> Self {
        StripState::Unavailable { reason: r.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum StripKind {
    #[default]
    App,
    Input,
    Loopback,
    Output,
    /// The implicit source of an app route.
    RouteSource,
    /// The implicit output of an app route (one per target device).
    RouteOutput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StripStatus {
    /// The config id (`MixSource::id` / `MixOutput::id`), or
    /// `route:<bundle id>` / `route-out:<device uid>` for route strips.
    pub id: String,
    pub name: String,
    pub kind: StripKind,
    pub state: StripState,
    /// The resolved device (devices and outputs).
    pub device_uid: Option<String>,
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    /// The device it resolved to, if connected.
    pub uid: Option<String>,
    pub name: String,
    pub available: bool,
    /// Why not, when not available.
    pub reason: Option<String>,
}

/// One `Config::app_routes` entry as it is right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouteStatus {
    pub bundle_id: String,
    pub name: String,
    pub enabled: bool,
    /// The app's audio is being routed right now.
    pub active: bool,
    pub keep_default: bool,
    pub targets: Vec<RouteTarget>,
    pub state: StripState,
    /// Caveats while active (e.g. what the platform can't do).
    pub message: Option<String>,
}

/// The mixer / router as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerState {
    /// The platform can run the mixer at all (macOS: always for devices;
    /// app sources and routes need macOS 14.4 — see `app_sources_supported`).
    pub supported: bool,
    pub app_sources_supported: bool,
    /// An IOProc is running.
    pub running: bool,
    pub sample_rate: Option<f64>,
    /// The device whose clock the mixer follows (the first active output).
    pub clock_device: Option<String>,
    pub capture_permission: CapturePermission,
    pub sources: Vec<StripStatus>,
    pub outputs: Vec<StripStatus>,
    pub routes: Vec<RouteStatus>,
    /// Problems and warnings (feedback loops, failed starts, …).
    pub errors: Vec<String>,
    /// IOProc callbacks so far (diagnostics).
    pub callbacks: u64,
}

impl Default for MixerState {
    fn default() -> Self {
        MixerState {
            supported: false,
            app_sources_supported: false,
            running: false,
            sample_rate: None,
            clock_device: None,
            capture_permission: CapturePermission::NotApplicable,
            sources: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            errors: Vec::new(),
            callbacks: 0,
        }
    }
}

/// One strip's meter (linear amplitudes; use [`MeterReading::peak_db`] for
/// dBFS). Peak falls back at ≈17 dB/s, RMS integrates over ≈300 ms; poll at
/// ~20 Hz ([`METER_INTERVAL`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MeterReading {
    pub id: String,
    pub peak: [f32; 2],
    pub rms: [f32; 2],
    /// Limiter gain reduction in dB (0 = none, positive = reducing).
    pub limiter_reduction_db: f32,
    /// Blocks that reached full scale so far.
    pub clips: u32,
}

impl MeterReading {
    pub fn peak_db(&self) -> [f32; 2] {
        self.peak.map(crate::mixer::dsp::gain_to_db)
    }

    pub fn rms_db(&self) -> [f32; 2] {
        self.rms.map(crate::mixer::dsp::gain_to_db)
    }
}

/// Suggested meter polling interval (20 Hz).
pub const METER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Meters {
    pub sources: Vec<MeterReading>,
    pub outputs: Vec<MeterReading>,
}
