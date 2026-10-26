//! Battery status types.

use serde::{Deserialize, Serialize};

/// What the OS reports about the battery (no privileges needed).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BatteryReading {
    /// False on desktops / when no battery is installed.
    pub has_battery: bool,
    /// State of charge, 0–100.
    pub percent: Option<u8>,
    /// The battery is currently charging.
    pub charging: bool,
    /// External power is connected.
    pub plugged: bool,
    /// The adapter's rated wattage.
    pub adapter_watts: Option<u32>,
    pub cycles: Option<u32>,
    /// Full-charge capacity / design capacity, in percent.
    pub health_percent: Option<f32>,
    pub design_capacity_mah: Option<u32>,
    pub max_capacity_mah: Option<u32>,
    /// Battery temperature in °C (registry `Temperature`, else SMC `TB0T`).
    pub temperature_c: Option<f32>,
    /// Minutes to empty (on battery) or to full (charging), if known.
    pub time_remaining_min: Option<u32>,
    /// Instantaneous battery current in mA (negative = discharging).
    pub amperage_ma: Option<i32>,
}

/// Everything the settings UI / tray needs to show.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BatteryStatus {
    #[serde(flatten)]
    pub battery: BatteryReading,
    /// Charging is being held off by us (helper reports the keys set).
    pub inhibited: bool,
    /// The adapter is turned off by us so the battery discharges.
    pub discharging: bool,
    /// `/Library/PrivilegedHelperTools/com.powermac.battery-helper` and its
    /// LaunchDaemon plist exist.
    pub helper_installed: bool,
    /// The helper answered on its socket.
    pub helper_running: bool,
    /// The limit is enabled but can't be enforced until the helper is
    /// installed (show an "Install helper…" button).
    pub needs_helper: bool,
    /// Detected SMC key scheme, e.g. `CHTE+CHIE` (from the helper, or read
    /// locally when the helper isn't running).
    pub scheme: Option<String>,
    /// This Mac's SMC has a known charge-inhibit key (`CHTE` or
    /// `CH0B`/`CH0C`). `None` = unknown (not macOS / SMC unreadable).
    pub charge_control: Option<bool>,
    /// "Charge to 100% now" is active until the adapter is unplugged.
    pub top_up_active: bool,
    /// What the controller decided last and why, e.g. "at limit (80%)".
    pub reason: String,
    /// Last helper / platform error, if any.
    pub error: Option<String>,
    /// Charge limiting is possible on this platform at all.
    pub supported: bool,
}
