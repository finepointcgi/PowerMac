//! macOS battery status from the `AppleSmartBattery` IORegistry entry
//! (readable by any user), with the SMC as a fallback for temperature.

use super::macos_ffi::*;
use crate::smc::{decode_number, AppleSmc, SmcBackend};
use crate::status::BatteryReading;

/// Unknown-time sentinel used by the battery driver.
const TIME_UNKNOWN: i64 = 65535;

struct Battery(IoObj);

impl Battery {
    fn int(&self, key: &str) -> Option<i64> {
        let v = self.0.property(key)?;
        // SAFETY: owned, valid CF object.
        unsafe { cf_to_i64(v.0) }
    }

    /// `key` at top level, else inside the `BatteryData` dictionary
    /// (newer macOS only publishes some values there).
    fn int_any(&self, key: &str) -> Option<i64> {
        self.int(key).or_else(|| self.nested("BatteryData", key))
    }

    fn nested(&self, dict: &str, key: &str) -> Option<i64> {
        let d = self.0.property(dict)?;
        // SAFETY: d is owned and valid; dict_get returns a borrowed value.
        unsafe {
            if !is_dict(d.0) {
                return None;
            }
            cf_to_i64(dict_get(d.0, key))
        }
    }
}

pub fn read_battery() -> BatteryReading {
    let Some(svc) = IoObj::matching("AppleSmartBattery") else {
        return BatteryReading::default();
    };
    let b = Battery(svc);
    let installed = b.int("BatteryInstalled").map(|v| v != 0).unwrap_or(true);
    if !installed {
        return BatteryReading::default();
    }
    let mut r = BatteryReading {
        has_battery: true,
        ..Default::default()
    };

    // On Apple Silicon CurrentCapacity/MaxCapacity are percentages (Max = 100);
    // on Intel both are mAh. The ratio works for both.
    let cur = b.int("CurrentCapacity");
    let max = b.int("MaxCapacity");
    if let (Some(c), Some(m)) = (cur, max) {
        if m > 0 {
            r.percent = Some(((c * 100 + m / 2) / m).clamp(0, 100) as u8);
        }
    }
    r.charging = b.int("IsCharging").unwrap_or(0) != 0;
    r.plugged = b.int("ExternalConnected").unwrap_or(0) != 0;
    r.adapter_watts = b
        .nested("AdapterDetails", "Watts")
        .filter(|w| *w > 0)
        .map(|w| w as u32);
    r.cycles = b.int_any("CycleCount").map(|v| v as u32);

    let design = b.int_any("DesignCapacity").filter(|v| *v > 0);
    // NominalChargeCapacity is what System Settings shows as "Maximum
    // Capacity"; AppleRawMaxCapacity is the gauge's current full-charge value.
    let full = b
        .int_any("NominalChargeCapacity")
        .or_else(|| b.int_any("AppleRawMaxCapacity"))
        .filter(|v| *v > 0);
    r.design_capacity_mah = design.map(|v| v as u32);
    r.max_capacity_mah = full.map(|v| v as u32);
    if let (Some(d), Some(f)) = (design, full) {
        r.health_percent = Some(((f as f64 / d as f64) * 1000.0).round() as f32 / 10.0);
    }

    r.amperage_ma = b
        .int("InstantAmperage")
        .or_else(|| b.int("Amperage"))
        .map(|a| a as i32);
    let time = if r.charging {
        b.int("AvgTimeToFull")
    } else {
        b.int("AvgTimeToEmpty")
    }
    .filter(|t| *t > 0 && *t < TIME_UNKNOWN)
    .or_else(|| {
        b.int("TimeRemaining")
            .filter(|t| *t > 0 && *t < TIME_UNKNOWN)
    });
    r.time_remaining_min = time.map(|t| t as u32);

    // `Temperature` is centi-degrees C where published (it isn't on recent
    // Apple Silicon releases); fall back to the SMC battery sensor.
    r.temperature_c = b
        .int("Temperature")
        .filter(|t| *t > 0)
        .map(|t| t as f32 / 100.0)
        .or_else(smc_battery_temperature);
    r
}

/// Battery temperature from the SMC (`TB0T`, then `TB1T`/`TB2T`); reading
/// these needs no privileges.
pub fn smc_battery_temperature() -> Option<f32> {
    let mut smc = AppleSmc::open().ok()?;
    for key in ["TB0T", "TB1T", "TB2T"] {
        if let Ok(Some(info)) = smc.key_info(key) {
            if let Ok(bytes) = smc.read_key(key) {
                if let Some(v) = decode_number(&info, &bytes) {
                    if v > 0.0 && v < 120.0 {
                        return Some(v as f32);
                    }
                }
            }
        }
    }
    None
}
