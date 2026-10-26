//! The settings window: sidebar of modules, one page per module, and
//! debounced auto-save of `config.json` / `fancyzones.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use eframe::egui::{self, Color32, Id, RichText, Ui};
use fancyzones::{AppliedLayout, CustomLayout, LayoutType, WindowSystem};
use pm_config::AppConfig;

use crate::model::*;
use crate::record::{self, Recorded};
use crate::widgets::*;

/// Wait this long after the last change before writing the file.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the files are checked for changes made elsewhere (tray, editor).
const POLL: Duration = Duration::from_secs(1);
const DEFAULT_TARGET: &str = "";

/// Order-independent JSON (`HashMap` fields serialize in random order).
fn canonical<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).map(|v| v.to_string()).unwrap_or_default()
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Save bookkeeping for one file.
#[derive(Default)]
struct FileState {
    /// Draft as of the last frame (to notice edits).
    seen: serde_json::Value,
    /// When the draft last changed and hasn't been saved yet.
    changed_at: Option<Instant>,
    /// What was last read from / written to disk.
    written: String,
    mtime: Option<SystemTime>,
    problems: Vec<Problem>,
    /// The file exists but couldn't be read: never overwrite it.
    load_error: Option<String>,
    save_error: Option<String>,
}

impl FileState {
    fn track(&mut self, v: serde_json::Value, now: Instant) -> bool {
        if v != self.seen {
            self.seen = v;
            self.changed_at = Some(now);
            true
        } else {
            false
        }
    }

    fn clean(&self) -> bool {
        self.changed_at.is_none() && self.problems.is_empty()
    }
}

struct FzTarget {
    id: String,
    label: String,
    work_area: fancyzones::Rect,
}

pub struct SettingsApp {
    config_path: PathBuf,
    fz_path: PathBuf,
    page: Page,
    app: AppDraft,
    fz: fancyzones::Config,
    app_file: FileState,
    fz_file: FileState,
    last_poll: Instant,
    last_saved: Option<chrono::DateTime<Local>>,

    // UI state
    recording: Option<Id>,
    show_key: bool,
    confirm_new_key: bool,
    monitors: Vec<fancyzones::Monitor>,
    fz_selected: String,
    login: bool,
    login_error: Option<String>,
    perms: Option<(bool, bool)>,
    perms_at: Instant,
}

impl SettingsApp {
    pub fn new(config_path: PathBuf) -> Self {
        let fz_path = fancyzones_path(&config_path);
        let now = Instant::now();
        let mut s = SettingsApp {
            fz_path,
            page: Page::General,
            app: AppDraft::new(AppConfig::default(), Local::now()),
            fz: fancyzones::Config::default(),
            app_file: FileState::default(),
            fz_file: FileState::default(),
            last_poll: now,
            last_saved: None,
            recording: None,
            show_key: false,
            confirm_new_key: false,
            monitors: Vec::new(),
            fz_selected: DEFAULT_TARGET.into(),
            login: pm_config::login::is_enabled(),
            login_error: None,
            perms: crate::platform::permissions(),
            perms_at: now,
            config_path,
        };
        if !s.config_path.exists() {
            // Same as the tray app's first start: a config with a fresh key.
            let c = AppConfig::with_new_key();
            if let Err(e) = c.save(&s.config_path) {
                s.app_file.save_error = Some(format!("{e:#}"));
            }
        }
        s.reload_app();
        s.reload_fz();
        s.refresh_monitors();
        if let Some(m) = s.monitors.iter().find(|m| m.primary).or(s.monitors.first()) {
            s.fz_selected = m.id.clone();
        }
        s
    }

    fn refresh_monitors(&mut self) {
        self.monitors = fancyzones::platform::NativeWindowSystem::new().monitors().unwrap_or_default();
    }

    fn reload_app(&mut self) {
        let m = mtime(&self.config_path);
        self.app_file.mtime = m;
        match AppConfig::load_or_default(&self.config_path) {
            Ok(cfg) => {
                let now = Local::now();
                self.app = AppDraft::new(cfg, now);
                let f = &mut self.app_file;
                f.load_error = None;
                f.seen = serde_json::to_value(&self.app).unwrap_or_default();
                f.changed_at = None;
                f.problems = self.app.build(now).err().unwrap_or_default();
                f.written = self.app.build(now).map(|c| canonical(&c)).unwrap_or_default();
            }
            Err(e) => self.app_file.load_error = Some(format!("{e:#}")),
        }
    }

    fn reload_fz(&mut self) {
        self.fz_file.mtime = mtime(&self.fz_path);
        match load_fancyzones(&self.fz_path) {
            Ok(c) => {
                self.fz = c;
                let v = serde_json::to_value(FzEdit::of(&self.fz)).unwrap_or_default();
                let f = &mut self.fz_file;
                f.load_error = None;
                f.written = v.to_string();
                f.seen = v;
                f.changed_at = None;
                f.problems = validate_fancyzones(&self.fz);
            }
            Err(e) => self.fz_file.load_error = Some(format!("{e:#}")),
        }
    }

    fn save_app(&mut self) {
        let f = &mut self.app_file;
        f.changed_at = None;
        if f.load_error.is_some() {
            return;
        }
        match self.app.build(Local::now()) {
            Ok(cfg) => {
                f.problems.clear();
                let json = canonical(&cfg);
                if json == f.written {
                    return;
                }
                match cfg.save(&self.config_path) {
                    Ok(()) => {
                        f.written = json;
                        f.mtime = mtime(&self.config_path);
                        f.save_error = None;
                        self.last_saved = Some(Local::now());
                    }
                    Err(e) => f.save_error = Some(format!("{e:#}")),
                }
            }
            Err(p) => f.problems = p,
        }
    }

    fn save_fz(&mut self) {
        let f = &mut self.fz_file;
        f.changed_at = None;
        if f.load_error.is_some() {
            return;
        }
        f.problems = validate_fancyzones(&self.fz);
        if !f.problems.is_empty() {
            return;
        }
        let edit = FzEdit::of(&self.fz);
        let v = canonical(&edit);
        if v == f.written {
            return;
        }
        match save_fancyzones(&self.fz_path, &edit) {
            Ok(()) => {
                f.written = v;
                f.mtime = mtime(&self.fz_path);
                f.save_error = None;
                self.last_saved = Some(Local::now());
            }
            Err(e) => f.save_error = Some(format!("{e:#}")),
        }
    }

    /// Notice edits, save after the debounce, pick up changes made elsewhere.
    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.app_file.track(serde_json::to_value(&self.app).unwrap_or_default(), now) {
            self.app_file.problems = self.app.build(Local::now()).err().unwrap_or_default();
        }
        if self.fz_file.track(serde_json::to_value(FzEdit::of(&self.fz)).unwrap_or_default(), now) {
            self.fz_file.problems = validate_fancyzones(&self.fz);
        }
        for which in [0, 1] {
            let f = if which == 0 { &self.app_file } else { &self.fz_file };
            if let Some(t) = f.changed_at {
                let left = DEBOUNCE.saturating_sub(now - t);
                if left.is_zero() {
                    if which == 0 {
                        self.save_app()
                    } else {
                        self.save_fz()
                    }
                } else {
                    ctx.request_repaint_after(left);
                }
            }
        }
        if now - self.last_poll >= POLL {
            self.last_poll = now;
            if mtime(&self.config_path) != self.app_file.mtime && self.app_file.clean() {
                self.reload_app();
            }
            if mtime(&self.fz_path) != self.fz_file.mtime && self.fz_file.clean() {
                self.reload_fz();
            }
            if now - self.perms_at >= Duration::from_secs(2) {
                self.perms_at = now;
                self.perms = crate::platform::permissions();
            }
        }
        ctx.request_repaint_after(POLL);
    }

    fn flush(&mut self) {
        if self.app_file.changed_at.is_some() {
            self.save_app();
        }
        if self.fz_file.changed_at.is_some() {
            self.save_fz();
        }
    }

    fn problems(&self) -> impl Iterator<Item = &Problem> {
        self.app_file.problems.iter().chain(&self.fz_file.problems)
    }

    // -----------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(12.0);
        ui.heading("PowerMac");
        ui.weak("Settings");
        ui.add_space(12.0);
        for p in Page::ALL {
            let bad = self.problems().any(|x| x.page == p);
            let mut text = RichText::new(p.title()).size(14.0);
            if bad {
                text = text.color(ERROR);
            }
            let r = ui.add_sized([ui.available_width(), 30.0], egui::Button::selectable(self.page == p, text));
            if r.clicked() {
                self.page = p;
                self.recording = None;
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            if let Some(e) = &self.app_file.load_error {
                ui.colored_label(ERROR, format!("config.json couldn't be read, so nothing is saved: {e}"));
                if ui.button("Reload").clicked() {
                    self.reload_app();
                }
                return;
            }
            if let Some(e) = &self.fz_file.load_error {
                ui.colored_label(ERROR, format!("fancyzones.json couldn't be read, so FancyZones changes aren't saved: {e}"));
                if ui.button("Reload").clicked() {
                    self.reload_fz();
                }
                return;
            }
            let problems: Vec<Problem> = self.problems().cloned().collect();
            if !problems.is_empty() {
                ui.colored_label(ERROR, format!("Not saved — fix {}:", plural(problems.len(), "problem")));
                for p in problems.iter().take(3) {
                    if ui.link(format!("{}: {}", p.page.title(), p.message)).clicked() {
                        self.page = p.page;
                    }
                }
                return;
            }
            if let Some(e) = self.app_file.save_error.as_ref().or(self.fz_file.save_error.as_ref()) {
                ui.colored_label(ERROR, format!("Saving failed: {e}"));
                return;
            }
            if self.app_file.changed_at.is_some() || self.fz_file.changed_at.is_some() {
                ui.weak("Saving…");
            } else if let Some(t) = self.last_saved {
                ui.weak(format!("All changes saved ({}). The running PowerMac applies them within a second.", t.format("%H:%M:%S")));
            } else {
                ui.weak("Changes are saved automatically.");
            }
        });
    }

    fn page(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        ui.heading(RichText::new(self.page.title()).size(24.0));
        ui.add_space(8.0);
        match self.page {
            Page::General => self.general(ui),
            Page::Mwb => self.mwb(ui),
            Page::FancyZones => self.fancyzones(ui),
            Page::Awake => self.awake(ui),
            Page::ScrollReverser => self.scroll(ui),
            Page::MouseUtils => self.mouse_utils(ui),
            Page::KeyboardManager => self.keyboard(ui),
        }
        ui.add_space(24.0);
    }

    // -----------------------------------------------------------------------
    // General

    fn general(&mut self, ui: &mut Ui) {
        ui.label("Changes are saved automatically. A running PowerMac picks them up within about a second.");
        section(ui, "Configuration files", |ui| {
            grid(ui, "files", |ui| {
                ui.label("Settings");
                ui.add(egui::Label::new(RichText::new(self.config_path.display().to_string()).monospace()).selectable(true));
                ui.end_row();
                ui.label("FancyZones");
                ui.add(egui::Label::new(RichText::new(self.fz_path.display().to_string()).monospace()).selectable(true));
                ui.end_row();
            });
            if ui.button("Open folder").clicked() {
                if let Some(d) = self.config_path.parent() {
                    crate::platform::open_folder(d);
                }
            }
        });
        section(ui, "Startup", |ui| {
            let mut on = self.login;
            if ui.checkbox(&mut on, "Launch PowerMac at login").changed() {
                match pm_config::login::set_enabled(on) {
                    Ok(()) => self.login_error = None,
                    Err(e) => self.login_error = Some(format!("{e:#}")),
                }
                self.login = pm_config::login::is_enabled();
            }
            if let Some(e) = &self.login_error {
                error_label(ui, e);
            }
            ui.weak(if cfg!(target_os = "macos") {
                "Adds a LaunchAgent (~/Library/LaunchAgents/com.powermac.app.plist) that runs this PowerMac binary."
            } else {
                "Adds PowerMac to HKCU\\…\\CurrentVersion\\Run."
            });
        });
        if let Some((ax, input)) = self.perms {
            section(ui, "Permissions", |ui| {
                ui.label("Mouse Without Borders, FancyZones, Scroll Reverser, Mouse Utilities and Keyboard Manager need these. \
                          Grant them to PowerMac.app (or to the terminal that runs powermac), then restart PowerMac.");
                grid(ui, "perms", |ui| {
                    for (name, ok, pane) in
                        [("Accessibility", ax, "Privacy_Accessibility"), ("Input Monitoring", input, "Privacy_ListenEvent")]
                    {
                        ui.label(name);
                        if ok {
                            ui.colored_label(OK, "Granted");
                        } else {
                            ui.colored_label(ERROR, "Not granted");
                        }
                        if ui.button("Open System Settings").clicked() {
                            crate::platform::open_privacy_pane(pane);
                        }
                        ui.end_row();
                    }
                });
            });
        }
        section(ui, "Modules", |ui| {
            let c = &mut self.app.cfg;
            let mu = &mut c.mouse_utils;
            let mut mouse_any = mu.find_my_mouse.enabled || mu.highlighter.enabled || mu.crosshairs.enabled;
            let rows: [(Page, &mut bool); 6] = [
                (Page::Mwb, &mut c.mwb.enabled),
                (Page::FancyZones, &mut c.fancyzones.enabled),
                (Page::Awake, &mut c.awake.enabled),
                (Page::ScrollReverser, &mut c.scroll_reverser.enabled),
                (Page::MouseUtils, &mut mouse_any),
                (Page::KeyboardManager, &mut c.keyboard_manager.enabled),
            ];
            let mut go = None;
            grid(ui, "modules", |ui| {
                for (p, on) in rows {
                    toggle(ui, on);
                    if ui.link(p.title()).clicked() {
                        go = Some(p);
                    }
                    ui.end_row();
                }
            });
            let was = mu.find_my_mouse.enabled || mu.highlighter.enabled || mu.crosshairs.enabled;
            if mouse_any != was {
                mu.find_my_mouse.enabled = mouse_any;
                mu.highlighter.enabled = mouse_any;
                mu.crosshairs.enabled = mouse_any;
            }
            if let Some(p) = go {
                self.page = p;
            }
        });
        section(ui, "About", |ui| {
            ui.label(format!("PowerMac {}", env!("CARGO_PKG_VERSION")));
            ui.weak("PowerToys-style utilities for macOS and Windows.");
        });
    }

    // -----------------------------------------------------------------------
    // Mouse Without Borders

    fn mwb(&mut self, ui: &mut Ui) {
        enable_row(ui, "Enable Mouse Without Borders", &mut self.app.cfg.mwb.enabled);
        let this = self.app.this_machine();
        let d = &mut self.app;
        let s = &mut d.cfg.mwb.settings;

        section(ui, "Security key", |ui| {
            ui.label("Every machine in the matrix uses the same key. Copy it to the other machines (PowerToys → Mouse Without Borders).");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.key).password(!self.show_key).desired_width(280.0).font(egui::TextStyle::Monospace));
                if ui.button(if self.show_key { "Hide" } else { "Show" }).clicked() {
                    self.show_key = !self.show_key;
                }
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(mwb::normalize_key(&s.key));
                }
                if ui.button("Generate new key…").clicked() {
                    self.confirm_new_key = true;
                }
            });
            if let Some(e) = validate_key(&s.key) {
                error_label(ui, &e);
            }
        });
        if self.confirm_new_key {
            let r = egui::Modal::new(Id::unique("new-key")).show(ui.ctx(), |ui| {
                ui.set_width(360.0);
                ui.heading("Generate a new security key?");
                ui.label("The other machines stay disconnected until you enter the new key on each of them.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Generate").clicked() {
                        s.key = mwb::create_random_key();
                        self.show_key = true;
                        self.confirm_new_key = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_new_key = false;
                    }
                });
            });
            if r.should_close() {
                self.confirm_new_key = false;
            }
        }

        section(ui, "This machine", |ui| {
            grid(ui, "machine", |ui| {
                ui.label("Machine name");
                ui.add(egui::TextEdit::singleline(&mut s.machine_name).hint_text(format!("{this} (host name)")).desired_width(220.0));
                ui.end_row();
                ui.label("Announced as");
                ui.strong(&this);
                ui.end_row();
            });
            if let Some(e) = validate_machine_name(&s.machine_name) {
                error_label(ui, &e);
            }
        });

        section(ui, "Machine layout", |ui| {
            ui.label("Up to four machines, in the order the cursor moves between them. Use the same layout on every machine.");
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.one_row, true, "One row");
                ui.radio_value(&mut s.one_row, false, "Two rows (2×2)");
            });
            ui.add_space(4.0);
            let mut action = None;
            for i in 0..MAX_MACHINE {
                ui.horizontal(|ui| {
                    ui.label(format!("{}.", i + 1));
                    ui.add(egui::TextEdit::singleline(&mut d.matrix[i]).hint_text("(empty)").desired_width(200.0));
                    if ui.add_enabled(i > 0, egui::Button::new("Up").small()).clicked() {
                        action = Some((i, 0));
                    }
                    if ui.add_enabled(i + 1 < MAX_MACHINE, egui::Button::new("Down").small()).clicked() {
                        action = Some((i, 1));
                    }
                    if ui.add_enabled(!d.matrix[i].is_empty(), egui::Button::new("Clear").small()).clicked() {
                        action = Some((i, 2));
                    }
                    if is_this_machine(&d.matrix[i], &this) {
                        ui.colored_label(ACCENT, "this machine");
                    }
                });
            }
            match action {
                Some((i, 0)) => matrix_move_up(&mut d.matrix, i),
                Some((i, 1)) => matrix_move_down(&mut d.matrix, i),
                Some((i, _)) => d.matrix[i].clear(),
                None => {}
            }
            if !d.matrix.iter().any(|m| is_this_machine(m, &this)) {
                ui.horizontal(|ui| {
                    ui.colored_label(WARN, format!("{this} (this machine) isn't in the matrix."));
                    if ui.add_enabled(d.matrix.iter().any(|m| m.trim().is_empty()), egui::Button::new("Add it")).clicked() {
                        matrix_add(&mut d.matrix, &this);
                    }
                });
            }
            for e in validate_matrix(&d.matrix) {
                error_label(ui, &e);
            }
            ui.add_space(6.0);
            matrix_preview(ui, &d.matrix, s.one_row, &this);
        });

        section(ui, "Machine addresses", |ui| {
            ui.label("Machines missing here are found by name. Add an address (host, IP, or host:port) when a name doesn't resolve.");
            let mut remove = None;
            for i in 0..d.peers.len() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.peers[i].name).hint_text("Machine name").desired_width(160.0));
                    ui.add(egui::TextEdit::singleline(&mut d.peers[i].address).hint_text("192.168.1.20").desired_width(200.0));
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
                if let Some(e) = validate_peer(&d.peers, i) {
                    error_label(ui, &e);
                }
            }
            if let Some(i) = remove {
                d.peers.remove(i);
            }
            if ui.button("Add address").clicked() {
                d.peers.push(PeerRow::default());
            }
        });

        section(ui, "Behavior", |ui| {
            ui.checkbox(&mut s.easy_mouse, "Easy Mouse: switch machines when the cursor reaches a screen edge");
            ui.checkbox(&mut s.wrap_around, "Wrap around: leave the last machine to reach the first one");
            ui.checkbox(&mut s.hide_cursor_when_away, "Hide the cursor here while controlling another machine");
            ui.checkbox(&mut s.move_mouse_relatively, "Move the mouse relatively (try this if the cursor jumps)");
            ui.checkbox(&mut s.share_clipboard, "Share clipboard text");
            ui.checkbox(&mut s.swap_cmd_ctrl, "Swap Cmd and Ctrl (so Cmd+C here arrives as Ctrl+C on Windows)");
            ui.checkbox(&mut s.accept_remote_matrix, "Accept the machine layout sent by other machines");
        });

        section(ui, "Network", |ui| {
            grid(ui, "net", |ui| {
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut s.port).range(1..=65534));
                ui.end_row();
                ui.label("Listen address");
                ui.add(egui::TextEdit::singleline(&mut s.bind_address).desired_width(160.0));
                ui.end_row();
            });
            ui.weak(format!(
                "Messages use TCP {} (port + 1); allow it through the firewall on every machine. PowerToys' default port is 15100.",
                s.port.saturating_add(1)
            ));
        });
    }

    // -----------------------------------------------------------------------
    // FancyZones

    fn fz_targets(&self) -> Vec<FzTarget> {
        let mut out: Vec<FzTarget> = Vec::new();
        let s = &self.fz.settings;
        if s.span_zones_across_monitors && !self.monitors.is_empty() {
            let wa = self.monitors.iter().skip(1).fold(self.monitors[0].work_area, |a, m| a.union(&m.work_area));
            out.push(FzTarget {
                id: fancyzones::engine::SPAN_MONITOR_ID.into(),
                label: "All monitors (spanned)".into(),
                work_area: wa,
            });
        }
        for m in &self.monitors {
            let mut label = format!("{} ({}×{})", m.name, m.frame.width(), m.frame.height());
            if m.primary {
                label.push_str(" — main");
            }
            out.push(FzTarget { id: m.id.clone(), label, work_area: m.work_area });
        }
        for id in self.fz.applied_layouts.keys() {
            if !out.iter().any(|t| &t.id == id) {
                out.push(FzTarget {
                    id: id.clone(),
                    label: format!("{id} (not connected)"),
                    work_area: fancyzones::Rect::new(0, 0, 1920, 1080),
                });
            }
        }
        out.push(FzTarget {
            id: DEFAULT_TARGET.into(),
            label: "Default for other monitors".into(),
            work_area: fancyzones::Rect::new(0, 0, 1920, 1080),
        });
        out
    }

    fn fancyzones(&mut self, ui: &mut Ui) {
        enable_row(ui, "Enable FancyZones", &mut self.app.cfg.fancyzones.enabled);
        ui.weak("Layouts and settings are written to fancyzones.json; a running PowerMac applies them within about a second.");
        let targets = self.fz_targets();
        if !targets.iter().any(|t| t.id == self.fz_selected) {
            self.fz_selected = DEFAULT_TARGET.into();
        }

        section(ui, "Layouts", |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(240.0);
                    for t in &targets {
                        let has_own = self.fz.applied_layouts.contains_key(&t.id);
                        let mut text = RichText::new(&t.label);
                        if t.id != DEFAULT_TARGET && !has_own {
                            text = text.italics();
                        }
                        if ui.add(egui::Button::selectable(self.fz_selected == t.id, text)).clicked() {
                            self.fz_selected = t.id.clone();
                        }
                    }
                    if ui.small_button("Refresh monitors").clicked() {
                        self.refresh_monitors();
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(t) = targets.iter().find(|t| t.id == self.fz_selected) else { return };
                    let custom = self.fz.custom_layouts.clone();
                    let fz = &mut self.fz;
                    if t.id == DEFAULT_TARGET {
                        ui.label("Used by monitors without a layout of their own.");
                        layout_editor(ui, "default", &mut fz.settings.default_layout, &custom);
                    } else if fz.applied_layouts.contains_key(&t.id) {
                        let mut reset = false;
                        if let Some(l) = fz.applied_layouts.get_mut(&t.id) {
                            layout_editor(ui, &t.id, l, &custom);
                        }
                        if ui.button("Use the default layout instead").clicked() {
                            reset = true;
                        }
                        if reset {
                            fz.applied_layouts.remove(&t.id);
                        }
                    } else {
                        ui.label("This monitor uses the default layout.");
                        if ui.button("Give this monitor its own layout").clicked() {
                            fz.applied_layouts.insert(t.id.clone(), fz.settings.default_layout.clone());
                        }
                    }
                    let l = fz.layout_for(&t.id);
                    ui.add_space(8.0);
                    zones_preview(ui, &l, t.work_area, &custom, &fz.settings);
                    if let Some(e) = validate_layout(&l, &custom) {
                        error_label(ui, &format!("This layout can't be used: {e}"));
                    }
                });
            });
        });

        let s = &mut self.fz.settings;
        let mods = mod_names();
        section(ui, "Zones", |ui| {
            ui.checkbox(&mut s.shift_drag, "Hold Shift while dragging a window to show zones");
            ui.weak(if s.shift_drag { "" } else { "Zones show while dragging; holding Shift hides them." });
            ui.checkbox(&mut s.mouse_switch, "Press the right mouse button while dragging to toggle zones");
            ui.checkbox(&mut s.show_zones_on_all_monitors, "Show zones on all monitors while dragging");
            ui.checkbox(&mut s.span_zones_across_monitors, "Allow zones to span across monitors");
            ui.checkbox(&mut s.show_zone_number, "Show zone numbers");
            ui.checkbox(&mut s.restore_size, "Restore a window's original size when it leaves a zone");
            ui.checkbox(&mut s.app_last_zone_move_windows, "Move newly opened windows to the zone they last used");
            grid(ui, "overlap", |ui| {
                ui.label("Overlapping zones");
                use fancyzones::layout::OverlappingZonesAlgorithm as O;
                combo(
                    ui,
                    "overlap",
                    &mut s.overlapping_zones_algorithm,
                    &[
                        (O::Smallest, "Activate the smallest zone"),
                        (O::Largest, "Activate the largest zone"),
                        (O::Positional, "Split by position"),
                        (O::ClosestCenter, "Activate the zone whose center is closest"),
                    ],
                );
                ui.end_row();
            });
        });

        section(ui, "Keyboard snapping", |ui| {
            ui.checkbox(&mut s.override_snap_hotkeys, "Snap the focused window with a hotkey + arrow keys");
            ui.add_enabled_ui(s.override_snap_hotkeys, |ui| {
                grid(ui, "snapkeys", |ui| {
                    ui.label("Snap: modifiers + arrow");
                    modifiers_edit(ui, &mut s.snap_modifiers, &mods);
                    ui.end_row();
                    ui.label("Extend: modifiers + arrow");
                    modifiers_edit(ui, &mut s.extend_modifiers, &mods);
                    ui.end_row();
                });
                if s.snap_modifiers == fancyzones::Modifiers::default() {
                    error_label(ui, "Pick at least one modifier for snapping, or the hotkeys won't fire.");
                }
                ui.radio_value(&mut s.move_windows_based_on_position, false, "Left/Right cycle through zones by number");
                ui.radio_value(&mut s.move_windows_based_on_position, true, "All four arrows move by position (enables Extend)");
                ui.checkbox(&mut s.move_window_across_monitors, "Move windows between monitors");
            });
        });

        section(ui, "Edge snapping", |ui| {
            ui.checkbox(&mut s.edge_snap_enabled, "Drag a window to a screen edge or corner to snap it (with zones hidden)");
            ui.add_enabled_ui(s.edge_snap_enabled, |ui| {
                grid(ui, "edge", |ui| {
                    ui.label("Edge distance");
                    ui.add(egui::Slider::new(&mut s.edge_snap_margin, 1..=50).suffix(" px"));
                    ui.end_row();
                    ui.label("Corner size");
                    ui.add(egui::Slider::new(&mut s.edge_snap_corner_size, 0..=200).suffix(" px"));
                    ui.end_row();
                    ui.label("Gap between windows");
                    ui.add(egui::Slider::new(&mut s.edge_snap_gap, 0..=50).suffix(" px"));
                    ui.end_row();
                    ui.label("Top edge");
                    use fancyzones::{BottomEdgeAction as B, TopEdgeAction as T};
                    combo(ui, "top", &mut s.edge_snap_top, &[(T::Maximize, "Maximize"), (T::TopHalf, "Top half"), (T::None, "Nothing")]);
                    ui.end_row();
                    ui.label("Bottom edge");
                    combo(
                        ui,
                        "bottom",
                        &mut s.edge_snap_bottom,
                        &[(B::BottomHalf, "Bottom half"), (B::Thirds, "Thirds (by cursor position)"), (B::None, "Nothing")],
                    );
                    ui.end_row();
                });
            });
            if cfg!(windows) {
                ui.weak("Windows Snap already does this; leave it off unless you've turned Snap off.");
            }
        });

        section(ui, "Appearance", |ui| {
            grid(ui, "colors", |ui| {
                for (label, c) in [
                    ("Zone color", &mut s.zone_color),
                    ("Border color", &mut s.zone_border_color),
                    ("Highlight color", &mut s.zone_highlight_color),
                    ("Number color", &mut s.zone_number_color),
                ] {
                    ui.label(label);
                    hex_color_edit(ui, c);
                    ui.end_row();
                }
                ui.label("Zone opacity");
                ui.add(egui::Slider::new(&mut s.zone_highlight_opacity, 0..=100).suffix(" %"));
                ui.end_row();
            });
        });
    }

    // -----------------------------------------------------------------------
    // Awake

    fn awake(&mut self, ui: &mut Ui) {
        enable_row(ui, "Enable Awake", &mut self.app.cfg.awake.enabled);
        let a = &mut self.app.awake;
        section(ui, "Behavior", |ui| {
            for k in AwakeKind::ALL {
                ui.radio_value(&mut a.kind, k, k.label());
            }
            match a.kind {
                AwakeKind::Timed => {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut a.hours).range(0..=999).suffix(" h"));
                        ui.add(egui::DragValue::new(&mut a.minutes).range(0..=59).suffix(" min"));
                    });
                    ui.weak("Counted from when the setting is applied.");
                }
                AwakeKind::Until => {
                    ui.add(egui::TextEdit::singleline(&mut a.until).hint_text("HH:MM or YYYY-MM-DD HH:MM").desired_width(180.0));
                    let now = Local::now();
                    match a.to_config(now) {
                        Ok(c) => {
                            if let awake::AwakeMode::Expirable(t) = c.mode {
                                ui.weak(format!("Until {}", t.format("%a %e %b %Y, %H:%M")));
                            }
                        }
                        Err(e) => error_label(ui, &e),
                    }
                    if let Some(w) = a.warning(now) {
                        ui.colored_label(WARN, w);
                    }
                }
                AwakeKind::Off | AwakeKind::Indefinite => {}
            }
            if let (AwakeKind::Timed, Err(e)) = (a.kind, a.to_config(Local::now())) {
                error_label(ui, &e);
            }
            ui.add_space(4.0);
            ui.checkbox(&mut a.keep_display_on, "Keep the screen on");
        });
        ui.weak("The Awake menu in the tray changes the current state without editing these settings.");
    }

    // -----------------------------------------------------------------------
    // Scroll Reverser

    fn scroll(&mut self, ui: &mut Ui) {
        let s = &mut self.app.cfg.scroll_reverser;
        enable_row(ui, "Enable Scroll Reverser", &mut s.enabled);
        section(ui, "Reverse scrolling", |ui| {
            egui::Grid::new("axes").num_columns(3).spacing([24.0, 8.0]).show(ui, |ui| {
                ui.label("");
                ui.strong("Vertical");
                ui.strong("Horizontal");
                ui.end_row();
                ui.label("Mouse wheel");
                ui.checkbox(&mut s.reverse_mouse_vertical, "");
                ui.checkbox(&mut s.reverse_mouse_horizontal, "");
                ui.end_row();
                ui.label("Trackpad");
                ui.checkbox(&mut s.reverse_trackpad_vertical, "");
                ui.checkbox(&mut s.reverse_trackpad_horizontal, "");
                ui.end_row();
            });
            if cfg!(windows) {
                ui.add_space(6.0);
                ui.checkbox(&mut s.windows_touchpad_heuristic, "Treat small wheel steps as touchpad scrolling");
                ui.weak("Windows doesn't say which device scrolled. Precision touchpads send small steps, but so do some high-resolution mice.");
            }
        });
        if cfg!(target_os = "macos") {
            let natural = scroll_reverser::system_natural_scrolling();
            section(ui, "macOS natural scrolling", |ui| {
                ui.label(match natural {
                    Some(true) => "Natural scrolling is on in System Settings, so these switches flip that: reversing the mouse wheel gives classic wheel scrolling while the trackpad stays natural.",
                    Some(false) => "Natural scrolling is off in System Settings, so reversing a device here makes it scroll the natural way.",
                    None => "Reversal is relative to the System Settings natural scrolling switch.",
                });
                ui.weak("Mouse Without Borders input and Magic Mouse scrolling: the Magic Mouse counts as a trackpad.");
            });
        }
    }

    // -----------------------------------------------------------------------
    // Mouse Utilities

    fn mouse_utils(&mut self, ui: &mut Ui) {
        let rec = &mut self.recording;
        let d = &mut self.app;
        let m = &mut d.cfg.mouse_utils;
        ui.weak("Each tool has its own switch. Shortcuts work while PowerMac is running.");

        section(ui, "Find My Mouse", |ui| {
            let f = &mut m.find_my_mouse;
            enable_row(ui, "Enable Find My Mouse", &mut f.enabled);
            ui.add_enabled_ui(f.enabled, |ui| {
                use mouse_utils::ActivationMethod as A;
                grid(ui, "find", |ui| {
                    ui.label("Activate by");
                    combo(
                        ui,
                        "activation",
                        &mut f.activation_method,
                        &[
                            (A::DoubleLeftControl, "Press Left Ctrl twice"),
                            (A::DoubleRightControl, "Press Right Ctrl twice"),
                            (A::ShakeMouse, "Shaking the mouse"),
                            (A::DoubleLeftControlOrShake, "Left Ctrl twice or shaking"),
                            (A::Shortcut, "A shortcut"),
                        ],
                    );
                    ui.end_row();
                    if f.activation_method == A::Shortcut {
                        ui.label("Shortcut");
                        hotkey_edit(ui, rec, Id::unique("find-hk"), &mut d.find_hotkey, RecMode::Combo);
                        ui.end_row();
                    }
                    ui.label("Spotlight radius");
                    ui.add(egui::Slider::new(&mut f.spotlight_radius, 20..=500).suffix(" pt"));
                    ui.end_row();
                    ui.label("Backdrop color");
                    color_edit(ui, &mut f.backdrop_color, false);
                    ui.end_row();
                    ui.label("Backdrop opacity");
                    percent_slider(ui, &mut f.overlay_opacity);
                    ui.end_row();
                    ui.label("Animation");
                    ui.add(egui::DragValue::new(&mut f.animation_duration_ms).range(0..=5000).suffix(" ms"));
                    ui.end_row();
                    ui.label("Initial zoom");
                    ui.add(egui::Slider::new(&mut f.initial_zoom, 1..=15).suffix("×"));
                    ui.end_row();
                    ui.label("Hide after idle");
                    ui.add(egui::DragValue::new(&mut f.idle_timeout_ms).range(0..=10000).suffix(" ms"));
                    ui.end_row();
                    if f.activation_method.uses_shake() {
                        ui.label("Shake distance");
                        ui.add(egui::DragValue::new(&mut f.shake_minimum_distance).range(100..=10000).suffix(" px"));
                        ui.end_row();
                        ui.label("Shake time window");
                        ui.add(egui::DragValue::new(&mut f.shake_interval_ms).range(100..=10000).suffix(" ms"));
                        ui.end_row();
                        ui.label("Shake factor");
                        ui.add(egui::DragValue::new(&mut f.shake_factor).range(100..=2000).suffix(" %"));
                        ui.end_row();
                    }
                });
                if f.activation_method == A::Shortcut {
                    if let Err(e) = parse_hotkey(&d.find_hotkey) {
                        error_label(ui, &e);
                    }
                }
            });
        });

        section(ui, "Mouse Highlighter", |ui| {
            let h = &mut m.highlighter;
            enable_row(ui, "Enable Mouse Highlighter", &mut h.enabled);
            ui.add_enabled_ui(h.enabled, |ui| {
                grid(ui, "hl", |ui| {
                    ui.label("Toggle shortcut");
                    hotkey_edit(ui, rec, Id::unique("hl-hk"), &mut d.highlighter_hotkey, RecMode::Combo);
                    ui.end_row();
                    ui.label("Left button color");
                    color_edit(ui, &mut h.left_button_color, true);
                    ui.end_row();
                    ui.label("Right button color");
                    color_edit(ui, &mut h.right_button_color, true);
                    ui.end_row();
                    ui.label("Radius");
                    ui.add(egui::Slider::new(&mut h.radius, 5..=100).suffix(" pt"));
                    ui.end_row();
                    ui.label("Fade delay");
                    ui.add(egui::DragValue::new(&mut h.fade_delay_ms).range(0..=5000).suffix(" ms"));
                    ui.end_row();
                    ui.label("Fade duration");
                    ui.add(egui::DragValue::new(&mut h.fade_duration_ms).range(0..=5000).suffix(" ms"));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut h.highlight_cursor, "Also highlight the cursor when no button is pressed");
                    ui.end_row();
                    if h.highlight_cursor {
                        ui.label("Cursor color");
                        color_edit(ui, &mut h.always_color, true);
                        ui.end_row();
                    }
                    ui.label("");
                    ui.checkbox(&mut h.auto_activate, "Turn on when PowerMac starts");
                    ui.end_row();
                });
                if let Err(e) = parse_hotkey(&d.highlighter_hotkey) {
                    error_label(ui, &e);
                }
            });
        });

        section(ui, "Mouse Pointer Crosshairs", |ui| {
            let c = &mut m.crosshairs;
            enable_row(ui, "Enable crosshairs", &mut c.enabled);
            ui.add_enabled_ui(c.enabled, |ui| {
                use mouse_utils::Orientation as O;
                grid(ui, "ch", |ui| {
                    ui.label("Toggle shortcut");
                    hotkey_edit(ui, rec, Id::unique("ch-hk"), &mut d.crosshairs_hotkey, RecMode::Combo);
                    ui.end_row();
                    ui.label("Color");
                    color_edit(ui, &mut c.color, false);
                    ui.end_row();
                    ui.label("Border color");
                    color_edit(ui, &mut c.border_color, false);
                    ui.end_row();
                    ui.label("Opacity");
                    percent_slider(ui, &mut c.opacity);
                    ui.end_row();
                    ui.label("Center gap");
                    ui.add(egui::Slider::new(&mut c.center_radius, 0..=500).suffix(" pt"));
                    ui.end_row();
                    ui.label("Thickness");
                    ui.add(egui::Slider::new(&mut c.thickness, 1..=50).suffix(" pt"));
                    ui.end_row();
                    ui.label("Border size");
                    ui.add(egui::Slider::new(&mut c.border_size, 0..=20).suffix(" pt"));
                    ui.end_row();
                    ui.label("Lines");
                    combo(ui, "orient", &mut c.orientation, &[(O::Both, "Horizontal and vertical"), (O::Vertical, "Vertical only"), (O::Horizontal, "Horizontal only")]);
                    ui.end_row();
                    ui.label("Line length");
                    ui.horizontal(|ui| {
                        let mut fixed = c.fixed_length > 0;
                        if ui.checkbox(&mut fixed, "Fixed").changed() {
                            c.fixed_length = if fixed { 100 } else { 0 };
                        }
                        if fixed {
                            ui.add(egui::DragValue::new(&mut c.fixed_length).range(1..=5000).suffix(" pt"));
                        } else {
                            ui.weak("to the screen edges");
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut c.auto_activate, "Turn on when PowerMac starts");
                    ui.end_row();
                });
                if let Err(e) = parse_hotkey(&d.crosshairs_hotkey) {
                    error_label(ui, &e);
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // Keyboard Manager

    fn keyboard(&mut self, ui: &mut Ui) {
        enable_row(ui, "Enable Keyboard Manager", &mut self.app.cfg.keyboard_manager.enabled);
        let rec = &mut self.recording;
        let d = &mut self.app;
        ui.weak(
            "Key names: A–Z, 0–9, F1–F24, Esc, Tab, Space, Enter, Backspace, Delete, Left/Right/Up/Down, Home, End, PageUp, PageDown, \
             CapsLock, punctuation (; = , - . / ` [ \\ ] '). Modifiers: Ctrl, Shift, Alt/Option, Win/Cmd, with L/R for one side (LCtrl, ROption). \
             Shortcuts join keys with +, like Ctrl+Shift+V.",
        );

        section(ui, "Remap keys", |ui| {
            remap_table(ui, rec, &mut d.key_remaps, false);
            if ui.button("Add key remap").clicked() {
                d.key_remaps.push(RemapRow::new());
            }
        });
        section(ui, "Remap shortcuts", |ui| {
            remap_table(ui, rec, &mut d.shortcut_remaps, true);
            if ui.button("Add shortcut remap").clicked() {
                d.shortcut_remaps.push(RemapRow::new());
            }
        });

        // Runtime warnings (duplicates etc.) for the rows that parse.
        let km = keyboard_manager::Config {
            remap_keys: d.key_remaps.iter().filter_map(|r| r.key_remap().ok()).collect(),
            remap_shortcuts: d.shortcut_remaps.iter().filter_map(|r| r.shortcut_remap().ok()).collect(),
            ..Default::default()
        };
        for w in km.validate() {
            ui.colored_label(WARN, w);
        }

        section(ui, "Paste as plain text", |ui| {
            let p = &mut d.cfg.keyboard_manager.paste_plain_text;
            enable_row(ui, "Enable paste as plain text", &mut p.enabled);
            ui.add_enabled_ui(p.enabled, |ui| {
                grid(ui, "paste", |ui| {
                    ui.label("Shortcut");
                    hotkey_edit(ui, rec, Id::unique("paste-hk"), &mut d.paste_hotkey, RecMode::Combo);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut p.restore_clipboard, "Put the original clipboard back afterwards");
                    ui.end_row();
                    if p.restore_clipboard {
                        ui.label("Restore after");
                        ui.add(egui::DragValue::new(&mut p.restore_delay_ms).range(50..=10000).suffix(" ms"));
                        ui.end_row();
                    }
                });
                if let Err(e) = parse_shortcut(&d.paste_hotkey) {
                    error_label(ui, &e);
                }
            });
            ui.weak("Works independently of the Enable Keyboard Manager switch above.");
        });
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl SettingsApp {
    /// One frame of the whole window.
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.flush();
        }
        egui::Panel::left("nav").exact_size(220.0).show(ui, |ui| self.sidebar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(4.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_max_width(760.0);
                self.page(ui);
            });
        });
        self.tick(&ctx);
    }
}

// ---------------------------------------------------------------------------
// Composite widgets that need model types

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecMode {
    /// Modifiers + key.
    Combo,
    /// One key, no modifiers.
    Key,
}

/// A text field in the crates' key syntax plus a "Record" button that
/// captures the next key press in this window.
fn hotkey_edit(ui: &mut Ui, rec: &mut Option<Id>, id: Id, text: &mut String, mode: RecMode) {
    ui.horizontal(|ui| {
        if *rec == Some(id) {
            match record::take(ui.ctx()) {
                Some(Recorded::Combo(c)) => {
                    *text = match mode {
                        RecMode::Combo => c.to_text(),
                        RecMode::Key => keyboard_manager::keys::key_name(c.vk),
                    };
                    *rec = None;
                }
                Some(Recorded::Cancel) => *rec = None,
                None => {}
            }
            let label = if mode == RecMode::Key { "Press a key… (Esc cancels)" } else { "Press a shortcut… (Esc cancels)" };
            if ui.add(egui::Button::selectable(true, label)).clicked() {
                *rec = None;
            }
        } else {
            ui.add(egui::TextEdit::singleline(text).desired_width(170.0).hint_text("none"));
            if ui.small_button("Record").on_hover_text("Press the keys in this window").clicked() {
                *rec = Some(id);
            }
        }
    });
}

fn remap_table(ui: &mut Ui, rec: &mut Option<Id>, rows: &mut Vec<RemapRow>, shortcuts: bool) {
    if rows.is_empty() {
        ui.weak("Nothing remapped yet.");
    }
    let mut remove = None;
    let salt = if shortcuts { "sc" } else { "key" };
    for (i, r) in rows.iter_mut().enumerate() {
        let id = Id::unique((salt, i));
        ui.horizontal_wrapped(|ui| {
            let from_mode = if shortcuts { RecMode::Combo } else { RecMode::Key };
            hotkey_edit(ui, rec, id.with("from"), &mut r.from, from_mode);
            ui.label("→");
            egui::ComboBox::from_id_salt(id.with("kind")).selected_text(r.kind.label()).width(150.0).show_ui(ui, |ui| {
                for k in TargetKind::ALL {
                    ui.selectable_value(&mut r.kind, k, k.label());
                }
            });
            match r.kind {
                TargetKind::Keys => hotkey_edit(ui, rec, id.with("to"), &mut r.value, RecMode::Combo),
                TargetKind::Text => {
                    ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("Text to type").desired_width(200.0));
                }
                TargetKind::OpenUri => {
                    ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("https://… or a path").desired_width(200.0));
                }
                TargetKind::Run => {
                    ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("/Applications/App.app").desired_width(180.0));
                    ui.add(egui::TextEdit::singleline(&mut r.args).hint_text("arguments").desired_width(100.0));
                }
                TargetKind::Disabled | TargetKind::PastePlainText => {}
            }
            if shortcuts {
                ui.add(egui::TextEdit::singleline(&mut r.app).hint_text("all apps").desired_width(130.0))
                    .on_hover_text("Only in this app: a bundle id or app name (macOS), or a process name like chrome.exe (Windows)");
            }
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
        if !r.is_blank() {
            let res = if shortcuts { r.shortcut_remap().map(|_| ()) } else { r.key_remap().map(|_| ()) };
            if let Err(e) = res {
                error_label(ui, &e);
            }
        }
        ui.add_space(2.0);
    }
    if let Some(i) = remove {
        rows.remove(i);
        *rec = None;
    }
}

fn layout_editor(ui: &mut Ui, salt: &str, l: &mut AppliedLayout, custom: &[CustomLayout]) {
    let kinds = [
        (LayoutType::Blank, "No layout"),
        (LayoutType::Focus, "Focus"),
        (LayoutType::Columns, "Columns"),
        (LayoutType::Rows, "Rows"),
        (LayoutType::Grid, "Grid"),
        (LayoutType::PriorityGrid, "Priority grid"),
    ];
    let current = if l.kind == LayoutType::Custom {
        fancyzones::layout::find_custom(custom, &l.uuid).map(|c| format!("Custom: {}", c.name)).unwrap_or("Custom (missing)".into())
    } else {
        kinds.iter().find(|(k, _)| *k == l.kind).map(|(_, n)| n.to_string()).unwrap_or_default()
    };
    grid(ui, ("layout", salt), |ui| {
        ui.label("Layout");
        egui::ComboBox::from_id_salt(("kind", salt)).selected_text(current).width(200.0).show_ui(ui, |ui| {
            for (k, name) in kinds {
                if ui.selectable_label(l.kind == k, name).clicked() && l.kind != k {
                    let count = if l.kind == LayoutType::Custom || l.zone_count < 1 { 3 } else { l.zone_count };
                    let (show, spacing, radius) = (l.show_spacing, l.spacing, l.sensitivity_radius);
                    *l = AppliedLayout::template(k, count).with_spacing(show, spacing);
                    l.sensitivity_radius = radius;
                }
            }
            for c in custom {
                let on = l.kind == LayoutType::Custom && fancyzones::layout::guid_eq(&l.uuid, &c.uuid);
                if ui.selectable_label(on, format!("Custom: {}", c.name)).clicked() {
                    *l = AppliedLayout::custom(c);
                }
            }
        });
        ui.end_row();
        let template = !matches!(l.kind, LayoutType::Blank | LayoutType::Custom);
        ui.label("Zones");
        ui.add_enabled(template, egui::DragValue::new(&mut l.zone_count).range(1..=40));
        ui.end_row();
        ui.label("Space around zones");
        ui.horizontal(|ui| {
            ui.checkbox(&mut l.show_spacing, "");
            ui.add_enabled(l.show_spacing, egui::DragValue::new(&mut l.spacing).range(fancyzones::layout::MAX_NEGATIVE_SPACING..=100).suffix(" px"));
        });
        ui.end_row();
        ui.label("Distance to highlight adjacent zones");
        ui.add(egui::DragValue::new(&mut l.sensitivity_radius).range(0..=200).suffix(" px"));
        ui.end_row();
    });
    if !custom.is_empty() || l.kind == LayoutType::Custom {
        ui.weak("Custom layouts come from fancyzones.json (import them from the PowerToys FancyZones editor).");
    }
}

/// The zones of `l` on `work_area`, scaled into a small picture.
fn zones_preview(ui: &mut Ui, l: &AppliedLayout, work_area: fancyzones::Rect, custom: &[CustomLayout], s: &fancyzones::Settings) {
    let (w, h) = (work_area.width().max(1) as f32, work_area.height().max(1) as f32);
    let pw = ui.available_width().clamp(160.0, 380.0);
    let ph = (pw * h / w).clamp(60.0, 300.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(pw, ph), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    p.rect_stroke(rect, 4.0, ui.visuals().widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
    let Ok(layout) = fancyzones::Layout::new(l, work_area, custom) else { return };
    let (sx, sy) = (pw / w, ph / h);
    let fill = hex_to_color32(&s.zone_color, Color32::from_rgb(0xAA, 0xCD, 0xFF));
    let alpha = (s.zone_highlight_opacity.min(100) as f32 / 100.0 * 255.0) as u8;
    let fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), alpha.max(60));
    let border = hex_to_color32(&s.zone_border_color, Color32::WHITE);
    let num = hex_to_color32(&s.zone_number_color, Color32::BLACK);
    for (i, z) in layout.zones.iter().enumerate() {
        let r = egui::Rect::from_min_max(
            rect.min + egui::vec2((z.left - work_area.left) as f32 * sx, (z.top - work_area.top) as f32 * sy),
            rect.min + egui::vec2((z.right - work_area.left) as f32 * sx, (z.bottom - work_area.top) as f32 * sy),
        )
        .shrink(0.5);
        p.rect(r, 2.0, fill, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
        if s.show_zone_number {
            p.text(r.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(13.0), num);
        }
    }
    if layout.zones.is_empty() {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, "No zones", egui::FontId::proportional(13.0), ui.visuals().weak_text_color());
    }
}

fn matrix_preview(ui: &mut Ui, slots: &[String; MAX_MACHINE], one_row: bool, this: &str) {
    let (cols, rows) = if one_row { (4, 1) } else { (2, 2) };
    let cell = egui::vec2(120.0, 44.0);
    let gap = 6.0;
    let size = egui::vec2(cols as f32 * (cell.x + gap) - gap, rows as f32 * (cell.y + gap) - gap);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    for (i, s) in slots.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        let min = rect.min + egui::vec2(c as f32 * (cell.x + gap), r as f32 * (cell.y + gap));
        let b = egui::Rect::from_min_size(min, cell);
        let me = is_this_machine(s, this);
        let fill = if me {
            ACCENT.gamma_multiply(0.35)
        } else if s.trim().is_empty() {
            Color32::TRANSPARENT
        } else {
            ui.visuals().faint_bg_color
        };
        p.rect(b, 6.0, fill, ui.visuals().widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
        let text = if s.trim().is_empty() { "—".to_string() } else { s.trim().to_string() };
        p.text(b.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), ui.visuals().text_color());
    }
}

fn modifiers_edit(ui: &mut Ui, m: &mut fancyzones::Modifiers, names: &[(keyboard_manager::keys::ModFamily, &str); 4]) {
    use keyboard_manager::keys::ModFamily as F;
    ui.horizontal(|ui| {
        for (f, n) in names {
            let v = match f {
                F::Ctrl => &mut m.ctrl,
                F::Alt => &mut m.alt,
                F::Shift => &mut m.shift,
                F::Win => &mut m.meta,
            };
            ui.checkbox(v, *n);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render every page headlessly (catches panics in layout code), then
    /// check an edit lands in the files.
    #[test]
    fn pages_render_and_save() {
        let dir = std::env::temp_dir().join(format!("pm-settings-ui-{}", std::process::id()));
        let path = dir.join("config.json");
        let mut cfg = AppConfig::with_new_key();
        cfg.keyboard_manager = keyboard_manager::Config::from_json(
            r#"{"remapKeys":[{"from":"CapsLock","to":"Esc"}],
                "remapShortcuts":[{"from":"Ctrl+Alt+T","to":{"run":{"path":"/bin/echo"}}}]}"#,
        )
        .unwrap();
        cfg.mwb.settings.peers.insert("pc".into(), "10.0.0.2".into());
        cfg.mouse_utils.find_my_mouse.activation_method = mouse_utils::ActivationMethod::Shortcut;
        cfg.save(&path).unwrap();
        let mut fz = fancyzones::Config::default();
        fz.applied_layouts.insert("gone".into(), AppliedLayout::template(LayoutType::Grid, 4));
        fz.settings.span_zones_across_monitors = true;
        fz.save_to(&fancyzones_path(&path)).unwrap();

        let mut app = SettingsApp::new(path.clone());
        assert!(app.problems().next().is_none(), "{:?}", app.problems().collect::<Vec<_>>());
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 720.0))),
            ..Default::default()
        };
        let frame = |app: &mut SettingsApp| {
            let mut out = ctx.run_ui(input(), |ui| app.draw(ui));
            out.textures_delta.clear();
        };
        for p in Page::ALL {
            app.page = p;
            if p == Page::FancyZones {
                for t in app.fz_targets() {
                    app.fz_selected = t.id;
                    frame(&mut app);
                }
            }
            for _ in 0..3 {
                frame(&mut app);
            }
        }
        app.confirm_new_key = true;
        app.page = Page::Mwb;
        frame(&mut app);
        // Nothing was edited, so nothing was written.
        assert!(app.app_file.changed_at.is_none() && app.fz_file.changed_at.is_none());

        app.app.cfg.awake.enabled = true;
        app.app.awake.kind = AwakeKind::Indefinite;
        app.fz.settings.shift_drag = false;
        frame(&mut app);
        assert!(app.app_file.changed_at.is_some());
        app.flush();
        let back = AppConfig::load_or_default(&path).unwrap();
        assert!(back.awake.enabled && back.awake.settings.mode == awake::AwakeMode::Indefinite);
        assert_eq!(back.mwb.settings.peers["pc"], "10.0.0.2");
        let fz_back = load_fancyzones(&fancyzones_path(&path)).unwrap();
        assert!(!fz_back.settings.shift_drag);
        assert!(fz_back.applied_layouts.contains_key("gone"));

        // An invalid edit blocks saving and is reported on its page.
        app.app.cfg.mwb.settings.key = "short".into();
        frame(&mut app);
        app.flush();
        assert!(app.problems().any(|p| p.page == Page::Mwb));
        assert_ne!(AppConfig::load_or_default(&path).unwrap().mwb.settings.key, "short");
        std::fs::remove_dir_all(&dir).ok();
    }
}
