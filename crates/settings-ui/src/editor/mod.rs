//! The FancyZones layout editor (a port of PowerToys' `FancyZonesEditor`):
//! a gallery of templates and custom layouts per monitor, template settings,
//! creating / renaming / duplicating / deleting custom layouts, layout
//! hotkeys 0–9, and the grid and canvas zone editors (in the window, or at
//! real size on the monitor with "Edit on screen").
//!
//! It runs as `powermac editor [--monitor ID]` and only edits
//! `fancyzones.json`; the running app applies changes within about a second.

pub mod canvas;
pub mod gallery;
pub mod grid;
pub mod store;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, pos2, vec2, Align, Align2, Layout, RichText, Sense, Stroke, Ui};
use fancyzones::layout::{
    compute_relative_zones, guid_eq, CustomLayout, CustomLayoutInfo, LayoutType,
};
use fancyzones::{AppliedLayout, Monitor, WindowSystem};

use crate::theme::{self, icon, pal};
use crate::widgets::{self, error_label};
use canvas::CanvasEditor;
use gallery::TEMPLATES;
use grid::{GridEditor, GridModel};
use store::DEFAULT_TARGET;

/// How often `fancyzones.json` is checked for changes made elsewhere.
const POLL: Duration = Duration::from_secs(1);
const FALLBACK_WORK_AREA: fancyzones::Rect = fancyzones::Rect { left: 0, top: 0, right: 1920, bottom: 1080 };

pub fn zones_label(n: usize) -> String {
    if n == 1 {
        "1 zone".into()
    } else {
        format!("{n} zones")
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A monitor (or the default / spanned pseudo-monitor) layouts apply to.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: String,
    pub label: String,
    pub work_area: fancyzones::Rect,
    /// Backing scale factor / DPI scale of the monitor.
    pub scale: f64,
    pub connected: bool,
}

impl Target {
    /// The monitor's name without its number and size.
    pub fn short_name(&self) -> String {
        if self.id == DEFAULT_TARGET {
            return "monitors without a layout of their own".into();
        }
        if self.id == fancyzones::engine::SPAN_MONITOR_ID {
            return "all monitors".into();
        }
        if !self.connected {
            return self.id.clone();
        }
        let l = self.label.split(" (").next().unwrap_or(&self.label);
        let l = l
            .split_once(". ")
            .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, r)| r)
            .unwrap_or(l);
        l.to_string()
    }

    pub fn work(&self) -> (i32, i32) {
        (self.work_area.width().max(1), self.work_area.height().max(1))
    }
}

/// The same choices as the settings page: connected monitors, the spanned
/// "all monitors" area, monitors that only exist in the file, the default.
pub fn targets(monitors: &[Monitor], cfg: &fancyzones::Config) -> Vec<Target> {
    let mut out = Vec::new();
    if cfg.settings.span_zones_across_monitors && !monitors.is_empty() {
        let wa = monitors.iter().skip(1).fold(monitors[0].work_area, |a, m| a.union(&m.work_area));
        out.push(Target {
            id: fancyzones::engine::SPAN_MONITOR_ID.into(),
            label: "All monitors (spanned)".into(),
            work_area: wa,
            scale: monitors[0].scale,
            connected: true,
        });
    }
    for (i, m) in monitors.iter().enumerate() {
        let mut label = format!("{}. {} ({}×{})", i + 1, m.name, m.frame.width(), m.frame.height());
        if m.primary {
            label.push_str(" — main");
        }
        out.push(Target { id: m.id.clone(), label, work_area: m.work_area, scale: m.scale, connected: true });
    }
    for id in cfg.applied_layouts.keys() {
        if !out.iter().any(|t| &t.id == id) {
            out.push(Target { id: id.clone(), label: format!("{id} (not connected)"), work_area: FALLBACK_WORK_AREA, scale: 1.0, connected: false });
        }
    }
    out.push(Target { id: DEFAULT_TARGET.into(), label: "Default for other monitors".into(), work_area: FALLBACK_WORK_AREA, scale: 1.0, connected: false });
    out
}

pub enum ZoneEditor {
    Grid(GridEditor),
    Canvas(CanvasEditor),
}

/// An open zone editor (PowerToys' grid / canvas editor window).
pub struct Session {
    pub uuid: String,
    pub name: String,
    pub is_new: bool,
    pub editor: ZoneEditor,
    pub target: Target,
    /// Editing in a borderless window covering the monitor.
    pub on_screen: bool,
}

impl Session {
    pub fn layout(&self) -> CustomLayout {
        let info = match &self.editor {
            ZoneEditor::Grid(g) => CustomLayoutInfo::Grid(g.to_info()),
            ZoneEditor::Canvas(c) => CustomLayoutInfo::Canvas(c.to_info()),
        };
        CustomLayout { uuid: self.uuid.clone(), name: self.name.trim().to_string(), info }
    }

    fn open(layout: &CustomLayout, target: Target, is_new: bool) -> Self {
        let editor = match &layout.info {
            CustomLayoutInfo::Grid(g) => ZoneEditor::Grid(GridEditor::new(g, target.work())),
            CustomLayoutInfo::Canvas(c) => ZoneEditor::Canvas(CanvasEditor::new(c, target.work())),
        };
        Session { uuid: layout.uuid.clone(), name: layout.name.clone(), is_new, editor, target, on_screen: false }
    }
}

enum Dialog {
    /// Template settings (index into `TEMPLATES`).
    Template { index: usize, layout: AppliedLayout },
    Custom { uuid: String, name: String, show_spacing: bool, spacing: i32, sensitivity: i32, hotkey: Option<u8>, grid: bool },
    New { name: String, canvas: bool },
    ConfirmDelete { uuid: String, name: String },
}

enum Action {
    Apply(AppliedLayout),
    EditTemplate(usize),
    EditCustom(String),
    New,
}

pub struct EditorApp {
    fz_path: PathBuf,
    snap: fancyzones::Config,
    load_error: Option<String>,
    status: Option<(String, bool)>,
    mtime: Option<SystemTime>,
    last_poll: Instant,
    monitors: Vec<Monitor>,
    selected: String,
    /// Current parameters of each template (zone count, spacing, distance).
    templates: Vec<AppliedLayout>,
    dialog: Option<Dialog>,
    pub session: Option<Session>,
}

impl EditorApp {
    /// Editor for `fz_path`, starting on `monitor` (default: the main monitor).
    pub fn new(fz_path: PathBuf, monitor: Option<String>) -> Self {
        let monitors = fancyzones::platform::NativeWindowSystem::new().monitors().unwrap_or_default();
        Self::with_monitors(fz_path, monitor, monitors)
    }

    pub fn with_monitors(fz_path: PathBuf, monitor: Option<String>, monitors: Vec<Monitor>) -> Self {
        let mut s = EditorApp {
            fz_path,
            snap: fancyzones::Config::default(),
            load_error: None,
            status: None,
            mtime: None,
            last_poll: Instant::now(),
            monitors,
            selected: DEFAULT_TARGET.into(),
            templates: TEMPLATES.iter().map(|t| gallery::default_template(t.0)).collect(),
            dialog: None,
            session: None,
        };
        s.reload();
        s.templates = TEMPLATES.iter().map(|t| s.snap.template(t.0)).collect();
        let ts = s.targets();
        s.selected = monitor
            .filter(|m| ts.iter().any(|t| &t.id == m))
            .or_else(|| s.monitors.iter().find(|m| m.primary).or(s.monitors.first()).map(|m| m.id.clone()))
            .unwrap_or_else(|| DEFAULT_TARGET.into());
        s.sync_templates();
        s
    }

    fn reload(&mut self) {
        self.mtime = mtime(&self.fz_path);
        match store::load(&self.fz_path) {
            Ok(s) => {
                self.snap = s;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("{e:#}")),
        }
    }

    pub fn targets(&self) -> Vec<Target> {
        targets(&self.monitors, &self.snap)
    }

    fn target(&self) -> Target {
        let ts = self.targets();
        ts.iter().find(|t| t.id == self.selected).or(ts.last()).cloned().expect("default target")
    }

    pub fn select(&mut self, id: &str) {
        self.selected = id.to_string();
        self.sync_templates();
    }

    /// Show the selected monitor's template parameters on its template card.
    fn sync_templates(&mut self) {
        let l = store::applied_for(&self.snap, &self.selected);
        if let Some(i) = TEMPLATES.iter().position(|t| t.0 == l.kind && l.kind != LayoutType::Custom) {
            self.templates[i] = l;
        }
    }

    /// Re-read the file, apply `f`, write it. Updates the status line.
    fn commit(&mut self, done: &str, f: impl FnOnce(&mut fancyzones::Config)) -> bool {
        if let Some(e) = &self.load_error {
            self.status = Some((format!("fancyzones.json couldn't be read, so nothing was saved: {e}"), true));
            return false;
        }
        match store::modify(&self.fz_path, f) {
            Ok(s) => {
                self.snap = s;
                self.mtime = mtime(&self.fz_path);
                let t = chrono::Local::now().format("%H:%M:%S");
                self.status = Some((format!("{done} ({t}). The running PowerMac applies it within a second."), false));
                true
            }
            Err(e) => {
                self.status = Some((format!("Saving failed: {e:#}"), true));
                false
            }
        }
    }

    fn poll(&mut self) {
        let now = Instant::now();
        if now - self.last_poll >= POLL {
            self.last_poll = now;
            if mtime(&self.fz_path) != self.mtime {
                self.reload();
            }
        }
    }

    fn open_custom_editor(&mut self, uuid: &str) {
        let Some(l) = fancyzones::layout::find_custom(&self.snap.custom_layouts, uuid).cloned() else { return };
        self.session = Some(Session::open(&l, self.target(), false));
    }

    pub fn open_new_layout(&mut self, name: &str, canvas: bool) {
        let target = self.target();
        let name = if name.trim().is_empty() { "Custom layout" } else { name.trim() };
        let name = store::unique_name(&self.snap.custom_layouts, name);
        let info = if canvas {
            CustomLayoutInfo::Canvas(CanvasEditor::new_layout(target.work()).to_info())
        } else {
            CustomLayoutInfo::Grid(GridModel::columns(3).to_info())
        };
        let l = CustomLayout { uuid: store::new_uuid(), name, info };
        self.session = Some(Session::open(&l, target, true));
    }

    /// Save the open zone editor (new layouts are applied to its monitor).
    pub fn save_session(&mut self) -> bool {
        let Some(s) = &self.session else { return false };
        let layout = s.layout();
        let (target, is_new) = (s.target.id.clone(), s.is_new);
        let name = layout.name.clone();
        let ok = self.commit(&format!("Saved “{name}”"), |snap| {
            store::upsert_custom(snap, layout.clone());
            if is_new {
                store::apply_layout(snap, &target, AppliedLayout::custom(&layout));
            }
        });
        if ok {
            self.session = None;
        }
        ok
    }

    // -----------------------------------------------------------------------
    // UI

    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        let p = pal(ui);
        self.poll();
        if self.session.is_some() {
            self.session_ui(ui);
        } else {
            egui::Panel::bottom("editor-status")
                .frame(
                    egui::Frame::new()
                        .fill(p.sidebar)
                        .inner_margin(egui::Margin::symmetric(24, 8))
                        .stroke(Stroke::new(1.0, p.card_border)),
                )
                .show(ui, |ui| self.status_bar(ui));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(p.bg))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Frame::new()
                                .inner_margin(egui::Margin {
                                    left: 32,
                                    right: 32,
                                    top: 26,
                                    bottom: 32,
                                })
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    self.gallery(ui);
                                });
                        });
                });
        }
        self.dialogs(&ctx);
        ctx.request_repaint_after(POLL);
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(e) = self.load_error.clone() {
                ui.label(theme::icon_text(icon::WARNING_CIRCLE).size(14.0).color(p.error));
                ui.add(egui::Label::new(RichText::new(format!("fancyzones.json couldn't be read, so nothing is saved: {e}")).size(12.5).color(p.error)).truncate());
                if ui.button("Reload").clicked() {
                    self.reload();
                }
            } else if let Some((m, err)) = &self.status {
                let (g, c) = if *err { (icon::WARNING_CIRCLE, p.error) } else { (icon::CHECK_CIRCLE, p.ok) };
                ui.label(RichText::new(g).size(14.0).color(c));
                ui.add(egui::Label::new(RichText::new(m).size(12.5).color(if *err { p.error } else { p.text2 })).truncate());
            } else {
                ui.label(theme::icon_text(icon::INFO).size(14.0).color(p.text3));
                ui.add(egui::Label::new(RichText::new("Click a layout to apply it. Changes are saved to fancyzones.json right away.").size(12.5).color(p.text2)).truncate());
            }
        });
    }

    fn gallery(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let targets = self.targets();
        let target = self.target();
        // Header.
        ui.horizontal(|ui| {
            crate::app::app_logo(ui, 40.0);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    RichText::new("FancyZones Layouts")
                        .font(theme::font_semibold(23.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(format!("Choose the layout for {}.", target.short_name()))
                        .color(p.text2),
                );
            });
        });
        ui.add_space(18.0);
        // Monitor picker.
        let mut pick = None;
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for t in &targets {
                if monitor_chip(ui, t, self.selected == t.id).clicked() {
                    pick = Some(t.id.clone());
                }
            }
            if widgets::icon_button(ui, icon::ARROWS_CLOCKWISE, "Refresh monitors").clicked() {
                refresh = true;
            }
        });
        if let Some(id) = pick {
            self.select(&id);
        }
        if refresh {
            self.monitors = fancyzones::platform::NativeWindowSystem::new()
                .monitors()
                .unwrap_or_default();
        }
        let cfg = &self.snap;
        let applied = store::applied_for(cfg, &target.id);
        if target.id != DEFAULT_TARGET && !cfg.applied_layouts.contains_key(&target.id) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon_text(icon::INFO).size(13.0).color(p.text3));
                ui.label(
                    RichText::new("This monitor uses the default layout until you pick one.")
                        .size(12.5)
                        .color(p.text2),
                );
            });
        }
        let custom = cfg.custom_layouts.clone();
        let work = target.work();
        let mut action = None;

        section_header(ui, "Templates", None);
        let templates = self.templates.clone();
        card_grid(ui, TEMPLATES.len(), |ui, i, w| {
            let (kind, name, _) = TEMPLATES[i];
            let active = applied.kind == kind;
            let r = gallery::card(ui, w, name, &templates[i], &custom, work, active, None);
            if r.edit {
                action = Some(Action::EditTemplate(i));
            } else if r.clicked {
                action = Some(Action::Apply(templates[i].clone()));
            }
        });

        let hint_text = (!self.snap.layout_hotkeys.is_empty()).then_some("The number on a layout is its hotkey: press it with the layout-switch modifiers to apply it.");
        section_header(ui, "Custom layouts", hint_text);
        let hotkeys = self.snap.layout_hotkeys.clone();
        card_grid(ui, custom.len() + 1, |ui, i, w| {
            if i == custom.len() {
                if gallery::new_card(ui, w) {
                    action = Some(Action::New);
                }
                return;
            }
            let c = &custom[i];
            let a = AppliedLayout::custom(c);
            let active = applied.kind == LayoutType::Custom && guid_eq(&applied.uuid, &c.uuid);
            let hk = store::hotkey_for(&hotkeys, &c.uuid);
            let r = gallery::card(ui, w, &c.name, &a, &custom, work, active, hk);
            if r.edit {
                action = Some(Action::EditCustom(c.uuid.clone()));
            } else if r.clicked {
                action = Some(Action::Apply(a));
            }
        });

        match action {
            Some(Action::Apply(l)) => {
                let name = match l.kind {
                    LayoutType::Custom => fancyzones::layout::find_custom(&custom, &l.uuid).map(|c| c.name.clone()).unwrap_or_default(),
                    k => gallery::template_name(k).to_string(),
                };
                let id = target.id.clone();
                self.commit(&format!("Applied “{name}” to {}", target.short_name()), |s| store::apply_layout(s, &id, l));
            }
            Some(Action::EditTemplate(i)) => self.dialog = Some(Dialog::Template { index: i, layout: self.templates[i].clone() }),
            Some(Action::EditCustom(uuid)) => {
                if let Some(c) = fancyzones::layout::find_custom(&custom, &uuid) {
                    let (show_spacing, spacing, sensitivity, grid) = match &c.info {
                        CustomLayoutInfo::Grid(g) => (g.show_spacing, g.spacing, g.sensitivity_radius, true),
                        CustomLayoutInfo::Canvas(cv) => (false, 0, cv.sensitivity_radius, false),
                    };
                    self.dialog = Some(Dialog::Custom {
                        uuid,
                        name: c.name.clone(),
                        show_spacing,
                        spacing,
                        sensitivity,
                        hotkey: store::hotkey_for(&self.snap.layout_hotkeys, &c.uuid),
                        grid,
                    });
                }
            }
            Some(Action::New) => {
                let name = store::unique_name(&custom, "Custom layout");
                self.dialog = Some(Dialog::New { name, canvas: false });
            }
            None => {}
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let target = self.target();
        let mut keep = true;
        let frame = {
            let p = theme::pal_ctx(ctx);
            egui::Frame::new()
                .fill(p.card)
                .stroke(Stroke::new(1.0, p.card_border))
                .corner_radius(14.0)
                .inner_margin(egui::Margin::same(22))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 40,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(if p.dark { 140 } else { 60 }),
                })
        };
        let kind = match &dialog {
            Dialog::Template { .. } => 0,
            Dialog::Custom { .. } => 1,
            Dialog::New { .. } => 2,
            Dialog::ConfirmDelete { .. } => 3,
        };
        // One id per kind: a modal remembers its size.
        let modal = egui::Modal::new(egui::Id::unique(("fz-editor-dialog", kind))).frame(frame).show(ctx, |ui| {
            ui.set_width(400.0);
            widgets::reset_rows(ui);
            match &mut dialog {
                Dialog::Template { index, layout } => {
                    let (kind, name, _) = TEMPLATES[*index];
                    crate::app::dialog_title(ui, icon::SQUARES_FOUR, name);
                    widgets::hint(ui, &format!("Template settings, applied to {}.", target.short_name()));
                    ui.add_space(8.0);
                    gallery::preview(ui, vec2(400.0, 150.0), layout, &[], target.work(), true);
                    ui.add_space(6.0);
                    if gallery::has_zone_count(kind) {
                        widgets::row(ui, "Number of zones", None, |ui| ui.add(egui::DragValue::new(&mut layout.zone_count).range(1..=gallery::MAX_ZONES)));
                    }
                    if gallery::has_spacing(kind) {
                        widgets::row(ui, "Space around zones", None, |ui| {
                            ui.add_enabled(
                                layout.show_spacing,
                                egui::DragValue::new(&mut layout.spacing).range(fancyzones::layout::MAX_NEGATIVE_SPACING..=1000).suffix(" px"),
                            );
                            widgets::toggle_labeled(ui, &mut layout.show_spacing, "Space around zones");
                        });
                    }
                    widgets::row(ui, "Highlight distance", Some("Neighbouring zones within this distance light up too."), |ui| {
                        ui.add(egui::DragValue::new(&mut layout.sensitivity_radius).range(0..=1000).suffix(" px"))
                    });
                    let err = compute_relative_zones(layout, target.work().0, target.work().1, &[]).err();
                    if let Some(e) = &err {
                        error_label(ui, &format!("This layout doesn't fit {}: {e}", target.short_name()));
                    }
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled_ui(err.is_none(), |ui| widgets::primary_button(ui, "Save and apply")).inner.clicked() {
                            let (l, id, i) = (layout.clone(), target.id.clone(), *index);
                            self.templates[i] = l.clone();
                            self.commit(&format!("Applied “{name}” to {}", target.short_name()), |s| {
                                store::set_template(s, l.clone());
                                store::apply_layout(s, &id, l);
                            });
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Custom { uuid, name, show_spacing, spacing, sensitivity, hotkey, grid } => {
                    crate::app::dialog_title(ui, icon::PENCIL_SIMPLE, "Edit layout");
                    let hotkeys = self.snap.layout_hotkeys.clone();
                    let custom = self.snap.custom_layouts.clone();
                    if let Some(c) = fancyzones::layout::find_custom(&custom, uuid) {
                        ui.add_space(4.0);
                        gallery::preview(ui, vec2(400.0, 150.0), &AppliedLayout::custom(c), &custom, target.work(), true);
                        ui.add_space(6.0);
                    }
                    widgets::row(ui, "Name", None, |ui| ui.add(egui::TextEdit::singleline(name).desired_width(200.0)));
                    if *grid {
                        widgets::row(ui, "Space around zones", None, |ui| {
                            ui.add_enabled(*show_spacing, egui::DragValue::new(spacing).range(fancyzones::layout::MAX_NEGATIVE_SPACING..=1000).suffix(" px"));
                            widgets::toggle_labeled(ui, show_spacing, "Space around zones");
                        });
                    }
                    widgets::row(ui, "Highlight distance", None, |ui| ui.add(egui::DragValue::new(sensitivity).range(0..=1000).suffix(" px")));
                    let label = |k: Option<u8>| match k {
                        None => "None".to_string(),
                        Some(k) => {
                            let other = hotkeys
                                .iter()
                                .find(|h| h.key == k && !guid_eq(&h.layout_uuid, uuid))
                                .and_then(|h| fancyzones::layout::find_custom(&custom, &h.layout_uuid))
                                .map(|c| format!("  (now {})", c.name))
                                .unwrap_or_default();
                            format!("{k}{other}")
                        }
                    };
                    widgets::row(ui, "Layout hotkey", Some("Press it with the layout-switch modifiers."), |ui| {
                        egui::ComboBox::from_id_salt("layout-hotkey").selected_text(label(*hotkey)).width(120.0).show_ui(ui, |ui| {
                            ui.selectable_value(hotkey, None, label(None));
                            for k in 0..=9u8 {
                                ui.selectable_value(hotkey, Some(k), label(Some(k)));
                            }
                        });
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if widgets::icon_text_button(ui, icon::GRID_FOUR, "Edit zones…").clicked() {
                            let u = uuid.clone();
                            self.open_custom_editor(&u);
                            keep = false;
                        }
                        if widgets::icon_text_button(ui, icon::COPY, "Duplicate").clicked() {
                            let u = uuid.clone();
                            self.commit("Duplicated the layout", |s| {
                                store::duplicate_custom(s, &u);
                            });
                            keep = false;
                        }
                        if ui.add(egui::Button::new(theme::icon_job(icon::TRASH, "Delete…", 13.0, pal(ui).error))).clicked() {
                            dialog_next(&mut self.dialog, Dialog::ConfirmDelete { uuid: uuid.clone(), name: name.clone() });
                            keep = false;
                        }
                    });
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let valid = !name.trim().is_empty();
                        if ui.add_enabled_ui(valid, |ui| widgets::primary_button(ui, "Save")).inner.clicked() {
                            let (u, n, ss, sp, sr, hk) = (uuid.clone(), name.trim().to_string(), *show_spacing, *spacing, *sensitivity, *hotkey);
                            self.commit(&format!("Saved “{n}”"), |s| {
                                if let Some(mut c) = fancyzones::layout::find_custom(&s.custom_layouts, &u).cloned() {
                                    c.name = n.clone();
                                    match &mut c.info {
                                        CustomLayoutInfo::Grid(g) => {
                                            g.show_spacing = ss;
                                            g.spacing = sp;
                                            g.sensitivity_radius = sr;
                                        }
                                        CustomLayoutInfo::Canvas(cv) => cv.sensitivity_radius = sr,
                                    }
                                    store::upsert_custom(s, c);
                                }
                                store::set_hotkey(s, &u, hk);
                            });
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::New { name, canvas } => {
                    crate::app::dialog_title(ui, icon::PLUS, "Create new layout");
                    widgets::hint(ui, &format!("The new layout is applied to {} when you save it.", target.short_name()));
                    ui.add_space(10.0);
                    widgets::row(ui, "Name", None, |ui| ui.add(egui::TextEdit::singleline(name).desired_width(200.0)));
                    widgets::block(ui, |ui| {
                        let w = (ui.available_width() - 12.0) / 2.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            if kind_tile(ui, w, icon::GRID_FOUR, "Grid", "Split and merge zones. Zones never overlap.", !*canvas).clicked() {
                                *canvas = false;
                            }
                            if kind_tile(ui, w, icon::STACK, "Canvas", "Free-form zones that may overlap.", *canvas).clicked() {
                                *canvas = true;
                            }
                        });
                    });
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::primary_button(ui, "Create").clicked() {
                            let (n, c) = (name.clone(), *canvas);
                            self.open_new_layout(&n, c);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmDelete { uuid, name } => {
                    crate::app::dialog_title(ui, icon::TRASH, "Delete layout?");
                    widgets::hint(ui, &format!("“{name}” will be deleted. Monitors using it switch to the default layout."));
                    ui.add_space(18.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let p = pal(ui);
                        if ui.add(egui::Button::new(RichText::new("Delete").font(theme::font_semibold(13.0)).color(egui::Color32::WHITE)).fill(p.error).stroke(Stroke::NONE)).clicked() {
                            let u = uuid.clone();
                            self.commit(&format!("Deleted “{name}”"), |s| store::delete_custom(s, &u));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        if modal.should_close() {
            keep = false;
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    fn session_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let mut save = false;
        let mut cancel = false;
        let Some(s) = self.session.as_mut() else {
            return;
        };
        egui::Panel::top("zone-editor-toolbar")
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin {
                        left: 20,
                        right: 20,
                        top: 12,
                        bottom: 10,
                    })
                    .stroke(Stroke::new(1.0, p.card_border)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(theme::icon_job(
                                icon::CARET_LEFT,
                                "Layouts",
                                13.5,
                                p.accent,
                            ))
                            .frame(false),
                        )
                        .on_hover_text("Discard changes and go back")
                        .clicked()
                    {
                        cancel = true;
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if s.is_new {
                            "New layout"
                        } else {
                            "Edit layout"
                        })
                        .font(theme::font_semibold(15.0))
                        .color(p.text),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut s.name)
                            .desired_width(200.0)
                            .hint_text("Layout name"),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(!s.name.trim().is_empty(), |ui| {
                                widgets::primary_button(ui, "Save")
                            })
                            .inner
                            .clicked()
                        {
                            save = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        let on_screen_label = if s.on_screen {
                            "Edit in this window"
                        } else {
                            "Edit on screen"
                        };
                        if ui
                            .add_enabled_ui(s.target.connected, |ui| {
                                widgets::icon_text_button(ui, icon::MONITOR, on_screen_label)
                            })
                            .inner
                            .on_hover_text("Edit the zones at real size on the monitor")
                            .clicked()
                        {
                            s.on_screen = !s.on_screen;
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    toolbar(ui, &mut s.editor);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon_text(icon::INFO).size(13.0).color(p.text3));
                    ui.add(
                        egui::Label::new(RichText::new(hint(&s.editor)).size(12.0).color(p.text2))
                            .truncate(),
                    );
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                if s.on_screen {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Editing on {} — use the toolbar at the top of that screen.",
                                s.target.short_name()
                            ))
                            .color(p.text2),
                        );
                    });
                } else {
                    let avail = ui.available_rect_before_wrap().shrink(28.0);
                    let rect = fit(avail, s.target.work());
                    // Monitor bezel around the editor.
                    let bezel = rect.expand(7.0);
                    ui.painter().rect_filled(
                        bezel.translate(vec2(0.0, 3.0)),
                        14.0,
                        egui::Color32::from_black_alpha(if p.dark { 90 } else { 30 }),
                    );
                    ui.painter().rect_filled(
                        bezel,
                        14.0,
                        if p.dark {
                            egui::Color32::from_rgb(0x0E, 0x0E, 0x10)
                        } else {
                            egui::Color32::from_rgb(0x2A, 0x2A, 0x2E)
                        },
                    );
                    show_editor(ui, &mut s.editor, rect);
                }
            });
        if s.on_screen {
            let (back, sv, cn) = on_screen(&ctx, s);
            if back {
                s.on_screen = false;
            }
            save |= sv;
            cancel |= cn;
        }
        if save {
            self.save_session();
        } else if cancel {
            self.session = None;
            self.status = Some(("Changes discarded.".into(), false));
        }
    }
}

/// A section title in the gallery.
fn section_header(ui: &mut Ui, title: &str, hint: Option<&str>) {
    let p = pal(ui);
    ui.add_space(26.0);
    ui.label(
        RichText::new(title)
            .font(theme::font_semibold(15.0))
            .color(p.text),
    );
    if let Some(h) = hint {
        ui.label(RichText::new(h).size(12.0).color(p.text2));
    }
    ui.add_space(10.0);
}

/// `n` cards in rows that fill the available width; `add(ui, i, card_w)`.
fn card_grid(ui: &mut Ui, n: usize, mut add: impl FnMut(&mut Ui, usize, f32)) {
    let gap = 16.0;
    let w = ui.available_width();
    let cols = (((w + gap) / (gallery::CARD_MIN_W + gap)).floor() as usize).clamp(1, 6);
    let card_w = ((w - gap * (cols as f32 - 1.0)) / cols as f32).min(gallery::CARD_MAX_W);
    let mut i = 0;
    while i < n {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for _ in 0..cols {
                if i >= n {
                    break;
                }
                add(ui, i, card_w);
                i += 1;
            }
        });
        ui.add_space(gap - ui.spacing().item_spacing.y);
    }
}

/// A monitor in the picker: icon, name and resolution in a pill.
fn monitor_chip(ui: &mut Ui, t: &Target, selected: bool) -> egui::Response {
    let p = pal(ui);
    let (glyph, title, detail) = if t.id == DEFAULT_TARGET {
        (
            icon::STACK,
            "Other monitors".to_string(),
            "Default".to_string(),
        )
    } else if t.id == fancyzones::engine::SPAN_MONITOR_ID {
        (
            icon::MONITOR,
            "All monitors".to_string(),
            "Spanned".to_string(),
        )
    } else if !t.connected {
        (icon::MONITOR, t.id.clone(), "Not connected".to_string())
    } else {
        let detail = t
            .label
            .split(" (")
            .nth(1)
            .and_then(|r| r.split(')').next())
            .unwrap_or("")
            .replace('×', " × ");
        (icon::MONITOR, t.short_name(), detail)
    };
    let tf = theme::font_semibold(12.5);
    let df = theme::font(11.5);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(title.clone(), tf.clone(), p.text).size().x);
    let dw = ui.fonts_mut(|f| {
        f.layout_no_wrap(detail.clone(), df.clone(), p.text)
            .size()
            .x
    });
    let size = vec2(40.0 + tw.max(dw) + 14.0, 44.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    let (fill, stroke) = if selected {
        (p.accent_soft, Stroke::new(1.5, p.accent))
    } else if resp.hovered() {
        (p.control_hover, Stroke::new(1.0, p.card_border))
    } else {
        (p.card, Stroke::new(1.0, p.card_border))
    };
    painter.rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
    painter.text(
        pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        glyph,
        theme::icon_font(17.0),
        if selected { p.accent } else { p.text2 },
    );
    let italic = !t.connected && t.id != DEFAULT_TARGET;
    painter.text(
        pos2(rect.left() + 38.0, rect.center().y - 7.5),
        Align2::LEFT_CENTER,
        title,
        tf,
        if italic { p.text2 } else { p.text },
    );
    painter.text(
        pos2(rect.left() + 38.0, rect.center().y + 8.5),
        Align2::LEFT_CENTER,
        detail,
        df,
        p.text2,
    );
    resp
}

/// One of the two layout kinds in the "Create new layout" dialog.
fn kind_tile(ui: &mut Ui, w: f32, glyph: &str, title: &str, desc: &str, selected: bool) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 108.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    let (fill, stroke) = if selected {
        (p.accent_soft, Stroke::new(1.5, p.accent))
    } else if resp.hovered() {
        (p.control_hover, Stroke::new(1.0, p.card_border))
    } else {
        (p.control, Stroke::new(1.0, p.card_border))
    };
    painter.rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
    painter.text(
        rect.left_top() + vec2(14.0, 14.0),
        Align2::LEFT_TOP,
        glyph,
        theme::icon_font(22.0),
        if selected { p.accent } else { p.text2 },
    );
    painter.text(
        rect.left_top() + vec2(14.0, 48.0),
        Align2::LEFT_TOP,
        title,
        theme::font_semibold(13.5),
        p.text,
    );
    let galley = ui.fonts_mut(|f| f.layout(desc.to_string(), theme::font(11.5), p.text2, w - 28.0));
    painter.galley(rect.left_top() + vec2(14.0, 68.0), galley, p.text2);
    resp
}

fn dialog_next(slot: &mut Option<Dialog>, d: Dialog) {
    *slot = Some(d);
}

/// The largest rect with the `work` aspect ratio centered in `avail`.
fn fit(avail: egui::Rect, work: (i32, i32)) -> egui::Rect {
    let (w, h) = (work.0.max(1) as f32, work.1.max(1) as f32);
    let s = (avail.width() / w).min(avail.height() / h).max(0.01);
    egui::Rect::from_center_size(avail.center(), egui::vec2(w * s, h * s))
}

fn show_editor(ui: &mut Ui, e: &mut ZoneEditor, rect: egui::Rect) {
    match e {
        ZoneEditor::Grid(g) => g.show(ui, rect),
        ZoneEditor::Canvas(c) => c.show(ui, rect),
    }
}

fn hint(e: &ZoneEditor) -> &'static str {
    match e {
        ZoneEditor::Grid(_) => {
            "Click a zone to split it (hold Shift to split vertically). Drag the handles between zones to resize. Drag across zones to select them, then Merge."
        }
        ZoneEditor::Canvas(_) => "Drag a zone to move it, drag its edges or corners to resize, × or Delete removes it. Hold Alt to move without snapping.",
    }
}

fn toolbar(ui: &mut Ui, e: &mut ZoneEditor) {
    let p = pal(ui);
    let sep = |ui: &mut Ui| {
        let (r, _) = ui.allocate_exact_size(vec2(1.0, 22.0), Sense::hover());
        ui.painter()
            .vline(r.center().x, r.y_range(), Stroke::new(1.0, p.card_border));
    };
    ui.spacing_mut().item_spacing.x = 10.0;
    match e {
        ZoneEditor::Grid(g) => {
            ui.label(theme::icon_text(icon::GRID_FOUR).size(16.0).color(p.accent));
            ui.label(
                RichText::new(zones_label(g.model.len()))
                    .font(theme::font_semibold(13.0))
                    .color(p.text),
            );
            sep(ui);
            widgets::toggle_labeled(ui, &mut g.show_spacing, "Space around zones");
            ui.label(RichText::new("Space around zones").color(p.text));
            ui.add_enabled(
                g.show_spacing,
                egui::DragValue::new(&mut g.spacing)
                    .range(fancyzones::layout::MAX_NEGATIVE_SPACING..=1000)
                    .suffix(" px"),
            );
            sep(ui);
            ui.label(RichText::new("Highlight distance").color(p.text));
            ui.add(
                egui::DragValue::new(&mut g.sensitivity_radius)
                    .range(0..=1000)
                    .suffix(" px"),
            );
        }
        ZoneEditor::Canvas(c) => {
            ui.label(theme::icon_text(icon::STACK).size(16.0).color(p.accent));
            ui.label(
                RichText::new(zones_label(c.zones.len()))
                    .font(theme::font_semibold(13.0))
                    .color(p.text),
            );
            sep(ui);
            if widgets::icon_text_button(ui, icon::PLUS, "Add zone").clicked() {
                c.add_zone();
            }
            if ui
                .add_enabled_ui(c.selected.is_some(), |ui| {
                    widgets::icon_text_button(ui, icon::TRASH, "Delete zone")
                })
                .inner
                .clicked()
            {
                if let Some(i) = c.selected {
                    c.delete_zone(i);
                }
            }
            sep(ui);
            widgets::toggle_labeled(ui, &mut c.magnetic, "Snap to edges");
            ui.label(RichText::new("Snap to edges").color(p.text));
            sep(ui);
            ui.label(RichText::new("Highlight distance").color(p.text));
            ui.add(
                egui::DragValue::new(&mut c.sensitivity_radius)
                    .range(0..=1000)
                    .suffix(" px"),
            );
        }
    }
}

/// "Edit on screen": a borderless window covering the monitor's work area.
/// Returns (back to window, save, cancel).
fn on_screen(ctx: &egui::Context, s: &mut Session) -> (bool, bool, bool) {
    let wa = s.target.work_area;
    // Monitor rects are points on macOS and physical pixels on Windows;
    // viewport positions are points.
    let k = if cfg!(windows) && s.target.scale > 0.0 { s.target.scale as f32 } else { 1.0 };
    let builder = egui::ViewportBuilder::default()
        .with_title("FancyZones zone editor")
        .with_position([wa.left as f32 / k, wa.top as f32 / k])
        .with_inner_size([wa.width() as f32 / k, wa.height() as f32 / k])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top();
    let (mut back, mut save, mut cancel) = (false, false, false);
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("fz-editor-on-screen"),
        builder,
        |ui, _class| {
            theme::ensure(ui.ctx());
            if ui
                .ctx()
                .input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape))
            {
                back = true;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let rect = fit(ui.max_rect(), s.target.work());
                    show_editor(ui, &mut s.editor, rect);
                });
            egui::Area::new(egui::Id::unique("fz-on-screen-toolbar"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, 16.0])
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    let p = pal(ui);
                    egui::Frame::new()
                        .fill(p.card)
                        .stroke(Stroke::new(1.0, p.card_border))
                        .corner_radius(14.0)
                        .inner_margin(egui::Margin::symmetric(16, 12))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 8],
                            blur: 30,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(90),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&s.name)
                                        .font(theme::font_semibold(14.0))
                                        .color(p.text),
                                );
                                ui.add_space(8.0);
                                toolbar(ui, &mut s.editor);
                                ui.add_space(8.0);
                                back |= ui.button("Back to window").clicked();
                                cancel = ui.button("Cancel").clicked();
                                save = widgets::primary_button(ui, "Save").clicked();
                            });
                            ui.add_space(4.0);
                            ui.label(RichText::new(hint(&s.editor)).size(12.0).color(p.text2));
                        });
                });
        },
    );
    (back, save, cancel)
}

#[cfg(test)]
impl EditorApp {
    pub fn test_open_template_dialog(&mut self, i: usize) {
        self.dialog = Some(Dialog::Template {
            index: i,
            layout: self.templates[i].clone(),
        });
    }
    pub fn test_open_custom_dialog(&mut self, uuid: &str) {
        self.dialog = Some(Dialog::Custom {
            uuid: uuid.into(),
            name: "Coding".into(),
            show_spacing: true,
            spacing: 8,
            sensitivity: 20,
            hotkey: Some(1),
            grid: true,
        });
    }
    pub fn test_open_new_dialog(&mut self) {
        self.dialog = Some(Dialog::New {
            name: "Custom layout 2".into(),
            canvas: false,
        });
    }
    pub fn test_close_dialog(&mut self) {
        self.dialog = None;
    }
    pub fn test_open_editor(&mut self, uuid: &str) {
        self.open_custom_editor(uuid);
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

// ---------------------------------------------------------------------------
// Starting the editor process from the settings window

static CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// Start `<this exe> --config <config_path> editor [--monitor ID]` (or bring
/// the one already started from here to the front).
pub fn open_editor_process(config_path: &Path, monitor: Option<&str>) -> anyhow::Result<()> {
    let mut child = CHILD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = child.as_mut() {
        if let Ok(None) = c.try_wait() {
            activate_process(c.id());
            return Ok(());
        }
    }
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("--config").arg(config_path).arg("editor");
    if let Some(m) = monitor.filter(|m| !m.is_empty()) {
        cmd.arg("--monitor").arg(m);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    *child = Some(cmd.spawn()?);
    Ok(())
}

#[cfg(target_os = "macos")]
fn activate_process(pid: u32) {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) {
        app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_process(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitors() -> Vec<Monitor> {
        vec![
            Monitor {
                id: "mon-a".into(),
                name: "Built-in".into(),
                frame: fancyzones::Rect::new(0, 0, 1512, 982),
                work_area: fancyzones::Rect::new(0, 33, 1512, 982),
                scale: 2.0,
                primary: true,
            },
            Monitor {
                id: "mon-b".into(),
                name: "External".into(),
                frame: fancyzones::Rect::new(1512, 0, 1512 + 2560, 1440),
                work_area: fancyzones::Rect::new(1512, 0, 1512 + 2560, 1440),
                scale: 1.0,
                primary: false,
            },
        ]
    }

    struct Harness {
        ctx: egui::Context,
    }

    impl Harness {
        fn frame(&self, app: &mut EditorApp) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 760.0))),
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(input, |ui| app.draw(ui));
            out.textures_delta.clear();
        }
        /// A frame with pointer events.
        fn events(&self, app: &mut EditorApp, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 760.0))),
                events,
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(input, |ui| app.draw(ui));
            out.textures_delta.clear();
        }
    }

    fn press(p: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pm-editor-ui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_screen_renders() {
        let d = dir("render");
        let path = d.join("fancyzones.json");
        let mut c = fancyzones::Config::default();
        c.settings.span_zones_across_monitors = true;
        c.applied_layouts.insert("gone".into(), AppliedLayout::template(LayoutType::Rows, 2));
        c.import_fancyzones_custom_layouts(
            r#"{"custom-layouts": [
              {"uuid": "{AAAAAAAA-0000-0000-0000-000000000001}", "name": "Grid", "type": "grid",
               "info": {"rows": 2, "columns": 2, "rows-percentage": [5000, 5000], "columns-percentage": [5000, 5000],
                        "cell-child-map": [[0, 1], [0, 2]], "show-spacing": true, "spacing": 8, "sensitivity-radius": 20}},
              {"uuid": "{AAAAAAAA-0000-0000-0000-000000000002}", "name": "Canvas", "type": "canvas",
               "info": {"ref-width": 1920, "ref-height": 1080, "zones": [{"X": 0, "Y": 0, "width": 960, "height": 1080},
                        {"X": 500, "Y": 100, "width": 900, "height": 500}], "sensitivity-radius": 20}}]}"#,
        )
        .unwrap();
        c.save_to(&path).unwrap();
        let h = Harness { ctx: egui::Context::default() };
        let mut app = EditorApp::with_monitors(path.clone(), Some("mon-b".into()), monitors());
        assert_eq!(app.selected, "mon-b");
        assert_eq!(app.targets().len(), 5); // span, a, b, gone, default
        for t in app.targets() {
            app.select(&t.id);
            h.frame(&mut app);
            h.frame(&mut app);
        }
        // Dialogs.
        app.dialog = Some(Dialog::Template { index: 4, layout: app.templates[4].clone() });
        h.frame(&mut app);
        app.dialog = Some(Dialog::Custom {
            uuid: "{AAAAAAAA-0000-0000-0000-000000000001}".into(),
            name: "Grid".into(),
            show_spacing: true,
            spacing: 8,
            sensitivity: 20,
            hotkey: Some(1),
            grid: true,
        });
        h.frame(&mut app);
        app.dialog = Some(Dialog::New { name: "x".into(), canvas: true });
        h.frame(&mut app);
        app.dialog = Some(Dialog::ConfirmDelete { uuid: "{AAAAAAAA-0000-0000-0000-000000000002}".into(), name: "Canvas".into() });
        h.frame(&mut app);
        app.dialog = None;
        // Zone editors, in the window and "on screen" (embedded in tests).
        for uuid in ["{AAAAAAAA-0000-0000-0000-000000000001}", "{AAAAAAAA-0000-0000-0000-000000000002}"] {
            app.select("mon-a");
            app.open_custom_editor(uuid);
            assert!(app.session.is_some());
            for on in [false, true, false] {
                app.session.as_mut().unwrap().on_screen = on;
                for _ in 0..3 {
                    h.frame(&mut app);
                }
            }
            app.session = None;
        }
        app.open_new_layout("", false);
        h.frame(&mut app);
        app.open_new_layout("Free", true);
        h.frame(&mut app);
        // Nothing was saved by just looking.
        assert_eq!(fancyzones::Config::load_from(&path).unwrap(), c);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn apply_create_edit_save_flow() {
        let d = dir("flow");
        let path = d.join("fancyzones.json");
        let h = Harness { ctx: egui::Context::default() };
        let mut app = EditorApp::with_monitors(path.clone(), None, monitors());
        assert_eq!(app.selected, "mon-a", "main monitor by default");
        h.frame(&mut app);

        // Apply a template.
        let mut cols = app.templates[2].clone();
        cols.zone_count = 5;
        app.commit("x", |s| store::apply_layout(s, "mon-a", cols.clone()));
        assert_eq!(fancyzones::Config::load_from(&path).unwrap().applied_layouts["mon-a"].zone_count, 5);

        // New grid layout: split the first column by clicking in the editor.
        app.open_new_layout("Mine", false);
        h.frame(&mut app);
        let ZoneEditor::Grid(g) = &app.session.as_ref().unwrap().editor else { panic!() };
        assert_eq!(g.model.len(), 3);
        // Click in the first column, a third of the way down.
        let rect = g.last_rect.expect("editor drawn");
        assert!((rect.width() / rect.height() - 1512.0 / 949.0).abs() < 0.01, "monitor aspect ratio");
        let p = rect.left_top() + egui::vec2(rect.width() / 6.0, rect.height() / 3.0);
        h.events(&mut app, vec![egui::Event::PointerMoved(p)]);
        h.events(&mut app, vec![press(p, true)]);
        h.events(&mut app, vec![press(p, false)]);
        h.frame(&mut app);
        let ZoneEditor::Grid(g) = &app.session.as_ref().unwrap().editor else { panic!() };
        assert_eq!(g.model.len(), 4, "clicking split a zone");
        assert_eq!(g.model.to_info().rows, 2);
        let uuid = app.session.as_ref().unwrap().uuid.clone();
        assert!(app.save_session());
        assert!(app.session.is_none());
        let disk = store::load(&path).unwrap();
        let l = fancyzones::layout::find_custom(&disk.custom_layouts, &uuid).unwrap();
        assert_eq!(l.name, "Mine");
        assert_eq!(l.zone_count(), 4);
        assert_eq!(disk.applied_layouts["mon-a"].uuid, uuid, "a new layout is applied on save");
        assert_eq!(disk.applied_layouts["mon-a"].zone_count, 4);

        // Cancel discards.
        app.open_custom_editor(&uuid);
        if let Some(Session { editor: ZoneEditor::Grid(g), .. }) = &mut app.session {
            g.model.merge(&[0, 1, 2, 3]);
        }
        app.session = None;
        assert_eq!(store::load(&path).unwrap().custom_layouts[0].zone_count(), 4);

        // Hotkey + rename through the store path used by the dialog.
        app.commit("x", |s| {
            store::rename_custom(s, &uuid, "Renamed");
            store::set_hotkey(s, &uuid, Some(7));
        });
        let disk = store::load(&path).unwrap();
        assert_eq!(disk.custom_layouts[0].name, "Renamed");
        assert_eq!(store::hotkey_for(&disk.layout_hotkeys, &uuid), Some(7));

        // The file changes elsewhere: the editor picks it up.
        let mut c = fancyzones::Config::load_from(&path).unwrap();
        c.custom_layouts.clear();
        std::thread::sleep(Duration::from_millis(20));
        c.save_to(&path).unwrap();
        app.last_poll = Instant::now() - POLL;
        app.mtime = None;
        h.frame(&mut app);
        assert!(app.snap.custom_layouts.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    fn drag(h: &Harness, app: &mut EditorApp, from: egui::Pos2, to: egui::Pos2) {
        h.events(app, vec![egui::Event::PointerMoved(from)]);
        h.events(app, vec![press(from, true)]);
        for i in 1..=6 {
            h.events(app, vec![egui::Event::PointerMoved(from.lerp(to, i as f32 / 6.0))]);
        }
        h.events(app, vec![press(to, false)]);
        h.frame(app);
    }

    #[test]
    fn grid_drag_merge_and_resize_with_the_mouse() {
        let d = dir("grid-mouse");
        let h = Harness { ctx: egui::Context::default() };
        let mut app = EditorApp::with_monitors(d.join("fancyzones.json"), None, monitors());
        app.open_new_layout("g", false);
        h.frame(&mut app);
        let rect = |app: &EditorApp| match &app.session.as_ref().unwrap().editor {
            ZoneEditor::Grid(g) => g.last_rect.unwrap(),
            _ => unreachable!(),
        };
        let grid = |app: &mut EditorApp| match &mut app.session.as_mut().unwrap().editor {
            ZoneEditor::Grid(g) => g.clone(),
            _ => unreachable!(),
        };
        let r = rect(&app);
        // Drag the divider between columns 1 and 2 to the right by 1/6 of the width.
        let from = egui::pos2(r.left() + r.width() / 3.0, r.center().y);
        drag(&h, &mut app, from, from + egui::vec2(r.width() / 6.0, 0.0));
        let g = grid(&mut app);
        assert_eq!(g.model.len(), 3);
        let cols = g.to_info().columns_percents;
        assert!((cols[0] - 5000).abs() < 150, "{cols:?}");
        // Drag from zone 2 into zone 3 selects both; then merge.
        let a = egui::pos2(r.left() + r.width() * 0.6, r.center().y + 40.0);
        let b = egui::pos2(r.left() + r.width() * 0.9, r.center().y + 40.0);
        drag(&h, &mut app, a, b);
        let g = grid(&mut app);
        assert_eq!(g.merge_selection, vec![1, 2]);
        if let Some(Session { editor: ZoneEditor::Grid(g), .. }) = &mut app.session {
            g.merge_selected();
            assert_eq!(g.model.len(), 2);
            assert_eq!(g.to_info().cell_child_map, vec![vec![0, 1]]);
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn canvas_move_resize_delete_with_the_mouse() {
        let d = dir("canvas-mouse");
        let path = d.join("fancyzones.json");
        let h = Harness { ctx: egui::Context::default() };
        let mut app = EditorApp::with_monitors(path.clone(), Some("mon-b".into()), monitors());
        app.open_new_layout("c", true);
        h.frame(&mut app);
        let state = |app: &EditorApp| match &app.session.as_ref().unwrap().editor {
            ZoneEditor::Canvas(c) => c.clone(),
            _ => unreachable!(),
        };
        let c = state(&app);
        assert_eq!(c.work, (2560, 1440));
        assert_eq!(c.zones, vec![fancyzones::layout::CanvasZone { x: 100, y: 100, width: 1024, height: 576 }]);
        let r = c.last_rect.unwrap();
        let s = r.width() / 2560.0;
        let at = |x: f32, y: f32| r.min + egui::vec2(x * s, y * s);
        // Move the zone towards the top-left corner: it snaps into it.
        drag(&h, &mut app, at(600.0, 400.0), at(600.0 - 90.0, 400.0 - 95.0));
        let z = state(&app).zones[0];
        assert_eq!((z.x, z.y, z.width, z.height), (0, 0, 1024, 576));
        // Resize with the bottom-right corner.
        drag(&h, &mut app, at(1024.0 - 2.0, 576.0 - 2.0), at(1280.0 - 2.0, 720.0 - 2.0));
        let z = state(&app).zones[0];
        assert!((z.width - 1280).abs() <= 4 && (z.height - 720).abs() <= 4, "{z:?}");
        // Click the × to delete it.
        let c = state(&app);
        let close = at(z.width as f32, 0.0) + egui::vec2(-14.0, 14.0);
        h.events(&mut app, vec![egui::Event::PointerMoved(close)]);
        h.events(&mut app, vec![press(close, true)]);
        h.events(&mut app, vec![press(close, false)]);
        h.frame(&mut app);
        assert!(state(&app).zones.is_empty(), "{:?}", c.zones);
        // Add two zones and save: canvas JSON with this monitor's size.
        if let Some(Session { editor: ZoneEditor::Canvas(c), .. }) = &mut app.session {
            c.add_zone();
            c.add_zone();
        }
        assert!(app.save_session());
        let disk = store::load(&path).unwrap();
        let CustomLayoutInfo::Canvas(info) = &disk.custom_layouts[0].info else { panic!() };
        assert_eq!((info.ref_width, info.ref_height, info.zones.len()), (2560, 1440, 2));
        assert_eq!(disk.applied_layouts["mon-b"].kind, LayoutType::Custom);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn broken_file_is_reported_not_overwritten() {
        let d = dir("broken");
        let path = d.join("fancyzones.json");
        std::fs::write(&path, "{ nope").unwrap();
        let h = Harness { ctx: egui::Context::default() };
        let mut app = EditorApp::with_monitors(path.clone(), None, vec![]);
        assert!(app.load_error.is_some());
        assert_eq!(app.selected, DEFAULT_TARGET);
        h.frame(&mut app);
        assert!(!app.commit("x", |s| store::apply_layout(s, "m", AppliedLayout::default())));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ nope");
        std::fs::remove_dir_all(&d).ok();
    }
}
