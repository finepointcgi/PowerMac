//! Plain data types shared by the rules engine, the platform backends and the
//! public API.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which "default device" slot a device fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKind {
    /// The default output (what apps play through).
    Output,
    /// The default input (microphone).
    Input,
    /// The device alerts and sound effects play through (macOS "Play sound
    /// effects through"; the default *communications* render endpoint on
    /// Windows).
    SystemOutput,
}

/// How a device is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Transport {
    BuiltIn,
    Usb,
    Bluetooth,
    BluetoothLe,
    Hdmi,
    DisplayPort,
    AirPlay,
    Virtual,
    Aggregate,
    Thunderbolt,
    Pci,
    FireWire,
    /// Continuity camera / iPhone microphone.
    Continuity,
    #[default]
    Unknown,
}

impl Transport {
    pub fn is_bluetooth(self) -> bool {
        matches!(self, Transport::Bluetooth | Transport::BluetoothLe)
    }

    pub fn label(self) -> &'static str {
        match self {
            Transport::BuiltIn => "Built-in",
            Transport::Usb => "USB",
            Transport::Bluetooth => "Bluetooth",
            Transport::BluetoothLe => "Bluetooth LE",
            Transport::Hdmi => "HDMI",
            Transport::DisplayPort => "DisplayPort",
            Transport::AirPlay => "AirPlay",
            Transport::Virtual => "Virtual",
            Transport::Aggregate => "Aggregate",
            Transport::Thunderbolt => "Thunderbolt",
            Transport::Pci => "PCI",
            Transport::FireWire => "FireWire",
            Transport::Continuity => "Continuity",
            Transport::Unknown => "Unknown",
        }
    }
}

/// One output or input device as seen from one direction. A device with both
/// inputs and outputs (a USB headset) shows up in both `outputs()` and
/// `inputs()`, with the same `uid`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    /// Persistent identifier: the CoreAudio device UID on macOS, the endpoint
    /// id string on Windows. Use this in rules.
    pub uid: String,
    pub name: String,
    pub transport: Transport,
    /// Default device for this direction (`outputs()` → default output,
    /// `inputs()` → default input).
    pub is_default: bool,
    /// Output devices only: the device alerts/sound effects play through.
    pub is_default_system: bool,
    pub channels: u32,
    /// Scalar volume 0.0–1.0, `None` when the device has no volume control.
    pub volume: Option<f32>,
    /// `None` when the device has no mute control.
    pub muted: Option<bool>,
}

/// An application that is producing (or recently produced) audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioApp {
    /// The first process of the app (e.g. the Chrome audio helper).
    pub pid: i32,
    /// Every process grouped into this app.
    pub pids: Vec<i32>,
    /// macOS: bundle id of the outermost `.app` (helpers fold into their
    /// app); executable name when there is no bundle. Windows: the
    /// executable file name in lower case (e.g. `chrome.exe`).
    pub bundle_id: String,
    pub name: String,
    /// 0.0–1.5 (1.0 = unchanged). Windows caps it at 1.0.
    pub volume: f32,
    pub muted: bool,
    /// Producing audio right now.
    pub playing: bool,
}

/// A device reference in a rule: matched by `uid` first, then by `name`.
///
/// An entry with an empty `uid` matches any device whose name *contains*
/// `name` (case-insensitive), so `"AirPods"` covers "Mitch's AirPods Pro".
/// With a `uid`, the name is the fallback for when the uid changes (USB
/// devices moved to another port) and must match exactly (case-insensitive).
///
/// In JSON an entry may be a plain string (a name pattern) or
/// `{"uid": "...", "name": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", from = "DeviceMatchRepr")]
pub struct DeviceMatch {
    pub uid: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DeviceMatchRepr {
    Name(String),
    Full {
        #[serde(default)]
        uid: String,
        #[serde(default)]
        name: String,
    },
}

impl From<DeviceMatchRepr> for DeviceMatch {
    fn from(r: DeviceMatchRepr) -> Self {
        match r {
            DeviceMatchRepr::Name(name) => DeviceMatch { uid: String::new(), name },
            DeviceMatchRepr::Full { uid, name } => DeviceMatch { uid, name },
        }
    }
}

impl DeviceMatch {
    pub fn uid(uid: impl Into<String>, name: impl Into<String>) -> Self {
        DeviceMatch { uid: uid.into(), name: name.into() }
    }

    pub fn name(name: impl Into<String>) -> Self {
        DeviceMatch { uid: String::new(), name: name.into() }
    }

    pub fn matches_uid(&self, dev: &AudioDevice) -> bool {
        !self.uid.is_empty() && self.uid == dev.uid
    }

    pub fn matches_name(&self, dev: &AudioDevice) -> bool {
        if self.name.is_empty() {
            return false;
        }
        let want = self.name.to_lowercase();
        let have = dev.name.to_lowercase();
        if self.uid.is_empty() {
            have.contains(&want)
        } else {
            have == want
        }
    }

    pub fn matches(&self, dev: &AudioDevice) -> bool {
        self.matches_uid(dev) || self.matches_name(dev)
    }
}

/// Preferred devices, highest priority first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PriorityRules {
    pub outputs: Vec<DeviceMatch>,
    pub inputs: Vec<DeviceMatch>,
}

impl PriorityRules {
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty() && self.inputs.is_empty()
    }
}

/// A persisted per-app level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppLevel {
    /// 0.0–1.5.
    pub volume: f32,
    pub muted: bool,
}

impl Default for AppLevel {
    fn default() -> Self {
        AppLevel { volume: 1.0, muted: false }
    }
}

impl AppLevel {
    pub fn is_neutral(&self) -> bool {
        !self.muted && (self.volume - 1.0).abs() < 0.005
    }
}

/// The `audioControl` config section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// Preferred outputs/inputs: when devices appear or disappear the
    /// highest-priority available one becomes the default.
    pub rules: PriorityRules,
    /// Devices macOS/Windows may not switch to on their own (e.g. AirPods).
    /// Choosing one yourself (our UI, Control Center, System Settings) still
    /// works.
    pub block_auto_switch_to: Vec<DeviceMatch>,
    /// Keep the default input on the built-in microphone when Bluetooth
    /// headphones connect, so they stay in high-quality (A2DP) mode.
    pub keep_builtin_mic_with_bluetooth: bool,
    /// Experimental on macOS (process taps, macOS 14.4+, needs the audio
    /// capture permission); native on Windows.
    pub per_app_volume: bool,
    /// Per-app levels keyed by bundle id (macOS) / exe name (Windows).
    pub app_volumes: BTreeMap<String, AppLevel>,
    /// Per-app output routing keyed by bundle id (macOS) / exe name
    /// (Windows). Empty by default.
    pub app_routes: BTreeMap<String, crate::mixer::AppRoute>,
    /// The mixer matrix (off by default).
    pub mixer: crate::mixer::Mixer,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            rules: PriorityRules::default(),
            block_auto_switch_to: Vec::new(),
            keep_builtin_mic_with_bluetooth: false,
            per_app_volume: false,
            app_volumes: BTreeMap::new(),
            app_routes: BTreeMap::new(),
            mixer: crate::mixer::Mixer::default(),
        }
    }
}

/// Who changed a default device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeSource {
    /// The OS or another app (Control Center, System Settings, a device
    /// connecting).
    System,
    /// Our rules (priority list, auto-switch block, built-in mic).
    Rules,
    /// `Handle::set_default_*`.
    Manual,
}

/// Why the rules engine switched a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleReason {
    Priority,
    BlockedAutoSwitch,
    BuiltInMic,
    /// The system-output device followed an output switch.
    FollowOutput,
}

/// Notifications for the tray / settings window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Event {
    /// The device list changed (something connected or disconnected).
    DevicesChanged,
    DefaultChanged {
        kind: DeviceKind,
        uid: Option<String>,
        name: Option<String>,
        source: ChangeSource,
    },
    /// The rules engine switched a default device.
    RuleApplied {
        kind: DeviceKind,
        uid: String,
        name: String,
        reason: RuleReason,
    },
    /// A per-app level changed (persist `Handle::app_levels()` into
    /// `Config::app_volumes`).
    AppLevelChanged {
        bundle_id: String,
        volume: f32,
        muted: bool,
    },
    /// The routes changed through the handle (persist `Handle::routes()`
    /// into `Config::app_routes`).
    RoutesChanged,
    /// The mixer changed through the handle (persist `Handle::mixer()` into
    /// `Config::mixer`).
    MixerChanged,
    /// Something went wrong in the background (e.g. a tap failed).
    Error { message: String },
}

/// State of the macOS audio-capture (process tap) permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapturePermission {
    Granted,
    Denied,
    /// Not asked yet (the first tap will prompt).
    NotDetermined,
    /// Not needed (Windows) or cannot be determined.
    NotApplicable,
}
