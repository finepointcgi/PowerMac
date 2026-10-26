//! Windows battery status via `GetSystemPowerStatus`.
//!
//! Windows has no generic charge-limit API: charge thresholds are vendor
//! features (Lenovo Vantage, ASUS Battery Health Charging / MyASUS, Dell
//! Power Manager, Surface "Battery Limit" UEFI setting), so the controller is
//! a status-only no-op there.

use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use crate::status::BatteryReading;

const BATTERY_FLAG_CHARGING: u8 = 8;
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
const BATTERY_FLAG_UNKNOWN: u8 = 255;

pub fn read_battery() -> BatteryReading {
    // SAFETY: plain out-parameter struct.
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return BatteryReading::default();
    }
    let has_battery =
        s.BatteryFlag != BATTERY_FLAG_UNKNOWN && s.BatteryFlag & BATTERY_FLAG_NO_BATTERY == 0;
    if !has_battery {
        return BatteryReading {
            plugged: s.ACLineStatus == 1,
            ..Default::default()
        };
    }
    BatteryReading {
        has_battery,
        percent: (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        charging: s.BatteryFlag & BATTERY_FLAG_CHARGING != 0,
        plugged: s.ACLineStatus == 1,
        time_remaining_min: (s.BatteryLifeTime != u32::MAX).then_some(s.BatteryLifeTime / 60),
        ..Default::default()
    }
}
