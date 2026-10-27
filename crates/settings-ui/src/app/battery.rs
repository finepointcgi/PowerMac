//! The Battery page (Displays & Power): health readout, the charge limit
//! with the strategy this Mac allows (and its trade-offs, shown before it is
//! turned on), and macOS's own Charge Limit.
//!
//! The controller runs in the app, which writes `battery-status.json` every
//! 5 s; "Charge to 100 % now" goes back through `battery-request.json`.
//! While the app isn't running the battery and the SMC scheme are read here
//! (unprivileged). Installing the privileged helper is only ever done by the
//! "Install helper…" button (one administrator prompt, on a background
//! thread) — never automatically.

use super::*;
use super::pages::callout;

use battery_limit::{BatteryStatus, Strategy};
use pm_config::bridge::{self, BatteryRequest};

const JOB: &str = "battery-helper";

/// Battery page state.
#[derive(Default)]
pub(super) struct BatteryUi {
    /// Written by the running app.
    status: Option<BatteryStatus>,
    /// Read here while the app isn't running (with when).
    local: Option<(BatteryStatus, Instant)>,
    /// Tests pin the status.
    pinned: bool,
}

impl SettingsApp {
    pub(super) fn poll_battery(&mut self) {
        if self.battery_ui.pinned {
            return;
        }
        self.battery_ui.status = bridge::read_fresh(&self.config_path, bridge::BATTERY_STATUS_FILE, bridge::STATUS_FRESH_MS);
        if self.page == Page::Battery
            && self.battery_ui.status.is_none()
            && self.battery_ui.local.as_ref().is_none_or(|(_, t)| t.elapsed() >= Duration::from_secs(5))
        {
            self.battery_ui.local = Some((local_status(&self.app.cfg.battery_limit), Instant::now()));
        }
    }

    fn write_battery_request(&self, r: BatteryRequest) {
        if let Err(e) = bridge::write(&self.config_path, bridge::BATTERY_REQUEST_FILE, &r) {
            log::warn!("battery request: {e:#}");
        }
    }

    /// For tests: show this status as if the app had written it.
    #[cfg(test)]
    pub fn set_battery_for_test(&mut self, st: Option<BatteryStatus>) {
        self.battery_ui.status = st;
        self.battery_ui.pinned = true;
    }

    pub(super) fn battery_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let from_app = self.battery_ui.status.is_some();
        let st = match (&self.battery_ui.status, &self.battery_ui.local) {
            (Some(s), _) => s.clone(),
            (None, Some((s, _))) => s.clone(),
            (None, None) => {
                let s = local_status(&self.app.cfg.battery_limit);
                self.battery_ui.local = Some((s.clone(), Instant::now()));
                s
            }
        };
        let b = &st.battery;
        let busy = self.busy.contains(JOB);
        let mut full = false;
        let mut install = false;
        let mut uninstall = false;

        if !from_app && mac {
            callout(ui, icon::INFO, "PowerMac isn't running", "The battery is read directly. The charge limit works while PowerMac runs.");
        }
        card(ui, "Status", |ui| {
            if !b.has_battery {
                block(ui, |ui| hint(ui, "This computer has no battery."));
                return;
            }
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    let p = pal(ui);
                    ui.label(RichText::new(b.percent.map(|v| format!("{v} %")).unwrap_or_else(|| "–".into())).font(theme::font_semibold(30.0)).color(p.text));
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        ui.label(RichText::new(battery_state(&st)).font(theme::font_semibold(13.5)).color(p.text));
                        if !st.reason.is_empty() {
                            ui.label(RichText::new(&st.reason).size(12.0).color(p.text2));
                        }
                    });
                });
                if let Some(pct) = b.percent {
                    let p = pal(ui);
                    let w = ui.available_width();
                    let (r, _) = ui.allocate_exact_size(vec2(w, 8.0), Sense::hover());
                    ui.painter().rect_filled(r, 4.0, p.control);
                    let mut fill = r;
                    fill.set_width(w * f32::from(pct.min(100)) / 100.0);
                    ui.painter().rect_filled(fill, 4.0, if pct <= 20 { p.error } else { p.ok });
                    // The limit, as a tick on the bar.
                    let c = &self.app.cfg.battery_limit;
                    if c.enabled && c.limit_percent < 100 {
                        let x = r.left() + w * f32::from(c.limit_percent) / 100.0;
                        ui.painter().vline(x, r.y_range().expand(3.0), Stroke::new(2.0, p.text));
                    }
                }
            });
            let health = match (b.health_percent, b.max_capacity_mah, b.design_capacity_mah) {
                (Some(h), Some(m), Some(d)) => format!("{h:.0} %  ({m} of {d} mAh)"),
                (Some(h), _, _) => format!("{h:.0} %"),
                _ => "–".into(),
            };
            row(ui, "Health", Some("Full-charge capacity compared to when it was new."), |ui| ui.label(health));
            row(ui, "Cycle count", None, |ui| ui.label(b.cycles.map(|c| c.to_string()).unwrap_or_else(|| "–".into())));
            row(ui, "Temperature", None, |ui| ui.label(b.temperature_c.map(|t| format!("{t:.1} °C")).unwrap_or_else(|| "–".into())));
            let adapter = match (b.plugged, b.adapter_watts) {
                (true, Some(w)) => format!("{w} W, connected"),
                (true, None) => "Connected".into(),
                (false, _) => "Not connected".into(),
            };
            row(ui, "Power adapter", None, |ui| ui.label(adapter));
            if let Some(m) = b.time_remaining_min.filter(|m| *m > 0) {
                let t = if m >= 60 { format!("{} h {:02} min", m / 60, m % 60) } else { format!("{m} min") };
                row(ui, if b.charging { "Until full" } else { "Time left" }, None, |ui| ui.label(t));
            }
        });

        let c = &mut self.app.cfg.battery_limit;
        let strategy = st.strategy;
        let usable = strategy.is_some_and(|s| s != Strategy::None);
        card_with(
            ui,
            "Charge limit",
            Some("Lithium batteries age slower when they don't sit at 100 %. A limit stops charging there; the Mac runs from the adapter."),
            |ui| {
                if !mac {
                    block(ui, |ui| {
                        hint(ui, "Windows has no common charge-limit setting. Most laptops have one in their maker's app: Lenovo Vantage, MyASUS, Dell Power Manager, HP Battery Health Manager, or the Surface UEFI.");
                    });
                    return;
                }
                if !b.has_battery {
                    block(ui, |ui| hint(ui, "Not available without a battery."));
                    return;
                }
                // How it works on this Mac, before anything is turned on.
                let (glyph, tint, title) = match strategy {
                    Some(Strategy::Inhibit) | Some(Strategy::FirmwareRange) => (icon::CHECK_CIRCLE, pal(ui).ok, format!("This Mac: {}", st.strategy_label)),
                    Some(Strategy::LimitViaDischarge) => (icon::WARNING, pal(ui).warn, format!("This Mac: {}", st.strategy_label)),
                    Some(Strategy::None) => (icon::WARNING_CIRCLE, pal(ui).error, "No charge control on this Mac".to_string()),
                    None => (icon::INFO, pal(ui).text3, "Charge control unknown".to_string()),
                };
                let note = if st.strategy_note.is_empty() {
                    match strategy {
                        Some(s) => s.note().to_string(),
                        None => "The battery controller couldn't be read, so PowerMac can't tell whether a limit is possible on this Mac.".to_string(),
                    }
                } else {
                    st.strategy_note.clone()
                };
                block(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.label(theme::icon_text(glyph).size(18.0).color(tint));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(pal(ui).text));
                            ui.add(egui::Label::new(RichText::new(note).size(12.0).color(pal(ui).text2)).wrap());
                            if let Some(s) = &st.scheme {
                                ui.label(RichText::new(format!("SMC keys: {s}")).size(11.0).color(pal(ui).text3));
                            }
                        });
                    });
                });
                ui.add_enabled_ui(usable || c.enabled, |ui| toggle_row(ui, "Limit charging", None, &mut c.enabled));
                if !usable && strategy.is_some() {
                    toggle_row(
                        ui,
                        "Limit via discharge",
                        Some("When the firmware has no charge-inhibit key, switch the adapter off at the limit and back on below the sailing threshold."),
                        &mut c.limit_via_discharge,
                    );
                }
                ui.add_enabled_ui(c.enabled && usable, |ui| {
                    row(ui, "Stop charging at", None, |ui| slider(ui, &mut c.limit_percent, 20..=100, " %"));
                    row(ui, "Let it drop before charging again", Some("Sailing: avoids topping up for every percent. 0 keeps it at the limit."), |ui| {
                        slider(ui, &mut c.sailing_percent, 0..=20, " %")
                    });
                    if strategy != Some(Strategy::LimitViaDischarge) {
                        toggle_row(
                            ui,
                            "Discharge down to the limit",
                            Some("When plugged in above the limit, the adapter is switched off until the battery is down to it."),
                            &mut c.discharge_to_limit,
                        );
                    }
                    row(ui, "Pause charging above", Some("Heat protection. 0 turns it off."), |ui| slider(ui, &mut c.heat_protection_celsius, 0..=60, " °C"));
                    toggle_row(
                        ui,
                        "Keep the limit while the Mac sleeps",
                        Some("Off: charging is allowed again before sleep, so nothing stays changed while PowerMac can't watch."),
                        &mut c.keep_limit_during_sleep,
                    );
                    if strategy.is_some_and(|s| s != Strategy::None) && matches!(strategy, Some(Strategy::Inhibit) | Some(Strategy::FirmwareRange)) {
                        toggle_row(
                            ui,
                            "Fall back to limit via discharge",
                            Some("Only used if the charge-inhibit key stops working (a firmware update)."),
                            &mut c.limit_via_discharge,
                        );
                    }
                    row(ui, "Charge to 100 % now", Some("Once, until you unplug the adapter (before a trip)."), |ui| {
                        if st.top_up_active {
                            tag_pill(ui, pal(ui).ok, icon::LIGHTNING, "Charging to 100 %");
                        } else if ui.add_enabled(from_app && c.enabled, egui::Button::new("Charge to 100 %")).clicked() {
                            full = true;
                        }
                    });
                });
                row(
                    ui,
                    "Charging helper",
                    Some("Changing how the battery charges needs a small privileged helper (a LaunchDaemon). Installing it asks for an administrator password once; nothing is installed until you click Install."),
                    |ui| {
                        if busy {
                            ui.spinner();
                            return;
                        }
                        let ok = st.helper_installed;
                        let running = st.helper_running || !from_app;
                        if ok && ui.add(egui::Button::new(RichText::new("Uninstall…").size(12.5)).frame(false)).clicked() {
                            uninstall = true;
                        }
                        if (!ok || st.needs_helper) && ui.add_enabled(usable, egui::Button::new("Install helper…")).clicked() {
                            install = true;
                        }
                        status_pill(ui, ok && running, if ok { if running { "Installed" } else { "Not responding" } } else { "Not installed" });
                    },
                );
                if st.needs_helper && c.enabled {
                    block(ui, |ui| warn_label(ui, "The limit is on but can't be enforced until the helper is installed."));
                }
                if busy {
                    block(ui, |ui| hint(ui, "Waiting for the administrator password in the macOS prompt…"));
                }
                if let Some(e) = &st.error {
                    block(ui, |ui| warn_label(ui, e));
                }
            },
        );
        self.note(ui, JOB);

        if mac && b.has_battery {
            card_with(
                ui,
                "macOS Charge Limit",
                Some("macOS 26.4 and later have their own limit (80–100 %) in System Settings → Battery. The firmware enforces it, also while the Mac sleeps. PowerMac only reads it."),
                |ui| {
                    let (text, ok) = match st.native_limit_percent {
                        Some(n) => (format!("Set to {n} %"), true),
                        None => ("Not set".to_string(), false),
                    };
                    row(ui, "Charge Limit", if ok && self.app.cfg.battery_limit.enabled { Some("The lower of the two limits wins.") } else { None }, |ui| {
                        if icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Open Battery Settings…").clicked() {
                            open_battery_settings();
                        }
                        ui.label(RichText::new(text).color(if ok { pal(ui).text } else { pal(ui).text2 }));
                    });
                },
            );
        }

        if full {
            self.write_battery_request(BatteryRequest::ChargeToFull);
            self.toast = Some((Instant::now(), "Charging to 100 %".into()));
        }
        if install {
            let path = self.config_path.clone();
            self.run_background(JOB, move || {
                let Some(helper) = helper_binary() else {
                    return Err(format!(
                        "{} isn't next to PowerMac (build the app bundle with scripts/bundle-macos.sh).",
                        battery_limit::HELPER_BINARY_NAME
                    ));
                };
                let r = battery_limit::install_helper(&helper).map(|()| "Helper installed.".to_string()).map_err(|e| format!("{e:#}"));
                let _ = bridge::write(&path, bridge::BATTERY_REQUEST_FILE, &BatteryRequest::Refresh);
                r
            });
        }
        if uninstall {
            let path = self.config_path.clone();
            self.run_background(JOB, move || {
                let r = battery_limit::uninstall_helper().map(|()| "Helper removed; charging is back to normal.".to_string()).map_err(|e| format!("{e:#}"));
                let _ = bridge::write(&path, bridge::BATTERY_REQUEST_FILE, &BatteryRequest::Refresh);
                r
            });
        }
    }
}

/// The helper binary shipped with this build: next to this executable (the
/// app bundle's `Contents/MacOS`), or next to the `powermac` that started
/// this window.
fn helper_binary() -> Option<std::path::PathBuf> {
    battery_limit::bundled_helper_path().or_else(|| {
        let p = crate::cli_exe()?.parent()?.join(battery_limit::HELPER_BINARY_NAME);
        p.is_file().then_some(p)
    })
}

/// The battery as this process can read it (no helper status).
fn local_status(cfg: &battery_limit::Config) -> BatteryStatus {
    let supported = cfg!(target_os = "macos");
    let scheme = if supported { battery_limit::detect_scheme().ok() } else { None };
    let strategy = scheme.map(|s| s.strategy(cfg.limit_via_discharge));
    BatteryStatus {
        battery: battery_limit::read_battery(),
        helper_installed: supported && battery_limit::helper_installed(),
        charge_control: scheme.map(|s| s.can_inhibit()),
        scheme: scheme.map(|s| s.name()),
        strategy,
        strategy_label: strategy.map(|s| s.label()).unwrap_or("unknown").to_string(),
        strategy_note: strategy.map(|s| s.note().to_string()).unwrap_or_default(),
        native_limit_percent: if supported { battery_limit::native::read_native_limit() } else { None },
        supported,
        reason: String::new(),
        ..Default::default()
    }
}

fn battery_state(st: &BatteryStatus) -> String {
    let b = &st.battery;
    if st.discharging && st.strategy == Some(Strategy::LimitViaDischarge) {
        "Holding the limit on battery".into()
    } else if st.discharging {
        "Discharging to the limit".into()
    } else if st.inhibited {
        "Holding at the limit".into()
    } else if b.charging {
        "Charging".into()
    } else if b.plugged {
        "On power, not charging".into()
    } else {
        "On battery".into()
    }
}

fn open_battery_settings() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Battery-Settings.extension").spawn();
}
