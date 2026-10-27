//! The Workspaces page and Mouse Without Borders' "Pairing with Windows"
//! card.
//!
//! Workspaces are launched, captured and edited by the running app (it has
//! the Accessibility grant and its own editor process); this window writes
//! an `AppRequest` (`app-request.json`) and only falls back to starting
//! `powermac workspace …` itself when the app isn't running. The pairing
//! card reads `mwb-diagnostics.json` (written by the app) and runs
//! `powermac mwb doctor` on a background thread.

use std::net::Ipv4Addr;

use super::*;
use pm_config::bridge::{self, AppRequest};

/// Workspaces page state.
#[derive(Default)]
pub(super) struct WorkspacesUi {
    list: Option<Vec<workspaces::Workspace>>,
    stamp: Option<SystemTime>,
    error: Option<String>,
    new_app: String,
    running_apps: Option<Vec<(String, String)>>,
}

/// Pairing card state.
#[derive(Default)]
pub(super) struct PairingUi {
    /// `mwb-diagnostics.json` as the app last wrote it.
    diag: Option<serde_json::Value>,
    /// (address, is the LAN address, is a tunnel).
    ips: Vec<(Ipv4Addr, bool, bool)>,
    ips_at: Option<Instant>,
    /// Tests pin the addresses.
    pinned: bool,
}

const MWB_DIAG_FILE: &str = "mwb-diagnostics.json";

fn ago_ms(at_ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let s = now.saturating_sub(at_ms) / 1000;
    match s {
        0..=89 => format!("{s} s ago"),
        90..=5399 => format!("{} min ago", s / 60),
        5400..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

impl SettingsApp {
    /// Once a second: the saved workspaces (on their page), MWB diagnostics.
    pub(super) fn poll_pages3(&mut self) {
        let dir = pm_config::data_dir_for(&self.config_path);
        if self.page == Page::Workspaces || self.ws.list.is_none() {
            let st = workspaces::store::stamp(&dir);
            if st != self.ws.stamp || self.ws.list.is_none() {
                self.ws.stamp = st;
                match workspaces::list(&dir) {
                    Ok(l) => {
                        self.ws.list = Some(l);
                        self.ws.error = None;
                    }
                    Err(e) => {
                        self.ws.list = Some(Vec::new());
                        self.ws.error = Some(format!("{e:#}"));
                    }
                }
            }
        }
        if self.page == Page::Mwb {
            self.pairing.diag = std::fs::read_to_string(self.config_path.with_file_name(MWB_DIAG_FILE))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            if !self.pairing.pinned && self.pairing.ips_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                self.pairing.ips_at = Some(Instant::now());
                let ifs = mwb::llmnr::local_ifs();
                let primary = mwb::llmnr::primary_ipv4(&ifs);
                self.pairing.ips = ifs
                    .iter()
                    .filter(|i| !i.ip.is_loopback() && !i.ip.is_unspecified())
                    .map(|i| (i.ip, Some(i.ip) == primary, i.p2p))
                    .collect();
            }
        }
    }

    /// Send `req` to the running app; `fallback` (CLI arguments) starts
    /// `powermac` itself when the app isn't running.
    fn app_request(&mut self, req: AppRequest, fallback: &[&str], done: &str) {
        if self.app_status.is_some() {
            match bridge::write(&self.config_path, bridge::APP_REQUEST_FILE, &req) {
                Ok(_) => self.toast = Some((Instant::now(), done.to_string())),
                Err(e) => self.ws.error = Some(format!("{e:#}")),
            }
            return;
        }
        match crate::cli_exe() {
            Some(exe) => {
                let r = std::process::Command::new(exe)
                    .arg("--config")
                    .arg(&self.config_path)
                    .args(fallback)
                    .stdin(std::process::Stdio::null())
                    .spawn();
                match r {
                    Ok(child) => {
                        std::thread::spawn(move || {
                            let mut c = child;
                            let _ = c.wait();
                        });
                        self.toast = Some((Instant::now(), done.to_string()));
                    }
                    Err(e) => self.ws.error = Some(format!("starting PowerMac: {e}")),
                }
            }
            None => self.ws.error = Some("PowerMac isn't running.".into()),
        }
    }

    // -----------------------------------------------------------------------
    // Workspaces

    pub(super) fn workspaces_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let p = pal(ui);
        let on = self.app.cfg.workspaces.enabled;
        let mut launch: Option<(String, String)> = None;
        let mut editor = false;
        let mut capture = false;
        let list = self.ws.list.clone().unwrap_or_default();
        card_with(
            ui,
            "Your workspaces",
            Some("Launching starts the apps that aren't running and puts every window back where it was, on the right display."),
            |ui| {
                if list.is_empty() {
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::icon_text(icon::STACK).size(22.0).color(p.text3));
                            ui.add_space(4.0);
                            hint(ui, "No workspaces yet. Arrange your apps and windows, then capture the desktop to save it.");
                        });
                    });
                }
                let now = workspaces::model::now_unix();
                for w in &list {
                    let n = w.apps.len();
                    let mut desc = format!("{n} window{} · {} app{}", if n == 1 { "" } else { "s" }, w.distinct_apps(), if w.distinct_apps() == 1 { "" } else { "s" });
                    if w.monitors.len() > 1 {
                        desc.push_str(&format!(" · {} displays", w.monitors.len()));
                    }
                    match w.last_launched_time {
                        Some(t) => desc.push_str(&format!(" · launched {}", workspaces::model::relative_time(t, now))),
                        None => desc.push_str(" · never launched"),
                    }
                    let (clicked, _) = icon_row(ui, icon::STACK, Color32::from_rgb(0x30, 0xB0, 0xC7), &w.name, Some(&desc), |ui| {
                        ui.add_enabled(on, egui::Button::new(theme::icon_job(icon::PLAY, "Launch", 13.0, if on { p.text } else { p.text3 })).min_size(vec2(28.0, 28.0)))
                            .on_disabled_hover_text("Turn Workspaces on first.")
                            .clicked()
                    });
                    if clicked {
                        launch = Some((w.id.clone(), w.name.clone()));
                    }
                }
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(on, |ui| {
                            if primary_icon_button(ui, icon::PENCIL_SIMPLE, "Open editor").clicked() {
                                editor = true;
                            }
                            if icon_text_button(ui, icon::PLUS, "Capture desktop").on_hover_text("Save the windows on your screens as a new workspace and open the editor.").clicked() {
                                capture = true;
                            }
                        });
                    });
                    if let Some(e) = &self.ws.error {
                        ui.add_space(6.0);
                        error_label(ui, e);
                    }
                });
            },
        );
        let rec = &mut self.recording;
        let d = &mut self.app;
        let c = &mut d.cfg.workspaces;
        card_with(ui, "Shortcut", Some("Open the Workspaces editor from anywhere. Not set by default (PowerToys on Windows uses Win+Ctrl+`)."), |ui| {
            row(ui, "Open the editor", None, |ui| {
                hotkey_edit(ui, rec, Id::unique("ws-hk"), &mut d.workspaces_hotkey, RecMode::Combo);
            });
        });
        card(ui, "Launching", |ui| {
            row(ui, "Wait for each window", Some("How long to wait for an app's window after starting it."), |ui| {
                ui.add(egui::DragValue::new(&mut c.launch.timeout_secs).range(3..=120).suffix(" s"));
            });
            row(ui, "When a display is missing", Some("A window saved on a display that isn't connected now."), |ui| {
                combo(
                    ui,
                    "ws-missing",
                    &mut c.launch.missing_monitor,
                    &[
                        (workspaces::MissingMonitor::ScaleToPrimary, "Scale onto the main display"),
                        (workspaces::MissingMonitor::PrimaryMinimized, "Main display, minimized"),
                    ],
                );
            });
            toggle_row(ui, "Bring the first app forward", Some("At the end, activate the workspace's first app."), &mut c.launch.activate_first);
            toggle_row(ui, "Move windows back to their Space", Some("Experimental: uses a private macOS interface that may not work on recent versions."), &mut c.launch.restore_spaces);
        });
        card(ui, "Capturing", |ui| {
            toggle_row(ui, "Include minimized windows", None, &mut c.capture.include_minimized);
            toggle_row(ui, "Remember open documents", Some("Apps that report their document (TextEdit, Preview, Pages, ...) reopen it on launch."), &mut c.capture.documents);
            toggle_row(
                ui,
                "Remember browser tabs",
                Some("Asks Safari, Chrome and other browsers for the front tab's address. macOS asks once per browser (Automation)."),
                &mut c.capture.browser_urls,
            );
        });
        card_with(ui, "Never capture", Some("Apps left out of every capture (bundle id or name). PowerMac itself is always left out."), |ui| {
            super::pages::apps_list_pub(ui, &mut c.capture.excluded_apps, &mut self.ws.new_app, &mut self.ws.running_apps, "com.apple.Music");
        });
        if cfg!(target_os = "macos") {
            card(ui, "Desktop shortcuts", |ui| {
                row(ui, "Shortcut type", Some("What the editor's \"Create desktop shortcut\" makes."), |ui| {
                    segmented(ui, &mut c.shortcut_kind, &[(workspaces::ShortcutKind::App, "Small app"), (workspaces::ShortcutKind::Command, ".command file")]);
                });
            });
        }
        if let Some((id, name)) = launch {
            self.app_request(AppRequest::LaunchWorkspace { id: id.clone() }, &["workspace", "launch", &id], &format!("Launching {name}…"));
        }
        if editor {
            self.app_request(AppRequest::OpenWorkspacesEditor, &["workspace", "edit"], "Opening the editor…");
        }
        if capture {
            self.app_request(AppRequest::CaptureWorkspace { name: String::new() }, &["workspace", "capture"], "Capturing the desktop…");
        }
    }

    // -----------------------------------------------------------------------
    // Mouse Without Borders: pairing with Windows

    pub(super) fn pairing_card(&mut self, ui: &mut Ui, status: &Option<pm_config::MwbStatus>) {
        let p = pal(ui);
        let name = self.app.this_machine();
        let upper = pm_config::mwb_help::connect_name(&name);
        let port = self.app.cfg.mwb.settings.port;
        let key = mwb::normalize_key(&self.app.cfg.mwb.settings.key);
        let lan = self.pairing.ips.iter().find(|(_, lan, _)| *lan).map(|(ip, _, _)| ip.to_string());
        let ip = lan.clone().unwrap_or_else(|| "<this Mac's IP>".into());
        let diag = self.pairing.diag.clone();
        let running = status.as_ref().is_some_and(|s| s.running);
        let mut copy: Option<(String, &str)> = None;
        let mut run_doctor = false;
        let doctor_busy = self.busy.contains("mwb-doctor");
        let doctor = self.notes.get("mwb-doctor").cloned();
        card_with(
            ui,
            "Pairing with Windows",
            Some("How a PC running PowerToys finds this Mac. If it doesn't connect, run the diagnostics and follow the steps for the PC."),
            |ui| {
                row(ui, "This Mac's name", Some("Type it in PowerToys' Connect dialog. Case doesn't matter."), |ui| {
                    mono_chip(ui, &upper);
                    if icon_button(ui, icon::COPY, "Copy the name").clicked() {
                        copy = Some((upper.clone(), "Name copied"));
                    }
                });
                let ips: Vec<String> = self
                    .pairing
                    .ips
                    .iter()
                    .map(|(ip, lan, tun)| {
                        if *lan {
                            format!("{ip} (LAN)")
                        } else if *tun {
                            format!("{ip} (VPN)")
                        } else {
                            ip.to_string()
                        }
                    })
                    .collect();
                let ip_desc = if ips.is_empty() { "No network address.".to_string() } else { ips.join(" · ") };
                row(ui, "IP addresses", Some(&ip_desc), |ui| {
                    if let Some(l) = &lan {
                        mono_chip(ui, l);
                        if icon_button(ui, icon::COPY, "Copy the LAN address").clicked() {
                            copy = Some((l.clone(), "Address copied"));
                        }
                    }
                });
                // LLMNR responder.
                let llmnr = diag.as_ref().map(|d| d["llmnr"].clone()).filter(|l| !l.is_null());
                let (state, desc) = match (&llmnr, running) {
                    (_, false) => (None, "Shown while PowerMac runs Mouse Without Borders.".to_string()),
                    (None, true) => (Some(false), "Off: turn on \"Answer name queries\" in config.json (answerNameQueries) so Windows finds this Mac by name.".to_string()),
                    (Some(l), true) => {
                        let ok = l["running"].as_bool().unwrap_or(false);
                        let mut d = format!(
                            "Answers Windows' name lookups for {} on UDP {} · {} queries, {} answered",
                            l["name"].as_str().unwrap_or(&upper),
                            l["port"].as_u64().unwrap_or(5355),
                            l["queries"].as_u64().unwrap_or(0),
                            l["answered"].as_u64().unwrap_or(0)
                        );
                        if let Some(from) = l["lastAnswer"]["from"].as_str() {
                            d.push_str(&format!(" · last from {from}"));
                        }
                        if let Some(e) = l["error"].as_str() {
                            d.push_str(&format!(" · {e}"));
                        }
                        (Some(ok), d)
                    }
                };
                row(ui, "Name lookups (LLMNR)", Some(&desc), |ui| match state {
                    Some(true) => tag_pill(ui, p.ok, icon::CHECK_CIRCLE, "Answering"),
                    Some(false) => tag_pill(ui, p.warn, icon::WARNING, "Not answering"),
                    None => tag_pill(ui, p.text3, icon::INFO, "Not running"),
                });
                block(ui, |ui| {
                    ui.label(RichText::new("Recent connection attempts").font(theme::font_semibold(13.0)).color(p.text));
                    ui.add_space(4.0);
                    let incoming = diag.as_ref().and_then(|d| d["incoming"].as_array().cloned()).unwrap_or_default();
                    if incoming.is_empty() {
                        hint(ui, &format!("None yet: no PC has reached TCP {} on this Mac.", port.wrapping_add(1)));
                    }
                    for a in incoming.iter().rev().take(6) {
                        ui.horizontal(|ui| {
                            let result = a["result"].as_str().unwrap_or("");
                            let ok = result.contains("trusted") || result.contains("connected");
                            ui.label(theme::icon_text(if ok { icon::CHECK_CIRCLE } else { icon::WARNING_CIRCLE }).size(14.0).color(if ok { p.ok } else { p.warn }));
                            ui.label(RichText::new(a["from"].as_str().unwrap_or("?")).font(egui::FontId::monospace(12.0)).color(p.text));
                            ui.label(RichText::new(format!("{} · {result}", ago_ms(a["at"].as_u64().unwrap_or(0)))).size(12.0).color(p.text2));
                        });
                    }
                    let learned = diag.as_ref().and_then(|d| d["learned"].as_array().cloned()).unwrap_or_default();
                    if !learned.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Learned addresses").font(theme::font_semibold(13.0)).color(p.text));
                        hint(ui, "Machines that connected from these addresses are reached there again.");
                        for l in &learned {
                            ui.horizontal(|ui| {
                                ui.label(theme::icon_text(icon::MONITOR).size(14.0).color(p.text3));
                                ui.label(RichText::new(l["name"].as_str().unwrap_or("?")).size(12.5).color(p.text));
                                ui.label(RichText::new(l["addr"].as_str().unwrap_or("")).font(egui::FontId::monospace(12.0)).color(p.text2));
                            });
                        }
                    }
                });
                // The PC steps.
                block(ui, |ui| {
                    ui.label(RichText::new("On the Windows PC").font(theme::font_semibold(13.0)).color(p.text));
                    ui.add_space(6.0);
                    step(ui, 1, "PowerToys → Mouse Without Borders → Connect: enter this Mac's name and the same security key.", |ui| {
                        if copy_chip(ui, &upper) {
                            copy = Some((upper.clone(), "Name copied"));
                        }
                        if icon_text_button(ui, icon::KEY, "Copy key").clicked() {
                            copy = Some((key.clone(), "Security key copied"));
                        }
                    });
                    let mapping = pm_config::mwb_help::ip_mapping_line(&name, &ip);
                    step(ui, 2, "If the PC can't find the name: Advanced → \"IP address mapping\", add this line.", |ui| {
                        if copy_chip(ui, &mapping) {
                            copy = Some((mapping.clone(), "Mapping copied"));
                        }
                    });
                    step_header(ui, 3, "Still nothing? Run these in PowerShell on the PC:");
                    ui.add_space(6.0);
                    for (what, cmd) in pm_config::mwb_help::powershell_commands(&name, &ip, port) {
                        ui.horizontal(|ui| {
                            ui.add_space(30.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(what).size(11.5).color(p.text3));
                                ui.horizontal(|ui| {
                                    if icon_button(ui, icon::COPY, "Copy the command").clicked() {
                                        copy = Some((cmd.clone(), "Command copied"));
                                    }
                                    ui.add(egui::Label::new(RichText::new(&cmd).font(egui::FontId::monospace(11.5)).color(p.text)).wrap());
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
                // Diagnostics.
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        let can = crate::cli_exe().is_some();
                        ui.add_enabled_ui(can && !doctor_busy, |ui| {
                            if icon_text_button(ui, icon::MAGNIFYING_GLASS, if doctor_busy { "Running diagnostics…" } else { "Run diagnostics" })
                                .on_hover_text("Runs `powermac mwb doctor`: name, ports, key, LLMNR, VPN, firewall and recent connection attempts.")
                                .clicked()
                            {
                                run_doctor = true;
                            }
                        });
                        if let Some(Ok(out)) | Some(Err(out)) = &doctor {
                            if icon_text_button(ui, icon::COPY, "Copy output").clicked() {
                                copy = Some((out.clone(), "Diagnostics copied"));
                            }
                        }
                    });
                    if let Some(r) = &doctor {
                        let (text, err) = match r {
                            Ok(t) => (t.as_str(), false),
                            Err(t) => (t.as_str(), true),
                        };
                        ui.add_space(8.0);
                        egui::Frame::new()
                            .fill(if p.dark { Color32::from_rgb(0x16, 0x16, 0x18) } else { Color32::from_rgb(0xF7, 0xF7, 0xF9) })
                            .stroke(Stroke::new(1.0, if err { p.error } else { p.card_border }))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let h = (text.lines().count() as f32 * 21.0 + 6.0).min(340.0);
                                egui::ScrollArea::both().id_salt("mwb-doctor-out").min_scrolled_height(h).max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                                    for line in text.lines() {
                                        let c = if line.starts_with("==") {
                                            p.accent
                                        } else if line.starts_with('#') {
                                            p.text3
                                        } else if line.starts_with("- ") || line.contains("NOT ") || line.contains("AFFECTED") {
                                            p.warn
                                        } else {
                                            p.text
                                        };
                                        ui.label(RichText::new(line).font(egui::FontId::monospace(11.5)).color(c));
                                    }
                                });
                            });
                    } else if crate::cli_exe().is_none() {
                        ui.add_space(4.0);
                        hint(ui, "Run `powermac mwb doctor` in Terminal for the full report.");
                    }
                });
            },
        );
        if let Some((text, toast)) = copy {
            ui.ctx().copy_text(text);
            self.toast = Some((Instant::now(), toast.to_string()));
        }
        if run_doctor {
            if let Some(exe) = crate::cli_exe().cloned() {
                let path = self.config_path.clone();
                self.run_background("mwb-doctor", move || {
                    let out = std::process::Command::new(exe)
                        .arg("--config")
                        .arg(&path)
                        .args(["mwb", "doctor"])
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| format!("running the diagnostics: {e}"))?;
                    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                    let err = String::from_utf8_lossy(&out.stderr);
                    if !out.status.success() {
                        text.push_str(&err);
                        return Err(text);
                    }
                    Ok(text)
                });
            }
        }
    }
}

#[cfg(test)]
impl SettingsApp {
    pub fn set_pairing_for_test(&mut self, ips: Vec<(Ipv4Addr, bool, bool)>, doctor: Option<String>) {
        self.pairing.ips = ips;
        self.pairing.pinned = true;
        if let Some(d) = doctor {
            self.notes.insert("mwb-doctor", Ok(d));
        }
    }
}

/// A numbered step with controls under its text.
fn step(ui: &mut Ui, n: usize, text: &str, controls: impl FnOnce(&mut Ui)) {
    step_header(ui, n, text);
    ui.horizontal(|ui| {
        ui.add_space(30.0);
        controls(ui);
    });
    ui.add_space(8.0);
}

/// The number and text of a step.
fn step_header(ui: &mut Ui, n: usize, text: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 10.0, p.accent_soft);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, n.to_string(), theme::font_semibold(11.5), p.accent);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text)).wrap());
    });
}

/// Monospace text in a rounded chip.
fn mono_chip(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.control)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(egui::FontId::monospace(12.5)).color(p.text));
        });
}

/// A monospace chip with a copy icon; true when clicked.
fn copy_chip(ui: &mut Ui, text: &str) -> bool {
    let p = pal(ui);
    let r = egui::Frame::new()
        .fill(p.control)
        .stroke(Stroke::new(1.0, p.control_border))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new(text).font(egui::FontId::monospace(12.5)).color(p.text));
                ui.label(theme::icon_text(icon::COPY).size(13.0).color(p.text2));
            });
        });
    let resp = ui.interact(r.response.rect, Id::unique(("copy-chip", text)), Sense::click());
    resp.on_hover_text("Copy").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
