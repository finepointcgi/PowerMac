//! Battery-limit settings (the `batteryLimit` section of config.json).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Opt-in: nothing is ever written to the SMC while this is false.
    pub enabled: bool,
    /// Stop charging at this percentage (clamped to 20–100; 100 = no limit).
    pub limit_percent: u8,
    /// Sailing mode: once at the limit, let the charge drift down this many
    /// percent before charging again (0 = keep it pinned at the limit).
    pub sailing_percent: u8,
    /// When plugged in above the limit, turn the adapter off so the battery
    /// discharges down to the limit (the Mac runs from the battery).
    pub discharge_to_limit: bool,
    /// Pause charging while the battery is at or above this temperature in
    /// °C (0 = off). Resumes 2 °C below.
    pub heat_protection_celsius: u8,
    /// Keep holding the limit while the Mac sleeps. Off by default: the
    /// helper re-enables charging (and always turns the adapter back on)
    /// before sleep, so a Mac that sleeps for days on the charger may charge
    /// to 100 % but is never left in a non-default state unattended.
    pub keep_limit_during_sleep: bool,
    /// How often the controller re-reads the battery and refreshes the
    /// helper (seconds, 5–600).
    pub poll_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            limit_percent: 80,
            sailing_percent: 5,
            discharge_to_limit: false,
            heat_protection_celsius: 35,
            keep_limit_during_sleep: false,
            poll_seconds: 30,
        }
    }
}

impl Config {
    /// Values clamped into their valid ranges.
    pub fn normalized(&self) -> Config {
        let limit = self.limit_percent.clamp(20, 100);
        Config {
            limit_percent: limit,
            sailing_percent: self.sailing_percent.min(20).min(limit.saturating_sub(10)),
            heat_protection_celsius: self.heat_protection_celsius.min(60),
            poll_seconds: self.poll_seconds.clamp(5, 600),
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_json() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert!(!c.enabled);
        assert_eq!(c.limit_percent, 80);
        assert_eq!(c.sailing_percent, 5);
        assert_eq!(c.heat_protection_celsius, 35);
        let c: Config = serde_json::from_str(
            r#"{"enabled":true,"limitPercent":60,"keepLimitDuringSleep":true}"#,
        )
        .unwrap();
        assert!(c.enabled && c.keep_limit_during_sleep);
        assert_eq!(c.limit_percent, 60);
        let j = serde_json::to_value(Config::default()).unwrap();
        assert!(j.get("dischargeToLimit").is_some() && j.get("heatProtectionCelsius").is_some());
    }

    #[test]
    fn normalize_clamps() {
        let c = Config {
            limit_percent: 5,
            sailing_percent: 50,
            poll_seconds: 1,
            ..Default::default()
        }
        .normalized();
        assert_eq!(c.limit_percent, 20);
        assert_eq!(c.sailing_percent, 10);
        assert_eq!(c.poll_seconds, 5);
        let c = Config {
            limit_percent: 150,
            ..Default::default()
        }
        .normalized();
        assert_eq!(c.limit_percent, 100);
    }
}
