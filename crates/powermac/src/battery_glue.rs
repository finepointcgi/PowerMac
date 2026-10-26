//! Battery glue: `battery-status.json` (the controller's status, every 5 s)
//! for the settings window, and its "Charge to 100 % now" / "Refresh"
//! requests from `battery-request.json`.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

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
                        log::info!("battery: charging to 100 % until unplugged");
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

/// "82 % · charging", "100 % · on power", "54 % · 3 h 10 min left".
pub(crate) fn status_line(st: &battery_limit::BatteryStatus) -> String {
    let b = &st.battery;
    if !b.has_battery {
        return "No battery".into();
    }
    let pct = b.percent.map(|p| format!("{p} %")).unwrap_or_else(|| "–".into());
    let what = if st.discharging {
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
