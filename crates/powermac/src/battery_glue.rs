//! Battery glue: `battery-status.json` (the controller's status, every 5 s)
//! for the settings window, and its "Charge to 100 % now" / "Refresh"
//! requests from `battery-request.json`.
//!
//! The controller always runs (status only while the limit is off), so the
//! tray and the settings window can show the battery. Nothing is written to
//! the SMC unless the limit is on *and* the helper was installed from
//! Settings → Battery ("Install helper…", one admin prompt); this module
//! never installs anything.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use battery_limit::Strategy;
use pm_config::bridge::{self, BatteryRequest, RequestReader, BATTERY_REQUEST_FILE, BATTERY_STATUS_FILE};
use pm_config::AppConfig;

use crate::RUNNING;

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("battery-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, BATTERY_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(req) = reader.poll::<BatteryRequest>(path, BATTERY_REQUEST_FILE) {
            let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(h) = guard.as_ref().and_then(|r| r.battery.as_ref()) {
                match req {
                    BatteryRequest::ChargeToFull => {
                        log::info!("battery: charging to 100 % until unplugged (settings)");
                        h.charge_to_full_once();
                    }
                    BatteryRequest::Refresh => h.refresh(),
                }
            }
            // The controller answers within a moment.
            written = Instant::now() - Duration::from_millis(4_200);
        }
        if written.elapsed() >= Duration::from_secs(5) {
            written = Instant::now();
            let st = {
                let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_ref().and_then(|r| r.battery.as_ref()).map(|h| h.status())
            };
            if let Some(st) = st {
                if let Err(e) = bridge::write(path, BATTERY_STATUS_FILE, &st) {
                    log::debug!("battery: status file: {e:#}");
                }
            }
        }
    }
}

/// Whether this Mac can limit charging at all (some strategy other than
/// "unavailable" was detected).
pub(crate) fn can_limit(st: &battery_limit::BatteryStatus) -> bool {
    st.strategy.is_some_and(|s| s != Strategy::None)
}

/// "82 % · charging", "100 % · on power", "54 % · 3 h 10 min left".
pub(crate) fn status_line(st: &battery_limit::BatteryStatus) -> String {
    let b = &st.battery;
    if !b.has_battery {
        return "No battery".into();
    }
    let pct = b.percent.map(|p| format!("{p} %")).unwrap_or_else(|| "–".into());
    let what = if st.discharging && st.strategy == Some(Strategy::LimitViaDischarge) {
        "holding the limit on battery".to_string()
    } else if st.discharging {
        "discharging to the limit".to_string()
    } else if st.inhibited {
        "charge limit held".to_string()
    } else if b.charging {
        "charging".to_string()
    } else if b.plugged {
        "on power".to_string()
    } else {
        match b.time_remaining_min {
            Some(m) if m > 0 => format!("{} h {:02} min left", m / 60, m % 60),
            _ => "on battery".to_string(),
        }
    };
    format!("{pct} · {what}")
}

/// "Health 91 % · 312 cycles · 31 °C · 96 W adapter".
pub(crate) fn detail_line(st: &battery_limit::BatteryStatus) -> String {
    let b = &st.battery;
    let mut detail = Vec::new();
    if let Some(h) = b.health_percent {
        detail.push(format!("health {h:.0} %"));
    }
    if let Some(c) = b.cycles {
        detail.push(format!("{c} cycles"));
    }
    if let Some(t) = b.temperature_c {
        detail.push(format!("{t:.0} °C"));
    }
    if let Some(w) = b.adapter_watts.filter(|_| b.plugged) {
        detail.push(format!("{w} W adapter"));
    }
    let mut detail = detail.join(" · ");
    if let Some(first) = detail.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    if detail.is_empty() {
        "–".into()
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use battery_limit::{BatteryReading, BatteryStatus};

    #[test]
    fn lines() {
        let mut st = BatteryStatus {
            battery: BatteryReading {
                has_battery: true,
                percent: Some(82),
                plugged: true,
                charging: true,
                cycles: Some(312),
                health_percent: Some(91.4),
                temperature_c: Some(31.2),
                adapter_watts: Some(96),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(status_line(&st), "82 % · charging");
        assert_eq!(detail_line(&st), "Health 91 % · 312 cycles · 31 °C · 96 W adapter");
        assert!(!can_limit(&st));
        st.strategy = Some(Strategy::LimitViaDischarge);
        st.discharging = true;
        assert!(can_limit(&st));
        assert_eq!(status_line(&st), "82 % · holding the limit on battery");
        st.strategy = Some(Strategy::None);
        assert!(!can_limit(&st));
        st.battery.has_battery = false;
        assert_eq!(status_line(&st), "No battery");
        assert_eq!(detail_line(&BatteryStatus::default()), "–");
    }
}
