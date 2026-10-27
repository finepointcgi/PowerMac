//! First-run setup (Welcome → Choose features → Permissions → Done), the
//! permission rows it shares with Settings → General → "Permissions &
//! Health", the Updates card, and restarting the app.
//!
//! The setup takes the whole window (`powermac settings --page welcome`;
//! `--page permissions` opens it on the Permissions step). The tray app
//! opens it on first launch and, unless turned off, when a permission an
//! enabled module needs is missing at startup. Finishing, skipping or
//! closing it sets `setup.completed` in `config.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;

use super::*;
use crate::permissions::{self as perms, Grant, Need, Perm, Probe, RowState};
use pm_config::bridge::{self, AppRequest, ModuleState};

pub(super) const STEP_WELCOME: usize = 0;
pub(super) const STEP_FEATURES: usize = 1;
pub(super) const STEP_PERMISSIONS: usize = 2;
pub(super) const STEP_DONE: usize = 3;
const STEPS: [&str; 4] = ["Welcome", "Features", "Permissions", "Done"];
const SETUP_W: f32 = 700.0;
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A feature group of the "Choose features" step: name, blurb, icon, tint, modules.
pub(super) struct Group {
    pub name: &'static str,
    pub blurb: &'static str,
    pub icon: &'static str,
    pub tint: Color32,
    pub pages: &'static [Page],
}

pub(super) const GROUPS: [Group; 5] = [
    Group {
        name: "Windows & Desktop",
        blurb: "Snap windows into zones, switch between windows, save whole desktops, and a shelf in the notch.",
        icon: icon::SQUARES_FOUR,
        tint: Color32::from_rgb(0x5E, 0x5C, 0xE6),
        pages: &[Page::FancyZones, Page::WindowSwitcher, Page::Workspaces, Page::NotchShelf, Page::DockPreviews, Page::MenuBar],
    },
    Group {
        name: "Mouse & Keyboard",
        blurb: "Pointer speed without acceleration, scroll direction per device, key remaps and text snippets.",
        icon: icon::KEYBOARD,
        tint: Color32::from_rgb(0x2F, 0xB3, 0x4F),
        pages: &[Page::MouseTuning, Page::ScrollReverser, Page::MouseUtils, Page::KeyboardManager, Page::TextExpander],
    },
    Group {
        name: "Displays & Sound",
        blurb: "Brightness and volume of external monitors, which speakers win, and keeping the Mac awake.",
        icon: icon::SUN,
        tint: Color32::from_rgb(0xF5, 0xA2, 0x00),
        pages: &[Page::Displays, Page::Sound, Page::Awake],
    },
    Group {
        name: "Productivity",
        blurb: "Clipboard history, a color picker and copying text from anywhere on screen.",
        icon: icon::CLIPBOARD_TEXT,
        tint: Color32::from_rgb(0x34, 0x7D, 0xF6),
        pages: &[Page::ClipboardHistory, Page::ColorPicker, Page::TextExtractor],
    },
    Group {
        name: "Mouse Without Borders",
        blurb: "One mouse and keyboard for this Mac and your Windows PCs, with a shared clipboard.",
        icon: icon::MOUSE,
        tint: Color32::from_rgb(0x0A, 0x7A, 0xF5),
        pages: &[Page::Mwb],
    },
];

/// State of the setup and of the permission rows.
#[derive(Default)]
pub(super) struct SetupUi {
    pub step: usize,
    /// Latest live permission state (a fresh `powermac permissions --json`,
    /// or this process when that isn't available).
    probe: Option<Probe>,
    probe_rx: Option<mpsc::Receiver<Option<Probe>>>,
    probe_at: Option<Instant>,
    /// Tests: keep this probe.
    pinned: bool,
    /// When a permission UI was last drawn (probes only run while one is visible).
    shown_at: Option<Instant>,
    /// "I turned it on, but it still says Not granted."
    says_on: BTreeSet<Perm>,
    /// When "Open Settings" / "Request" was clicked, per permission.
    opened: BTreeMap<Perm, Instant>,
    /// "Restart PowerMac" was clicked.
    restart_at: Option<Instant>,
    restart_error: Option<String>,
}

/// Live state of a non-core permission from what the app reports.
fn notch_grant(st: Option<&bridge::NotchStatus>, p: Perm) -> Grant {
    let Some(st) = st.filter(|s| s.running) else { return Grant::Unknown };
    Grant::from_notch(match p {
        Perm::Calendars => &st.calendar,
        Perm::Reminders => &st.reminders,
        Perm::Camera => &st.camera,
        _ => "",
    })
}

impl SettingsApp {
    // -----------------------------------------------------------------------
    // Polling

    /// Once a second: the app's status, the update state, live permissions.
    pub(super) fn poll_setup(&mut self) {
        let p = &self.config_path;
        self.app_status = bridge::read_fresh(p, bridge::APP_STATUS_FILE, bridge::STATUS_FRESH_MS);
        self.update_state = bridge::read::<pm_update::UpdateState>(p, bridge::UPDATE_STATUS_FILE).map(|s| s.data);
        self.poll_probe();
    }

    fn poll_probe(&mut self) {
        let s = &mut self.setup;
        if s.pinned {
            return;
        }
        if let Some(rx) = &s.probe_rx {
            match rx.try_recv() {
                Ok(Some(p)) => {
                    s.probe = Some(p);
                    s.probe_rx = None;
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    s.probe = Some(perms::probe_in_process());
                    s.probe_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        let visible = s.shown_at.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let due = s.probe_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(900));
        if !(visible || s.probe.is_none()) || !due {
            return;
        }
        s.probe_at = Some(Instant::now());
        match crate::cli_exe() {
            Some(exe) => {
                // A fresh process: macOS caches some answers per process.
                let (tx, rx) = mpsc::channel();
                let exe = exe.clone();
                std::thread::spawn(move || {
                    let out = std::process::Command::new(exe)
                        .args(["permissions", "--json"])
                        .stdin(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .output();
                    let probe = out.ok().filter(|o| o.status.success()).and_then(|o| serde_json::from_slice::<Probe>(&o.stdout).ok());
                    let _ = tx.send(probe);
                });
                s.probe_rx = Some(rx);
            }
            None => s.probe = Some(perms::probe_in_process()),
        }
    }

    /// Mark the setup as done (saved with the next save).
    pub(super) fn complete_setup(&mut self) {
        let s = &mut self.app.cfg.setup;
        s.completed = true;
        s.completed_version = VERSION.to_string();
    }

    /// The live grant of `p` and what the running app sees (`None`: not known).
    fn grant_of(&self, p: Perm) -> (Grant, Option<bool>) {
        let probe = self.setup.probe.clone().unwrap_or_else(perms::probe_in_process);
        let live = match p {
            Perm::Calendars | Perm::Reminders | Perm::Camera => notch_grant(self.notch_status.as_ref(), p),
            Perm::Automation => Grant::Unknown,
            _ => probe.get(p),
        };
        let app = self.app_status.as_ref().and_then(|st| {
            let a = &st.permissions;
            match p {
                Perm::Accessibility => Some(a.accessibility),
                Perm::InputMonitoring => Some(a.input_monitoring),
                Perm::ScreenRecording => Some(a.screen_recording),
                _ => None,
            }
        });
        (live, app)
    }

    fn row_state_of(&self, p: Perm) -> RowState {
        let (live, app) = self.grant_of(p);
        perms::row_state(p, live, app, self.setup.says_on.contains(&p))
    }

    /// Ask the running app to restart (or start it), then close this window:
    /// the restarted app reopens it on `reopen`.
    pub(super) fn restart_app(&mut self, ctx: &egui::Context, reopen: Option<&str>) {
        self.flush();
        self.setup.restart_error = None;
        let reopen = reopen.map(str::to_string);
        if self.app_status.as_ref().is_some_and(|s| !s.executable.is_empty()) {
            let req = AppRequest::Restart { reopen_page: reopen };
            match bridge::write(&self.config_path, bridge::APP_REQUEST_FILE, &req) {
                Ok(_) => {
                    self.setup.restart_at = Some(Instant::now());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => self.setup.restart_error = Some(format!("{e:#}")),
            }
            return;
        }
        // Not running (or a build without restart support): start a new app.
        match start_app(&self.config_path, reopen.as_deref()) {
            Ok(()) => {
                self.setup.restart_at = Some(Instant::now());
                if self.app_status.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Err(e) => self.setup.restart_error = Some(e),
        }
    }

    // -----------------------------------------------------------------------
    // The setup window

    pub(super) fn draw_setup(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let step = self.setup.step.min(STEP_DONE);
        let mut next: Option<usize> = None;
        let mut finish = false;
        egui::Panel::top("setup-steps")
            .exact_size(74.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                if let Some(s) = step_indicator(ui, step) {
                    next = Some(s);
                }
            });
        egui::Panel::bottom("setup-bar")
            .exact_size(66.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.card_border)))
            .show(ui, |ui| {
                let r = ui.max_rect();
                let col = SETUP_W.min(r.width() - 32.0);
                let inner = Rect::from_center_size(r.center(), vec2(col, r.height()));
                let mut bar = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
                if step < STEP_DONE {
                    let skip = bar.add(egui::Button::new(RichText::new("Skip setup").size(13.0).color(p.text2)).frame(false));
                    if skip.on_hover_text("Close the setup; it's in Settings → General whenever you want it.").clicked() {
                        finish = true;
                    }
                }
                bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = match step {
                        STEP_WELCOME => "Get started",
                        STEP_DONE => "Finish",
                        _ => "Continue",
                    };
                    let main = ui.add(
                        egui::Button::new(RichText::new(label).font(theme::font_semibold(14.0)).color(p.on_accent))
                            .fill(p.accent)
                            .stroke(Stroke::NONE)
                            .corner_radius(8.0)
                            .min_size(vec2(132.0, 34.0)),
                    );
                    if main.clicked() {
                        if step == STEP_DONE {
                            finish = true;
                        } else {
                            next = Some(step + 1);
                        }
                    }
                    if step > STEP_WELCOME
                        && ui.add(egui::Button::new(RichText::new("Back").size(13.5)).corner_radius(8.0).min_size(vec2(84.0, 34.0))).clicked()
                    {
                        next = Some(step - 1);
                    }
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("setup-scroll", step)).auto_shrink([false, false]).show(ui, |ui| {
                let w = ui.available_width();
                let col = (w - 48.0).clamp(320.0, SETUP_W);
                let left = ((w - col) / 2.0).max(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(left);
                    ui.vertical(|ui| {
                        ui.set_width(col);
                        ui.set_max_width(col);
                        match step {
                            STEP_WELCOME => self.setup_welcome(ui),
                            STEP_FEATURES => self.setup_features(ui),
                            STEP_PERMISSIONS => self.setup_permissions(ui),
                            _ => self.setup_done(ui),
                        }
                        ui.add_space(28.0);
                    });
                });
            });
        });
        if let Some(s) = next {
            self.setup.step = s.min(STEP_DONE);
        }
        if finish {
            self.complete_setup();
            self.save_app();
            self.set_page(Page::General);
        }
    }

    fn setup_welcome(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            app_logo(ui, 72.0);
            ui.add_space(14.0);
            ui.label(RichText::new("Welcome to PowerMac").font(theme::font_semibold(30.0)).color(p.text));
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new("PowerToys-style utilities for your Mac — window zones, a window switcher, one mouse for all your computers, and a lot more, in one menu bar app.")
                        .size(15.0)
                        .color(p.text2),
                )
                .wrap()
                .halign(Align::Center),
            );
        });
        ui.add_space(22.0);
        let tiles: [(&str, Color32, &str, &str); 6] = [
            (icon::SQUARES_FOUR, Color32::from_rgb(0x5E, 0x5C, 0xE6), "FancyZones", "Hold Shift while dragging to snap windows into zones."),
            (icon::CARDS, Color32::from_rgb(0x58, 0x56, 0xD6), "Window Switcher", "Option+Tab through windows, with live previews."),
            (icon::MOUSE, Color32::from_rgb(0x0A, 0x7A, 0xF5), "Mouse Without Borders", "Move the cursor onto your Windows PC and back."),
            (icon::STACK, Color32::from_rgb(0x30, 0xB0, 0xC7), "Workspaces", "Save your apps and windows, restore them in one click."),
            (icon::TRAY, Color32::from_rgb(0x3A, 0x3A, 0x3C), "Notch Shelf", "Park files in the notch, see what's playing and what's next."),
            (icon::CLIPBOARD_TEXT, Color32::from_rgb(0x34, 0x7D, 0xF6), "Clipboard History", "Everything you copied, one shortcut away."),
        ];
        let gap = 12.0;
        let w = ui.available_width();
        let cols = if w > 560.0 { 3 } else { 2 };
        let tw = (w - gap * (cols as f32 - 1.0)) / cols as f32;
        for chunk in tiles.chunks(cols) {
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (glyph, tint, title, text) in chunk {
                    ui.allocate_ui_with_layout(vec2(tw, 104.0), Layout::top_down(Align::Min), |ui| {
                        card_frame(&p).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                            ui.set_width(tw - 26.0);
                            ui.set_max_width(tw - 26.0);
                            ui.set_height(78.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                icon_tile(ui, glyph, *tint, 28.0);
                                ui.add(egui::Label::new(RichText::new(*title).font(theme::font_semibold(13.0)).color(p.text)).truncate());
                            });
                            ui.add_space(6.0);
                            ui.add(egui::Label::new(RichText::new(*text).size(12.0).color(p.text2)).wrap());
                        });
                    });
                }
            });
            ui.add_space(gap);
        }
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(theme::mix(p.card, p.ok, if p.dark { 0.10 } else { 0.06 }))
            .stroke(Stroke::new(1.0, theme::mix(p.card_border, p.ok, 0.35)))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(theme::icon_text(icon::SHIELD_CHECK).size(20.0).color(p.ok));
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new("Private by design").font(theme::font_semibold(13.5)).color(p.text));
                        ui.add(
                            egui::Label::new(
                                RichText::new("Everything runs on this Mac. No account, no tracking. The network is only used by Mouse Without Borders on your local network and by an optional daily update check.")
                                    .size(12.5)
                                    .color(p.text2),
                            )
                            .wrap(),
                        );
                    });
                });
            });
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Takes about a minute: choose features, grant two or three permissions, done.").size(12.5).color(p.text3));
        });
    }

    fn setup_features(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        step_title(ui, "Choose your features", "Turn on what you'd like to use. Everything can be changed later in Settings, and what's off uses no resources.");
        let mut set: Vec<(Page, bool)> = Vec::new();
        for g in &GROUPS {
            let on: Vec<bool> = g.pages.iter().map(|pg| self.module_on(*pg).unwrap_or(false)).collect();
            let n_on = on.iter().filter(|b| **b).count();
            let mut all = n_on > 0;
            ui.add_space(12.0);
            card_frame(&p).inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icon_tile(ui, g.icon, g.tint, 40.0);
                    ui.add_space(6.0);
                    let right = 120.0;
                    ui.allocate_ui_with_layout(vec2(ui.available_width() - right, 40.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(g.name).font(theme::font_semibold(15.0)).color(p.text));
                        ui.add(egui::Label::new(RichText::new(g.blurb).size(12.0).color(p.text2)).wrap());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if toggle_labeled(ui, &mut all, g.name).changed() {
                            for pg in g.pages {
                                set.push((*pg, all));
                            }
                        }
                        if g.pages.len() > 1 {
                            ui.label(RichText::new(format!("{n_on} of {}", g.pages.len())).size(12.0).color(p.text3));
                        }
                    });
                });
                if g.pages.len() > 1 {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add_space(46.0);
                        ui.vertical(|ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                                for (pg, on) in g.pages.iter().zip(&on) {
                                    if feature_chip(ui, pg.title(), meta(*pg).icon, *on, g.tint).clicked() {
                                        set.push((*pg, !*on));
                                    }
                                }
                            });
                        });
                    });
                }
            });
        }
        for (pg, on) in set {
            self.set_module(pg, on);
        }
        ui.add_space(14.0);
        hint(ui, "File Tools (PowerRename, New+, Image Resizer) and the Finder basics are always available from Finder's Quick Actions and the menu bar icon.");
    }

    fn setup_permissions(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        step_title(
            ui,
            "Allow what your features need",
            "macOS asks you to switch these on in System Settings → Privacy & Security. Only the ones the features you chose use are needed.",
        );
        ui.add_space(14.0);
        let needs = perms::needs(&self.app.cfg);
        let (core, optional): (Vec<Need>, Vec<Need>) = needs.into_iter().partition(|n| n.perm.is_core());
        card_frame(&p).inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (i, n) in core.iter().enumerate() {
                if i > 0 {
                    divider(ui);
                }
                self.perm_row(ui, n, false);
            }
        });
        if !optional.is_empty() {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Also used by features you turned on").font(theme::font_semibold(13.5)).color(p.text));
            });
            ui.add_space(6.0);
            card_frame(&p).inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (i, n) in optional.iter().enumerate() {
                    if i > 0 {
                        divider(ui);
                    }
                    self.perm_row(ui, n, false);
                }
            });
        }
        self.restart_callout(ui);
        ui.add_space(10.0);
        let mut remind = self.app.cfg.setup.remind_missing_permissions;
        card_frame(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            reset_rows(ui);
            toggle_row(
                ui,
                "Remind me at startup",
                Some("Open this step when PowerMac starts and a permission an enabled feature needs is missing."),
                &mut remind,
            );
        });
        self.app.cfg.setup.remind_missing_permissions = remind;
    }

    /// The "restart after granting" note with its button.
    fn restart_callout(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let restart_needed = Perm::ALL.iter().any(|pm| self.row_state_of(*pm) == RowState::NeedsRestart);
        let c = if restart_needed { p.warn } else { p.accent };
        ui.add_space(16.0);
        let mut restart = false;
        egui::Frame::new()
            .fill(theme::mix(p.card, c, if p.dark { 0.16 } else { 0.07 }))
            .stroke(Stroke::new(1.0, theme::mix(p.card_border, c, 0.4)))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(theme::icon_text(icon::ARROWS_CLOCKWISE).size(20.0).color(c));
                    ui.add_space(2.0);
                    let right = 170.0;
                    ui.allocate_ui_with_layout(vec2(ui.available_width() - right, 10.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let title = if restart_needed { "Restart PowerMac to use the new permissions" } else { "Restart PowerMac after granting" };
                        ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(p.text));
                        ui.add(
                            egui::Label::new(
                                RichText::new("macOS gives Input Monitoring and Screen Recording only to a freshly started app. Reopening the window isn't enough; this quits PowerMac and starts it again.")
                                    .size(12.5)
                                    .color(p.text2),
                            )
                            .wrap(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let r = if restart_needed {
                            primary_icon_button(ui, icon::ARROWS_CLOCKWISE, "Restart PowerMac")
                        } else {
                            icon_text_button(ui, icon::ARROWS_CLOCKWISE, "Restart PowerMac")
                        };
                        restart = r.clicked();
                    });
                });
                if let Some(e) = &self.setup.restart_error {
                    ui.add_space(6.0);
                    error_label(ui, e);
                }
            });
        if restart {
            let ctx = ui.ctx().clone();
            let reopen = if self.page == Page::Welcome { "permissions" } else { "general" };
            self.restart_app(&ctx, Some(reopen));
        }
    }

    fn setup_done(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        ui.add_space(26.0);
        ui.vertical_centered(|ui| {
            icon_tile(ui, icon::CHECK, p.ok, 64.0);
            ui.add_space(14.0);
            ui.label(RichText::new("You're all set").font(theme::font_semibold(28.0)).color(p.text));
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new("PowerMac lives in the menu bar. Here are the shortcuts of the features you turned on.")
                        .size(14.5)
                        .color(p.text2),
                )
                .wrap()
                .halign(Align::Center),
            );
        });
        ui.add_space(8.0);
        let mut uses = self.app.external_hotkeys.clone();
        uses.extend(hotkey_uses(&self.app.cfg));
        let mut lines: Vec<(String, String)> = Vec::new();
        if self.app.cfg.fancyzones.enabled {
            lines.push(("Shift+Drag".into(), "Drag a window with Shift held to snap it into a zone (FancyZones)".into()));
        }
        for u in uses.iter().filter(|u| u.combo.vk != 0) {
            let name = if u.name == u.page.title() { u.name.clone() } else { format!("{} ({})", u.name, u.page.title()) };
            if !lines.iter().any(|(k, _)| *k == u.combo.to_text()) {
                lines.push((u.combo.to_text(), name));
            }
        }
        card_with(ui, "Shortcuts", Some("Change any of them on the feature's page in Settings."), |ui| {
            block(ui, |ui| {
                if lines.is_empty() {
                    hint(ui, "The features you chose have no shortcuts. Everything is in the menu bar icon.");
                }
                for (keys, what) in lines.iter().take(14) {
                    ui.horizontal(|ui| {
                        ui.set_min_height(30.0);
                        let r = keycaps(ui, keys);
                        ui.add_space((232.0 - r.rect.width()).max(8.0));
                        ui.add(egui::Label::new(RichText::new(what).size(13.0).color(p.text)).wrap());
                    });
                }
            });
        });
        card(ui, "Good to know", |ui| {
            for (glyph, title, text) in [
                (icon::TRAY, "The menu bar icon", "Turns features on and off, shows the status and opens Settings, the layout editor and your workspaces."),
                (icon::MAGNIFYING_GLASS, "Search the settings", "Type in the field at the top of the Settings sidebar to find any option."),
                (icon::SHIELD_CHECK, "Permissions & Health", "Settings → General shows every permission and whether each feature runs."),
                (icon::ROCKET_LAUNCH, "Updates", "PowerMac checks GitHub once a day and tells you when a new version is out; nothing is installed without you."),
            ] {
                let _ = icon_row(ui, glyph, p.accent, title, Some(text), |_| {});
            }
        });
        let mut login = self.login;
        let mut updates = self.app.cfg.updates.check;
        card(ui, "Startup", |ui| {
            if toggle_row(ui, "Launch PowerMac at login", None, &mut login).changed() {
                match pm_config::login::set_enabled(login) {
                    Ok(()) => self.login_error = None,
                    Err(e) => self.login_error = Some(format!("{e:#}")),
                }
                self.login = pm_config::login::is_enabled();
            }
            toggle_row(ui, "Check for updates once a day", Some("Asks GitHub for the latest release; shows it in the menu bar and in Settings."), &mut updates);
        });
        self.app.cfg.updates.check = updates;
    }

    // -----------------------------------------------------------------------
    // Permission rows (setup and Settings → General)

    fn perm_row(&mut self, ui: &mut Ui, need: &Need, compact: bool) {
        let p = pal(ui);
        let perm = need.perm;
        self.setup.shown_at = Some(Instant::now());
        let state = self.row_state_of(perm);
        let unused = need.features.is_empty();
        let tint = match state {
            RowState::Granted => p.ok,
            RowState::NeedsRestart | RowState::Stale => p.warn,
            RowState::Missing if unused => p.text3,
            RowState::Missing => p.error,
            RowState::Unknown => p.text3,
        };
        let mut open = false;
        let mut request = false;
        let mut restart = false;
        let (glyph, label) = match state {
            RowState::Granted => (icon::CHECK_CIRCLE, "Granted"),
            RowState::NeedsRestart => (icon::ARROWS_CLOCKWISE, "Restart needed"),
            RowState::Missing if unused => (icon::INFO, "Not needed now"),
            RowState::Missing => (icon::WARNING_CIRCLE, "Not granted"),
            RowState::Stale => (icon::WARNING, "Outdated entry"),
            RowState::Unknown => (icon::INFO, "Asked when used"),
        };
        let tile = if compact { 28.0 } else { 34.0 };
        let indent = tile + 12.0;
        ui.add_space(10.0);
        let w = ui.available_width();
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.vertical(|ui| {
                ui.add_space(1.0);
                let c = if state == RowState::Granted { Color32::from_rgb(0x8E, 0x8E, 0x93) } else { tint };
                icon_tile(ui, perm.icon(), c, tile);
            });
            ui.add_space(12.0);
            let right = if state == RowState::Granted { 110.0 } else { 290.0 };
            let text_w = (w - indent - right).max(180.0);
            ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(text_w);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(perm.title()).font(theme::font_semibold(13.5)).color(p.text));
                ui.add(egui::Label::new(RichText::new(perm.why()).size(12.0).color(p.text2)).wrap());
                let used = if unused {
                    "Not used by the features that are on.".to_string()
                } else {
                    format!("Used by {}.", need.features.join(", "))
                };
                ui.add(egui::Label::new(RichText::new(used).size(11.5).color(p.text3)).wrap());
                match state {
                    RowState::NeedsRestart => {
                        ui.add_space(4.0);
                        warn_label(ui, "Granted. PowerMac still runs with the old answer: restart it to use this.");
                    }
                    RowState::Missing if !unused => {
                        ui.add_space(2.0);
                        let l = ui.add(
                            egui::Button::new(RichText::new("Turned it on, but it still says Not granted?").size(12.0).color(p.accent))
                                .frame(false)
                                .small(),
                        );
                        if l.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.setup.says_on.insert(perm);
                        }
                    }
                    _ => {}
                }
            });
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 28.0), Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if state != RowState::Granted {
                    open = icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Open").on_hover_text(format!("System Settings → Privacy & Security → {}", perm.title())).clicked();
                }
                if perm.can_request() && state == RowState::Missing && !unused {
                    request = ui.add(egui::Button::new("Request…").min_size(vec2(28.0, 28.0))).on_hover_text("Show macOS' permission prompt").clicked();
                }
                chip(ui, tint, glyph, label);
            });
        });
        if state == RowState::Stale {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.vertical(|ui| {
                    egui::Frame::new()
                        .fill(theme::mix(p.card, p.warn, if p.dark { 0.14 } else { 0.07 }))
                        .stroke(Stroke::new(1.0, theme::mix(p.card_border, p.warn, 0.35)))
                        .corner_radius(10.0)
                        .inner_margin(egui::Margin::symmetric(14, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("macOS has the switch on for an older PowerMac build").font(theme::font_semibold(13.0)).color(p.text));
                            ui.add_space(4.0);
                            for (i, s) in perms::stale_steps(perm).iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                    ui.painter().circle_filled(r.center(), 9.0, theme::mix(p.card, p.warn, 0.35));
                                    ui.painter().text(r.center(), Align2::CENTER_CENTER, (i + 1).to_string(), theme::font_semibold(11.0), p.text);
                                    ui.add(egui::Label::new(RichText::new(s).size(12.5).color(p.text2)).wrap());
                                });
                                ui.add_space(2.0);
                            }
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Open System Settings").clicked() {
                                    open = true;
                                }
                                if icon_text_button(ui, icon::ARROWS_CLOCKWISE, "Restart PowerMac").clicked() {
                                    restart = true;
                                }
                                if ui.add(egui::Button::new(RichText::new("It's fixed").size(12.5).color(p.text2)).frame(false)).clicked() {
                                    self.setup.says_on.remove(&perm);
                                }
                            });
                        });
                });
            });
        }
        ui.add_space(10.0);
        if open {
            self.setup.opened.insert(perm, Instant::now());
            perms::open_pane(perm);
        }
        if request {
            self.setup.opened.insert(perm, Instant::now());
            if perm == Perm::AudioCapture {
                // Blocks until answered.
                self.run_background("perm-request", move || {
                    Ok(if perms::request(perm) { "Granted.".into() } else { "Not granted.".into() })
                });
            } else if !perms::request(perm) {
                // Prompts once; after that only System Settings can change it.
                perms::open_pane(perm);
            }
            self.setup.probe_at = None;
        }
        if restart {
            let ctx = ui.ctx().clone();
            let reopen = if self.page == Page::Welcome { "permissions" } else { "general" };
            self.restart_app(&ctx, Some(reopen));
        }
    }

    // -----------------------------------------------------------------------
    // Settings → General

    /// "Permissions & Health": permissions, the input hook, every module.
    pub(super) fn health_card(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let needs = perms::needs(&self.app.cfg);
        let shown: Vec<Need> = needs.into_iter().filter(|n| n.perm.is_core() || !n.features.is_empty()).collect();
        let mut go = None;
        let mut setup = false;
        let mut start = false;
        card_with(
            ui,
            "Permissions & Health",
            Some("What PowerMac may do on this Mac, and whether each module is running."),
            |ui| {
                if cfg!(target_os = "macos") {
                    for (i, n) in shown.iter().enumerate() {
                        if i > 0 {
                            divider(ui);
                        }
                        self.perm_row(ui, n, true);
                    }
                    continue_rows(ui);
                }
                let Some(st) = self.app_status.clone() else {
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::icon_text(icon::INFO).size(16.0).color(p.text3));
                            hint(ui, "PowerMac isn't running, so the module status isn't available. It appears here while PowerMac runs.");
                        });
                        if crate::cli_exe().is_some() {
                            ui.add_space(6.0);
                            if icon_text_button(ui, icon::PLAY, "Start PowerMac").clicked() {
                                start = true;
                            }
                        }
                    });
                    return;
                };
                // The input hook.
                let h = &st.hook;
                let (ok, label) = match h.state.as_str() {
                    "active" if h.healthy => (Some(true), "Healthy".to_string()),
                    "active" => (Some(false), "Not responding".to_string()),
                    "idle" => (None, "Not needed".to_string()),
                    "listenOnly" => (Some(false), "Listen-only".to_string()),
                    "starting" => (None, "Starting".to_string()),
                    "disabled" => (Some(false), "Disabled by macOS".to_string()),
                    _ => (Some(false), "Unavailable".to_string()),
                };
                let mut desc = format!("One shared keyboard and mouse hook for every module · {} subscriber{}", h.subscribers.len(), if h.subscribers.len() == 1 { "" } else { "s" });
                if h.reenabled > 0 {
                    desc.push_str(&format!(" · re-enabled {} time{}", h.reenabled, if h.reenabled == 1 { "" } else { "s" }));
                }
                if h.secure_input {
                    desc.push_str(" · Secure Input is on (a password field has the keyboard)");
                }
                if !h.detail.is_empty() {
                    desc.push_str(&format!(" · {}", h.detail));
                }
                row(ui, "Input hook", Some(&desc), |ui| match ok {
                    Some(true) => tag_pill(ui, p.ok, icon::CHECK_CIRCLE, &label),
                    Some(false) => tag_pill(ui, p.warn, icon::WARNING, &label),
                    None => tag_pill(ui, p.text3, icon::INFO, &label),
                });
                let slow: Vec<_> = h.slow_handlers.iter().filter(|s| s.slow > 0).collect();
                if !slow.is_empty() {
                    block(ui, |ui| {
                        ui.label(RichText::new("Slow handlers (over 10 ms)").font(theme::font_semibold(12.5)).color(p.text));
                        for s in slow {
                            ui.label(
                                RichText::new(format!("{} — {} slow of {} calls, slowest {} ms", s.name, s.slow, s.calls, s.max_ms))
                                    .size(12.0)
                                    .color(p.text2),
                            );
                        }
                    });
                }
                // Modules.
                block(ui, |ui| {
                    let on: Vec<_> = st.modules.iter().filter(|m| m.state != ModuleState::Off).collect();
                    let off = st.modules.len() - on.len();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Modules").font(theme::font_semibold(13.0)).color(p.text));
                        ui.label(RichText::new(format!("{} on · {off} off · PowerMac {} (pid {})", on.len(), st.version, st.pid)).size(12.0).color(p.text3));
                    });
                    ui.add_space(4.0);
                    let w = ui.available_width();
                    let cols = if w > 560.0 { 2 } else { 1 };
                    let cw = (w - 12.0 * (cols as f32 - 1.0)) / cols as f32;
                    for chunk in on.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            for m in chunk {
                                ui.allocate_ui_with_layout(vec2(cw, 30.0), Layout::left_to_right(Align::Center), |ui| {
                                    ui.set_width(cw);
                                    let (c, g) = match m.state {
                                        ModuleState::Running => (p.ok, icon::CHECK_CIRCLE),
                                        ModuleState::WaitingForPermission => (p.warn, icon::LOCK_KEY),
                                        ModuleState::Failed => (p.error, icon::WARNING_CIRCLE),
                                        ModuleState::Off => (p.text3, icon::POWER),
                                    };
                                    ui.label(theme::icon_text(g).size(15.0).color(c));
                                    let name = ui.add(egui::Label::new(RichText::new(&m.name).size(13.0).color(p.text)).sense(Sense::click()));
                                    if name.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        go = Page::from_name(&m.page);
                                    }
                                    let detail = match m.state {
                                        ModuleState::Running if m.detail.is_empty() => "running".to_string(),
                                        ModuleState::WaitingForPermission if m.detail.is_empty() => "waiting for permission".to_string(),
                                        ModuleState::Failed if m.detail.is_empty() => "didn't start — see the log".to_string(),
                                        _ => m.detail.clone(),
                                    };
                                    ui.add(egui::Label::new(RichText::new(detail).size(12.0).color(if m.state == ModuleState::Running { p.text2 } else { c })).truncate());
                                });
                            }
                        });
                    }
                    if on.is_empty() {
                        hint(ui, "No module is on.");
                    }
                });
            },
        );
        // Restart / setup again, below the card's rows.
        ui.add_space(10.0);
        card_frame(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            reset_rows(ui);
            row(
                ui,
                "Restart PowerMac",
                Some("Quits and starts the app again: needed after granting Input Monitoring or Screen Recording."),
                |ui| {
                    if icon_text_button(ui, icon::ARROWS_CLOCKWISE, "Restart").clicked() {
                        let ctx = ui.ctx().clone();
                        self.restart_app(&ctx, Some("general"));
                    }
                },
            );
            row(ui, "Setup", Some("The welcome window: choose features and walk through the permissions again."), |ui| {
                if icon_text_button(ui, icon::SPARKLE, "Open setup").clicked() {
                    setup = true;
                }
            });
            if let Some(e) = &self.setup.restart_error {
                error_label(ui, e);
                ui.add_space(8.0);
            }
        });
        if start {
            if let Err(e) = start_app(&self.config_path, None) {
                self.setup.restart_error = Some(e);
            }
        }
        if setup {
            self.setup.step = STEP_WELCOME;
            self.set_page(Page::Welcome);
        }
        if let Some(pg) = go {
            self.go(pg);
        }
    }

    /// "Updates": the daily check, "Check now", the release notes and a
    /// Download button (opens the browser; nothing installs by itself).
    pub(super) fn updates_card(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let st = self.update_state.clone().unwrap_or_default();
        let available = st.available(VERSION).cloned();
        let busy = self.busy.contains("update-check");
        let mut check = false;
        let mut open: Option<String> = None;
        let u = &mut self.app.cfg.updates;
        card_with(
            ui,
            "About & updates",
            Some("PowerToys-style utilities for macOS and Windows. PowerMac asks GitHub for the latest release; nothing is downloaded or installed without you."),
            |ui| {
                let desc = if busy { "Checking…".to_string() } else { st.summary(VERSION) };
                let mut desc = desc;
                if st.checked_at > 0 && !busy {
                    desc.push_str(&format!(" Last checked {}.", ago_secs(st.checked_at)));
                }
                row(ui, &format!("PowerMac {VERSION}"), Some(&desc), |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if icon_text_button(ui, icon::ARROWS_CLOCKWISE, if busy { "Checking…" } else { "Check now" }).clicked() {
                            check = true;
                        }
                    });
                });
                toggle_row(ui, "Check for updates automatically", Some("Once a day while PowerMac runs."), &mut u.check);
                toggle_row(ui, "Include beta versions", Some("Offer pre-releases too."), &mut u.include_prereleases);
                if let Some(r) = &available {
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            icon_tile(ui, icon::ROCKET_LAUNCH, p.accent, 36.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(RichText::new(&r.name).font(theme::font_semibold(15.0)).color(p.text));
                                let mut d = format!("Version {}", r.version);
                                if r.prerelease {
                                    d.push_str(" (beta)");
                                }
                                if let Some(day) = r.published_at.get(..10) {
                                    d.push_str(&format!(" · published {day}"));
                                }
                                ui.label(RichText::new(d).size(12.0).color(p.text2));
                            });
                        });
                        if !r.notes.trim().is_empty() {
                            ui.add_space(8.0);
                            egui::Frame::new()
                                .fill(p.input)
                                .stroke(Stroke::new(1.0, p.card_border))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    let h = (r.notes.lines().count() as f32 * 19.0 + 6.0).min(180.0);
                                    egui::ScrollArea::vertical().id_salt("release-notes").min_scrolled_height(h).max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.y = 3.0;
                                        release_notes(ui, &r.notes);
                                    });
                                });
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let label = match (&r.download_name, r.download_size) {
                                (Some(_), Some(sz)) => format!("Download {} ({:.0} MB)", r.version, sz as f64 / 1_048_576.0),
                                (Some(_), None) => format!("Download {}", r.version),
                                _ => "Open the release page".to_string(),
                            };
                            if primary_icon_button(ui, icon::ARROW_DOWN, &label).clicked() {
                                open = Some(r.best_link().to_string());
                            }
                            if r.download_url.is_some() && icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Release page").clicked() {
                                open = Some(r.url.clone());
                            }
                        });
                        ui.add_space(4.0);
                        hint(ui, "Open the disk image and drag PowerMac to Applications, replacing this one. Your settings stay.");
                    });
                }
            },
        );
        if let Some(url) = open {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
        if check {
            self.app.cfg.updates.last_check = Some(pm_update::now_secs());
            let path = self.config_path.clone();
            let pre = self.app.cfg.updates.include_prereleases;
            self.run_background("update-check", move || {
                let r = pm_update::check(&pm_update::CheckOptions::new(VERSION, pre));
                let mut st: pm_update::UpdateState =
                    bridge::read(&path, bridge::UPDATE_STATUS_FILE).map(|s| s.data).unwrap_or_default();
                st.record(VERSION, r);
                bridge::write(&path, bridge::UPDATE_STATUS_FILE, &st).map_err(|e| format!("{e:#}"))?;
                Ok(st.summary(VERSION))
            });
        }
    }
}

#[cfg(test)]
impl SettingsApp {
    pub fn set_setup_step_for_test(&mut self, step: usize) {
        self.setup.step = step;
    }
    pub fn set_probe_for_test(&mut self, p: Probe, says_on: &[Perm]) {
        self.setup.probe = Some(p);
        self.setup.pinned = true;
        self.setup.says_on = says_on.iter().copied().collect();
    }
    pub fn set_update_state_for_test(&mut self, s: pm_update::UpdateState) {
        self.update_state = Some(s);
    }
    pub fn set_app_status_for_test(&mut self, s: Option<bridge::AppStatus>) {
        self.app_status = s;
    }
}

/// Start the app (`open -n` of the bundle, or this executable with `run`),
/// optionally reopening the settings on `page`.
pub(super) fn start_app(config_path: &Path, page: Option<&str>) -> Result<(), String> {
    let exe = crate::cli_exe().ok_or("PowerMac's executable isn't known here")?;
    let mut args: Vec<String> = vec!["--config".into(), config_path.display().to_string(), "run".into()];
    if let Some(p) = page {
        args.push("--open-settings".into());
        args.push(p.into());
    }
    let bundle = exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf);
    let r = match bundle {
        Some(b) if cfg!(target_os = "macos") => std::process::Command::new("open").arg("-n").arg(b).arg("--args").args(&args).spawn(),
        _ => std::process::Command::new(exe).args(&args).stdin(std::process::Stdio::null()).spawn(),
    };
    r.map(|_| ()).map_err(|e| format!("starting PowerMac: {e}"))
}

/// "5 min ago" for a Unix time in seconds.
fn ago_secs(t: u64) -> String {
    let s = pm_update::now_secs().saturating_sub(t);
    match s {
        0..=59 => "just now".into(),
        60..=5399 => format!("{} min ago", s / 60),
        5400..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// Markdown release notes, lightly formatted (headings, bullets, bold markers removed).
fn release_notes(ui: &mut Ui, md: &str) {
    let p = pal(ui);
    for line in md.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            ui.add_space(4.0);
            continue;
        }
        let clean = |s: &str| s.replace("**", "").replace('`', "");
        if let Some(h) = t.trim_start().strip_prefix('#') {
            ui.add_space(2.0);
            ui.label(RichText::new(clean(h.trim_start_matches('#').trim())).font(theme::font_semibold(13.0)).color(p.text));
        } else if let Some(b) = t.trim_start().strip_prefix("- ").or_else(|| t.trim_start().strip_prefix("* ")) {
            ui.add(egui::Label::new(RichText::new(format!("•  {}", clean(b))).size(12.5).color(p.text2)).wrap());
        } else {
            ui.add(egui::Label::new(RichText::new(clean(t)).size(12.5).color(p.text2)).wrap());
        }
    }
}

/// A status chip (icon + text) that draws the same in any layout direction.
pub(super) fn chip(ui: &mut Ui, c: Color32, glyph: &str, text: &str) -> egui::Response {
    let p = pal(ui);
    let font = theme::font(12.0);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), c).size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + 38.0, 26.0), Sense::hover());
    ui.painter().rect_filled(rect, 13.0, c.gamma_multiply(if p.dark { 0.22 } else { 0.12 }));
    ui.painter().text(rect.left_center() + vec2(15.0, 0.0), Align2::CENTER_CENTER, glyph, theme::icon_font(13.0), c);
    ui.painter().text(rect.left_center() + vec2(26.0, 0.0), Align2::LEFT_CENTER, text, font, c);
    resp
}

/// Title and subtitle of a setup step.
fn step_title(ui: &mut Ui, title: &str, text: &str) {
    let p = pal(ui);
    ui.add_space(18.0);
    ui.label(RichText::new(title).font(theme::font_semibold(25.0)).color(p.text));
    ui.add_space(4.0);
    ui.add(egui::Label::new(RichText::new(text).size(14.0).color(p.text2)).wrap());
}

/// A module chip of the features step: icon, name, a check when on.
fn feature_chip(ui: &mut Ui, label: &str, glyph: &str, on: bool, tint: Color32) -> egui::Response {
    let p = pal(ui);
    let font = theme::font(12.5);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), font.clone(), p.text).size().x);
    let size = vec2(tw + 52.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hover = resp.hovered();
    let fill = if on {
        theme::mix(p.card, tint, if p.dark { 0.30 } else { 0.14 })
    } else if hover {
        p.control_hover
    } else {
        p.control
    };
    let stroke = if on { theme::mix(p.card_border, tint, 0.7) } else { p.control_border };
    ui.painter().rect(rect, 14.0, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let c = if on { if p.dark { theme::mix(tint, Color32::WHITE, 0.35) } else { theme::mix(tint, Color32::BLACK, 0.15) } } else { p.text2 };
    ui.painter().text(rect.left_center() + vec2(14.0, 0.0), Align2::CENTER_CENTER, glyph, theme::icon_font(14.0), c);
    ui.painter().text(rect.left_center() + vec2(26.0, 0.0), Align2::LEFT_CENTER, label, font, if on { p.text } else { p.text2 });
    ui.painter().text(
        rect.right_center() - vec2(13.0, 0.0),
        Align2::CENTER_CENTER,
        if on { icon::CHECK } else { icon::PLUS },
        theme::icon_font(12.0),
        c,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The numbered steps at the top of the setup; returns a clicked step.
fn step_indicator(ui: &mut Ui, current: usize) -> Option<usize> {
    let p = pal(ui);
    let r = ui.max_rect();
    let n = STEPS.len();
    let span = (r.width() - 64.0).min(560.0);
    let x0 = r.center().x - span / 2.0;
    let dx = span / (n as f32 - 1.0);
    let cy = r.top() + 30.0;
    let mut clicked = None;
    for i in 0..n - 1 {
        let a = pos2(x0 + dx * i as f32 + 16.0, cy);
        let b = pos2(x0 + dx * (i + 1) as f32 - 16.0, cy);
        let c = if i < current { p.accent } else { p.card_border };
        ui.painter().line_segment([a, b], Stroke::new(2.0, c));
    }
    for (i, name) in STEPS.iter().enumerate() {
        let c = pos2(x0 + dx * i as f32, cy);
        let rect = Rect::from_center_size(c + vec2(0.0, 10.0), vec2(dx.min(120.0), 48.0));
        let resp = ui.interact(rect, Id::unique(("setup-step", i)), Sense::click());
        let done = i < current;
        let now = i == current;
        let fill = if done || now { p.accent } else { p.card };
        ui.painter().circle(c, 13.0, fill, Stroke::new(1.5, if done || now { p.accent } else { p.control_border }));
        if done {
            ui.painter().text(c, Align2::CENTER_CENTER, icon::CHECK, theme::icon_font(13.0), p.on_accent);
        } else {
            ui.painter().text(c, Align2::CENTER_CENTER, (i + 1).to_string(), theme::font_semibold(12.5), if now { p.on_accent } else { p.text2 });
        }
        ui.painter().text(
            c + vec2(0.0, 25.0),
            Align2::CENTER_CENTER,
            *name,
            if now { theme::font_semibold(12.0) } else { theme::font(12.0) },
            if now { p.text } else { p.text2 },
        );
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && i != current {
            clicked = Some(i);
        }
    }
    clicked
}
