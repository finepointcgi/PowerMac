//! Scroll Reverser: reverse the scroll direction per device class, in the
//! spirit of the macOS app "Scroll Reverser". The usual setup keeps natural
//! scrolling on the trackpad and reverses it for a mouse wheel; any
//! combination of {mouse, trackpad} x {vertical, horizontal} works.
//!
//! ```no_run
//! use scroll_reverser::{Config, ScrollReverser};
//!
//! let mut handle = ScrollReverser::start(Config::default())?;
//! handle.update(Config { reverse_mouse_horizontal: true, ..Config::default() });
//! handle.stop();
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Direction is relative
//!
//! Reversal is relative to the OS behaviour. On macOS the global "natural
//! scrolling" setting (`com.apple.swipescrolldirection`, see
//! [`system_natural_scrolling`]) applies to every device. It is a sign flip,
//! as is ours, so the result is the system direction, inverted for the
//! classes/axes enabled here; it does not matter where in the event pipeline
//! the system flip happens (observed: an event posted at the HID tap still has
//! its original sign at the HID tap, and is natural-flipped by the time a
//! session tap sees it). With natural scrolling ON (the macOS default) the
//! default [`Config`] gives natural trackpad + classic mouse wheel. With it
//! OFF, the same config gives classic trackpad + "natural" mouse; flip the
//! flags accordingly.
//!
//! # Platforms
//!
//! * **macOS:** an active `CGEventTap` (HID level, falling back to session
//!   level) on its own `CFRunLoop` thread that negates the scroll fields of
//!   `kCGEventScrollWheel` events in place. Needs Accessibility permission
//!   ([`needs_permission`], [`request_permission`]). Device classification:
//!   see [`MacClassifier`]. The Magic Mouse produces continuous, phased
//!   scroll events exactly like a trackpad, so it is classified as
//!   [`Device::Trackpad`] (the same limitation the original app has without
//!   its touch-counting heuristics).
//! * **Windows:** a `WH_MOUSE_LL` hook on its own message-loop thread. LL hooks
//!   cannot modify events, so a wheel event that must be reversed is swallowed
//!   and re-injected with `SendInput` and a negated delta, tagged with
//!   [`SELF_INJECTED_TAG`]. LL hooks carry no per-event device information,
//!   so by default every wheel event counts as [`Device::Mouse`] and only the
//!   `reverse_mouse_*` flags apply. [`Config::windows_touchpad_heuristic`]
//!   optionally treats non-multiple-of-120 deltas as touchpad input (precision
//!   touchpads send small deltas; so do some free-spinning/high-res mice).
//!
//! Events injected by `pm-input` (Mouse Without Borders) carry
//! [`PM_INPUT_INJECTED_TAG`] and are always left alone, on both platforms.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(test)]
mod tests;

/// `pm-input`'s injection tag ("PMMW"): `kCGEventSourceUserData` on macOS,
/// `dwExtraInfo` on Windows. Scroll events carrying it are never touched.
pub const PM_INPUT_INJECTED_TAG: usize = 0x504D_4D57;

/// Tag ("SRVS") put in `dwExtraInfo` of the wheel events we re-inject on
/// Windows, so our own hook (and anyone else who cares) can recognise them.
/// It is deliberately distinct from [`PM_INPUT_INJECTED_TAG`]: Mouse Without
/// Borders skips events with that tag, and it must still see reversed scrolls.
pub const SELF_INJECTED_TAG: usize = 0x5352_5653;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// User settings. Live-updatable through [`Handle::update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch; when false every event passes through untouched.
    pub enabled: bool,
    pub reverse_mouse_vertical: bool,
    pub reverse_mouse_horizontal: bool,
    pub reverse_trackpad_vertical: bool,
    pub reverse_trackpad_horizontal: bool,
    /// Windows only: classify wheel deltas that are not a multiple of
    /// `WHEEL_DELTA` (120) as touchpad input. Off by default because
    /// high-resolution mice also produce such deltas. Ignored on macOS.
    pub windows_touchpad_heuristic: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            reverse_mouse_vertical: true,
            reverse_mouse_horizontal: false,
            reverse_trackpad_vertical: false,
            reverse_trackpad_horizontal: false,
            windows_touchpad_heuristic: false,
        }
    }
}

const BIT_ENABLED: u32 = 1 << 0;
const BIT_MOUSE_V: u32 = 1 << 1;
const BIT_MOUSE_H: u32 = 1 << 2;
const BIT_PAD_V: u32 = 1 << 3;
const BIT_PAD_H: u32 = 1 << 4;
const BIT_WIN_HEUR: u32 = 1 << 5;

impl Config {
    /// Pack into a bitset (for lock-free sharing with the hook thread).
    pub fn to_bits(self) -> u32 {
        let mut b = 0;
        for (on, bit) in [
            (self.enabled, BIT_ENABLED),
            (self.reverse_mouse_vertical, BIT_MOUSE_V),
            (self.reverse_mouse_horizontal, BIT_MOUSE_H),
            (self.reverse_trackpad_vertical, BIT_PAD_V),
            (self.reverse_trackpad_horizontal, BIT_PAD_H),
            (self.windows_touchpad_heuristic, BIT_WIN_HEUR),
        ] {
            if on {
                b |= bit;
            }
        }
        b
    }

    pub fn from_bits(b: u32) -> Self {
        Self {
            enabled: b & BIT_ENABLED != 0,
            reverse_mouse_vertical: b & BIT_MOUSE_V != 0,
            reverse_mouse_horizontal: b & BIT_MOUSE_H != 0,
            reverse_trackpad_vertical: b & BIT_PAD_V != 0,
            reverse_trackpad_horizontal: b & BIT_PAD_H != 0,
            windows_touchpad_heuristic: b & BIT_WIN_HEUR != 0,
        }
    }

    /// Which axes to reverse for an event from `device`.
    pub fn reverse_for(&self, device: Device) -> Reverse {
        if !self.enabled {
            return Reverse::NONE;
        }
        match device {
            Device::Mouse => Reverse {
                vertical: self.reverse_mouse_vertical,
                horizontal: self.reverse_mouse_horizontal,
            },
            Device::Trackpad => Reverse {
                vertical: self.reverse_trackpad_vertical,
                horizontal: self.reverse_trackpad_horizontal,
            },
        }
    }
}

/// Config shared with the event thread: a single atomic word, so the tap
/// callback never blocks on a lock.
#[derive(Debug)]
pub(crate) struct SharedConfig(AtomicU32);

impl SharedConfig {
    pub(crate) fn new(c: Config) -> Self {
        Self(AtomicU32::new(c.to_bits()))
    }
    pub(crate) fn load(&self) -> Config {
        Config::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub(crate) fn store(&self, c: Config) {
        self.0.store(c.to_bits(), Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Classification + transform (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Device class of a scroll event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Device {
    /// Discrete, line-based wheel (classic mouse wheel).
    Mouse,
    /// Continuous, gesture-based scrolling: trackpads, and on macOS also the
    /// Magic Mouse's touch surface.
    Trackpad,
}

/// Axes to negate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reverse {
    pub vertical: bool,
    pub horizontal: bool,
}

impl Reverse {
    pub const NONE: Reverse = Reverse { vertical: false, horizontal: false };
    pub fn any(self) -> bool {
        self.vertical || self.horizontal
    }
}

/// `CGScrollPhase` values (`kCGScrollWheelEventScrollPhase`).
pub mod scroll_phase {
    pub const NONE: i64 = 0;
    pub const BEGAN: i64 = 1;
    pub const CHANGED: i64 = 2;
    pub const ENDED: i64 = 4;
    pub const CANCELLED: i64 = 8;
    pub const MAY_BEGIN: i64 = 128;
}

/// `CGMomentumScrollPhase` values (`kCGScrollWheelEventMomentumPhase`).
pub mod momentum_phase {
    pub const NONE: i64 = 0;
    pub const BEGIN: i64 = 1;
    pub const CONTINUE: i64 = 2;
    pub const END: i64 = 3;
}

/// The classification-relevant fields of a macOS scroll event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MacScrollFields {
    /// `kCGScrollWheelEventIsContinuous`
    pub is_continuous: bool,
    /// `kCGScrollWheelEventScrollPhase`
    pub scroll_phase: i64,
    /// `kCGScrollWheelEventMomentumPhase`
    pub momentum_phase: i64,
}

/// Classifies macOS scroll events the way Scroll Reverser does.
///
/// * Not continuous (line-based) => [`Device::Mouse`].
/// * Continuous with a scroll phase or a momentum phase => [`Device::Trackpad`]
///   (trackpad, Magic Mouse). Momentum events that follow a gesture carry a
///   momentum phase, so the whole fling keeps the gesture's class.
/// * Continuous with no phase at all is ambiguous: smooth-scrolling mouse
///   drivers produce these, but stragglers right after a gesture do too. They
///   count as trackpad within [`MacClassifier::GESTURE_GRACE_NS`] of the last
///   phased event, otherwise as mouse.
#[derive(Clone, Debug, Default)]
pub struct MacClassifier {
    last_phased_ns: Option<u64>,
}

impl MacClassifier {
    pub const GESTURE_GRACE_NS: u64 = 250_000_000;

    pub fn new() -> Self {
        Self::default()
    }

    /// `timestamp_ns` is the event's `CGEventGetTimestamp` (any monotonic ns).
    pub fn classify(&mut self, f: MacScrollFields, timestamp_ns: u64) -> Device {
        if !f.is_continuous {
            return Device::Mouse;
        }
        if f.scroll_phase != scroll_phase::NONE || f.momentum_phase != momentum_phase::NONE {
            self.last_phased_ns = Some(timestamp_ns);
            return Device::Trackpad;
        }
        match self.last_phased_ns {
            Some(t) if timestamp_ns.saturating_sub(t) <= Self::GESTURE_GRACE_NS => Device::Trackpad,
            _ => Device::Mouse,
        }
    }
}

/// The delta fields of a macOS scroll event. Index 0 = axis 1 (vertical),
/// index 1 = axis 2 (horizontal).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MacDeltas {
    /// `kCGScrollWheelEventDeltaAxis1/2` (lines, integer)
    pub delta: [i64; 2],
    /// `kCGScrollWheelEventPointDeltaAxis1/2` (pixels, integer)
    pub point: [i64; 2],
    /// `kCGScrollWheelEventFixedPtDeltaAxis1/2` (lines, 16.16 fixed read as f64)
    pub fixed: [f64; 2],
}

impl MacDeltas {
    pub fn reversed(mut self, r: Reverse) -> Self {
        for (axis, on) in [(0, r.vertical), (1, r.horizontal)] {
            if on {
                self.delta[axis] = -self.delta[axis];
                self.point[axis] = -self.point[axis];
                self.fixed[axis] = -self.fixed[axis];
            }
        }
        self
    }
}

/// Windows: device class for a wheel delta (see [`Config::windows_touchpad_heuristic`]).
pub fn classify_windows_delta(delta: i32, heuristic: bool) -> Device {
    if heuristic && delta % 120 != 0 {
        Device::Trackpad
    } else {
        Device::Mouse
    }
}

/// What the Windows hook should do with a wheel event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinAction {
    Pass,
    /// Swallow it and inject this delta instead.
    Reinject(i32),
}

/// Windows decision for one wheel event. `extra_info` is `dwExtraInfo`.
pub fn windows_action(cfg: &Config, horizontal: bool, delta: i32, extra_info: usize) -> WinAction {
    if extra_info == SELF_INJECTED_TAG || extra_info == PM_INPUT_INJECTED_TAG || delta == 0 {
        return WinAction::Pass;
    }
    let r = cfg.reverse_for(classify_windows_delta(delta, cfg.windows_touchpad_heuristic));
    let flip = if horizontal { r.horizontal } else { r.vertical };
    if flip {
        WinAction::Reinject(delta.saturating_neg())
    } else {
        WinAction::Pass
    }
}

// ---------------------------------------------------------------------------
// Runtime API
// ---------------------------------------------------------------------------

/// One observed scroll event (for diagnostics, e.g. `examples/scroll_probe.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollEventInfo {
    pub device: Device,
    /// Vertical delta as delivered (before reversal). macOS: point delta
    /// (falls back to line delta); Windows: wheel units (120 = one notch).
    pub vertical: f64,
    pub horizontal: f64,
    pub reversed: Reverse,
    /// macOS only (false/0 on Windows).
    pub continuous: bool,
    pub scroll_phase: i64,
    pub momentum_phase: i64,
}

/// Callback invoked on the event thread for each scroll event. Keep it fast
/// (e.g. send into a channel): a slow tap callback gets disabled by macOS.
pub type Observer = Arc<dyn Fn(&ScrollEventInfo) + Send + Sync>;

/// Typed errors, reachable through `anyhow::Error::downcast_ref`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("permission missing: {0}")]
    PermissionDenied(String),
    #[error("scroll reversing is not supported on this platform")]
    Unsupported,
}

/// True when `err` (from [`ScrollReverser::start`]) means a missing OS
/// permission, so the app can show "grant Accessibility" instead of a failure.
pub fn is_permission_error(err: &anyhow::Error) -> bool {
    matches!(err.downcast_ref::<Error>(), Some(Error::PermissionDenied(_)))
}

/// True when the process lacks the permission needed to start (macOS:
/// Accessibility). Never prompts. Always false on Windows.
pub fn needs_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        !macos::is_trusted(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// macOS: show the system Accessibility prompt if not yet trusted; returns
/// whether the process is trusted now. Other platforms: true.
pub fn request_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_trusted(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// macOS: the global "natural scrolling" preference
/// (`com.apple.swipescrolldirection`, true = natural, which is also the
/// default when unset). `None` on other platforms.
pub fn system_natural_scrolling() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        Some(macos::natural_scrolling())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub struct ScrollReverser;

impl ScrollReverser {
    /// Install the hook/tap and start reversing. Fails with
    /// [`Error::PermissionDenied`] when macOS Accessibility is missing.
    pub fn start(config: Config) -> anyhow::Result<Handle> {
        Self::start_with_observer(config, None)
    }

    /// Like [`start`](Self::start), also calling `observer` for every scroll event.
    pub fn start_with_observer(config: Config, observer: Option<Observer>) -> anyhow::Result<Handle> {
        let shared = Arc::new(SharedConfig::new(config));
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let inner = platform::start(shared.clone(), observer)?;
            Ok(Handle { shared, inner: Some(inner) })
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = (shared, observer);
            Err(Error::Unsupported.into())
        }
    }
}

/// Running reverser. Dropping it stops it.
pub struct Handle {
    shared: Arc<SharedConfig>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    inner: Option<platform::Tap>,
}

impl Handle {
    /// Apply new settings immediately (a single atomic store).
    pub fn update(&self, config: Config) {
        self.shared.store(config);
    }

    pub fn config(&self) -> Config {
        self.shared.load()
    }

    pub fn is_running(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.inner.is_some()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    /// Remove the hook/tap and join its thread. Idempotent.
    pub fn stop(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(t) = self.inner.take() {
            t.stop();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}
