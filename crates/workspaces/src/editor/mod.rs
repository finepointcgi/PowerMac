//! The Workspaces editor and launcher window (egui, PowerMac theme):
//! a gallery of workspace cards with layout previews, capture / import /
//! launch, and an edit view for the apps of one workspace (include or
//! remove apps, monitor and frame, state, launch options, desktop
//! shortcut).

mod preview;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use pm_ui::theme::{self, icon, pal};
use pm_ui::widgets;

use self::preview::Icons;
use crate::geometry::{self, Monitor};
use crate::launch::{AppStatus, LaunchEvent, LaunchReport};
use crate::model::{now_unix, relative_time, App, ExistingWindows, OtherWindows, WindowState, Workspace};
use crate::{store, Config};

const POLL: Duration = Duration::from_millis(800);
const CARD_MIN_W: f32 = 270.0;
const CARD_MAX_W: f32 = 380.0;
const PREVIEW_H: f32 = 148.0;
const CARD_H: f32 = PREVIEW_H + 102.0;

/// Open the editor (blocks until the window is closed). The module config
/// is read from `<data_dir>/config.json` (`"workspaces"` key) when present.
pub fn run_editor(data_dir: impl Into<PathBuf>) -> anyhow::Result<()> {
    let dir = data_dir.into();
    let cfg = config_from_app_config(&dir).unwrap_or_default();
    run_editor_with(dir, cfg)
}

/// [`run_editor`] with an explicit config.
pub fn run_editor_with(data_dir: impl Into<PathBuf>, config: Config) -> anyhow::Result<()> {
    let app = EditorApp::new(data_dir.into(), config);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Workspaces")
            .with_app_id("com.powermac.workspaces")
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([720.0, 520.0])
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "Workspaces",
        options,
        Box::new(move |_cc| {
            crate::platform::become_foreground_app();
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("workspaces editor: {e}"))
}

/// The `workspaces` section of PowerMac's `config.json`, if there is one.
pub fn config_from_app_config(data_dir: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(data_dir.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(v.get("workspaces")?.clone()).ok()
}

/// Progress of a launch started from the editor.
#[derive(Debug, Clone, Default)]
pub struct LaunchUi {
    pub name: String,
    pub rows: Vec<(String, AppStatus, Option<String>)>,
    pub done: bool,
    pub summary: Option<String>,
    pub error: Option<String>,
    /// Workspace id (to reload its "last launched").
    pub id: String,
    /// The entries (for their icons).
    pub apps: Vec<App>,
}

impl LaunchUi {
    fn apply(&mut self, e: &LaunchEvent) {
        while self.rows.len() <= e.index {
            self.rows.push((String::new(), AppStatus::Pending, None));
        }
        self.rows[e.index] = (e.app.clone(), e.status.clone(), e.detail.clone());
    }

    fn finish(&mut self, r: Result<LaunchReport, String>) {
        self.done = true;
        match r {
            Ok(rep) => {
                for e in &rep.results {
                    self.apply(e);
                }
                self.summary = Some(rep.summary());
            }
            Err(e) => self.error = Some(e),
        }
    }
}

enum JobResult {
    Captured(Result<Workspace, String>),
    Picked(Option<PathBuf>),
    Imported(Result<crate::ImportReport, String>),
}

#[derive(Default)]
struct Shared {
    result: Option<JobResult>,
    launch: Option<LaunchUi>,
}

/// The workspace being edited.
#[derive(Clone)]
struct Draft {
    ws: Workspace,
    saved: Workspace,
    is_new: bool,
    included: Vec<bool>,
    selected: Option<usize>,
}

impl Draft {
    fn new(ws: Workspace, is_new: bool) -> Self {
        let n = ws.apps.len();
        Self { saved: ws.clone(), ws, is_new, included: vec![true; n], selected: None }
    }

    fn dirty(&self) -> bool {
        self.is_new || self.ws != self.saved || self.included.iter().any(|i| !i)
    }

    /// The workspace as it would be saved.
    fn result(&self) -> Workspace {
        let mut ws = self.ws.clone();
        ws.apps = ws.apps.iter().zip(&self.included).filter(|(_, i)| **i).map(|(a, _)| a.clone()).collect();
        ws.name = ws.name.trim().to_string();
        ws.ensure_app_ids();
        ws
    }

    fn remove(&mut self, i: usize) {
        if i < self.ws.apps.len() {
            self.ws.apps.remove(i);
            self.included.remove(i);
            self.selected = match self.selected {
                Some(s) if s == i => None,
                Some(s) if s > i => Some(s - 1),
                s => s,
            };
        }
    }
}

enum Dialog {
    ConfirmDelete { id: String, name: String },
    Rename { id: String, name: String },
    Import { path: String },
    Launch,
    Discard,
}

#[derive(Clone)]
struct Status {
    text: String,
    error: bool,
    reveal: Option<PathBuf>,
}

/// Editor state; [`EditorApp::draw`] draws one frame.
pub struct EditorApp {
    data_dir: PathBuf,
    config: Config,
    list: Vec<Workspace>,
    stamp: Option<SystemTime>,
    last_poll: Instant,
    load_error: Option<String>,
    search: String,
    draft: Option<Draft>,
    hovered_app: Option<usize>,
    dialog: Option<Dialog>,
    status: Option<Status>,
    shared: Arc<Mutex<Shared>>,
    busy: Option<&'static str>,
    icons: Icons,
    monitors: Vec<Monitor>,
    permissions: bool,
    now: Option<i64>,
}

fn lock(s: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

impl EditorApp {
    pub fn new(data_dir: PathBuf, config: Config) -> Self {
        let monitors = crate::platform::monitors().unwrap_or_default();
        let mut s = Self {
            data_dir,
            config,
            list: vec![],
            stamp: None,
            last_poll: Instant::now(),
            load_error: None,
            search: String::new(),
            draft: None,
            hovered_app: None,
            dialog: None,
            status: None,
            shared: Arc::new(Mutex::new(Shared::default())),
            busy: None,
            icons: Icons::default(),
            monitors,
            permissions: crate::platform::has_permissions(),
            now: None,
        };
        s.reload();
        s
    }

    fn reload(&mut self) {
        match store::load(&self.data_dir) {
            Ok(l) => {
                self.list = l;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("{e:#}")),
        }
        self.stamp = store::stamp(&self.data_dir);
    }

    pub fn workspaces(&self) -> &[Workspace] {
        &self.list
    }

    // --- hooks for tests and screenshots ---------------------------------

    #[doc(hidden)]
    pub fn test_setup(&mut self, monitors: Vec<Monitor>, permissions: bool, now: i64, real_icons: bool) {
        self.monitors = monitors;
        self.permissions = permissions;
        self.now = Some(now);
        self.icons.disabled = !real_icons;
    }

    #[doc(hidden)]
    pub fn test_edit(&mut self, id: &str, selected: Option<usize>) {
        if let Some(ws) = self.list.iter().find(|w| w.id == id).cloned() {
            let mut d = Draft::new(ws, false);
            d.selected = selected;
            self.draft = Some(d);
        }
    }

    #[doc(hidden)]
    pub fn test_new_draft(&mut self, ws: Workspace, excluded: &[usize]) {
        let mut d = Draft::new(ws, true);
        for &i in excluded {
            if let Some(v) = d.included.get_mut(i) {
                *v = false;
            }
        }
        self.draft = Some(d);
    }

    #[doc(hidden)]
    pub fn test_launch_progress(&mut self, l: LaunchUi) {
        lock(&self.shared).launch = Some(l);
        self.dialog = Some(Dialog::Launch);
    }

    #[doc(hidden)]
    pub fn test_confirm_delete(&mut self, id: &str) {
        if let Some(w) = self.list.iter().find(|w| w.id == id) {
            self.dialog = Some(Dialog::ConfirmDelete { id: w.id.clone(), name: w.name.clone() });
        }
    }

    #[doc(hidden)]
    pub fn test_import_dialog(&mut self, path: &str) {
        self.dialog = Some(Dialog::Import { path: path.into() });
    }

    #[doc(hidden)]
    pub fn test_status(&mut self, text: &str, error: bool) {
        self.status = Some(Status { text: text.into(), error, reveal: None });
    }

    #[doc(hidden)]
    pub fn test_search(&mut self, s: &str) {
        self.search = s.into();
    }

    // --- actions ----------------------------------------------------------

    fn now(&self) -> i64 {
        self.now.unwrap_or_else(now_unix)
    }

    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status { text: text.into(), error, reveal: None });
    }

    fn save_list(&mut self) -> bool {
        match store::save(&self.data_dir, &self.list) {
            Ok(()) => {
                self.stamp = store::stamp(&self.data_dir);
                true
            }
            Err(e) => {
                self.set_status(format!("Couldn't save workspaces.json: {e:#}"), true);
                false
            }
        }
    }

    fn start_capture(&mut self, ctx: &egui::Context) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Capturing the desktop…");
        let name = store::unique_name(&self.list, "Workspace");
        let cfg = self.config.clone();
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = crate::capture(&name, &cfg).map_err(|e| format!("{e:#}"));
            lock(&shared).result = Some(JobResult::Captured(r));
            ctx.request_repaint();
        });
    }

    fn start_pick(&mut self, ctx: &egui::Context) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Choose the PowerToys workspaces.json…");
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let p = crate::platform::pick_file("Choose PowerToys' workspaces.json");
            lock(&shared).result = Some(JobResult::Picked(p));
            ctx.request_repaint();
        });
    }

    fn start_import(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.busy = Some("Importing…");
        let dir = self.data_dir.clone();
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = crate::import_powertoys_file(&dir, &path).map_err(|e| format!("{e:#}"));
            lock(&shared).result = Some(JobResult::Imported(r));
            ctx.request_repaint();
        });
    }

    /// Launch `ws`; `saved` = record the launch time (it's the stored one).
    fn start_launch(&mut self, ctx: &egui::Context, ws: Workspace, saved: bool) {
        if lock(&self.shared).launch.as_ref().is_some_and(|l| !l.done) {
            self.dialog = Some(Dialog::Launch);
            return;
        }
        let ui_state = LaunchUi {
            name: ws.name.clone(),
            rows: ws.apps.iter().map(|a| (a.name.clone(), AppStatus::Pending, None)).collect(),
            id: ws.id.clone(),
            apps: ws.apps.clone(),
            ..Default::default()
        };
        lock(&self.shared).launch = Some(ui_state);
        self.dialog = Some(Dialog::Launch);
        let cfg = self.config.clone();
        let dir = self.data_dir.clone();
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let progress = |e: &LaunchEvent| {
                if let Some(l) = lock(&shared).launch.as_mut() {
                    l.apply(e);
                }
                ctx.request_repaint();
            };
            let r = if saved { crate::launch(&dir, &ws.id, &cfg, &progress) } else { crate::launch_workspace(&ws, &cfg, &progress) };
            if let Some(l) = lock(&shared).launch.as_mut() {
                l.finish(r.map_err(|e| format!("{e:#}")));
            }
            ctx.request_repaint();
        });
    }

    fn create_shortcut(&mut self, ws: &Workspace) {
        match crate::create_shortcut(ws, &self.config) {
            Ok(p) => {
                self.status = Some(Status { text: format!("Desktop shortcut created: {}", p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()), error: false, reveal: Some(p) });
                if let Some(w) = self.list.iter_mut().find(|w| w.id == ws.id) {
                    w.is_shortcut_needed = true;
                }
                self.save_list();
            }
            Err(e) => self.set_status(format!("Couldn't create the shortcut: {e:#}"), true),
        }
    }

    fn delete(&mut self, id: &str) {
        let name = self.list.iter().find(|w| w.id == id).map(|w| w.name.clone()).unwrap_or_default();
        self.list.retain(|w| w.id != id);
        if self.save_list() {
            self.set_status(format!("Deleted “{name}”"), false);
        }
        if self.draft.as_ref().is_some_and(|d| d.ws.id == id) {
            self.draft = None;
        }
    }

    fn save_draft(&mut self) -> bool {
        let Some(d) = &self.draft else { return false };
        let mut ws = d.result();
        if ws.name.is_empty() {
            ws.name = store::unique_name(&self.list, "Workspace");
        }
        if ws.apps.is_empty() {
            self.set_status("A workspace needs at least one app.", true);
            return false;
        }
        let name = ws.name.clone();
        match self.list.iter_mut().find(|w| w.id == ws.id) {
            Some(w) => *w = ws,
            None => self.list.insert(0, ws),
        }
        if self.save_list() {
            self.set_status(format!("Saved “{name}”"), false);
            self.draft = None;
            true
        } else {
            false
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed() >= POLL {
            self.last_poll = Instant::now();
            let s = store::stamp(&self.data_dir);
            if s != self.stamp {
                self.reload();
            }
        }
        let res = lock(&self.shared).result.take();
        if let Some(r) = res {
            self.busy = None;
            match r {
                JobResult::Captured(Ok(ws)) => {
                    let n = ws.apps.len();
                    self.draft = Some(Draft::new(ws, true));
                    self.set_status(format!("Captured {n} window{}. Turn off what you don't need, then save.", if n == 1 { "" } else { "s" }), false);
                }
                JobResult::Captured(Err(e)) => self.set_status(format!("Capture failed: {e}"), true),
                JobResult::Picked(Some(p)) => self.dialog = Some(Dialog::Import { path: p.to_string_lossy().into_owned() }),
                JobResult::Picked(None) => {}
                JobResult::Imported(Ok(rep)) => {
                    self.reload();
                    let err = rep.workspaces.is_empty();
                    self.set_status(rep.summary(), err);
                }
                JobResult::Imported(Err(e)) => self.set_status(format!("Import failed: {e}"), true),
            }
        }
        let finished = {
            let mut s = lock(&self.shared);
            match s.launch.as_mut() {
                Some(l) if l.done && !l.id.is_empty() => {
                    let id = std::mem::take(&mut l.id);
                    Some((id, l.summary.clone().or(l.error.clone()).unwrap_or_default(), l.error.is_some()))
                }
                _ => None,
            }
        };
        if let Some((_, text, err)) = finished {
            self.reload();
            if !matches!(self.dialog, Some(Dialog::Launch)) {
                self.set_status(text, err);
            }
        }
        if self.busy.is_some() || lock(&self.shared).launch.as_ref().is_some_and(|l| !l.done) {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(POLL);
        }
    }

    // --- drawing ----------------------------------------------------------

    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        let p = pal(ui);
        self.poll(&ctx);
        egui::Panel::bottom("ws-status")
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin::symmetric(24, 8)).stroke(Stroke::new(1.0, p.card_border)))
            .show(ui, |ui| self.status_bar(ui));
        if self.draft.is_some() {
            egui::Panel::top("ws-edit-bar")
                .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 24, right: 24, top: 14, bottom: 12 }).stroke(Stroke::new(1.0, p.separator)))
                .show(ui, |ui| self.edit_bar(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(if self.draft.is_some() { "edit" } else { "list" }).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 32, right: 32, top: 24, bottom: 32 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.draft.is_some() {
                        self.edit_view(ui);
                    } else {
                        self.list_view(ui);
                    }
                });
            });
        });
        self.dialogs(&ctx);
        // Esc leaves the edit view when nothing changed.
        if self.dialog.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(d) = &self.draft {
                if d.dirty() {
                    self.dialog = Some(Dialog::Discard);
                } else {
                    self.draft = None;
                }
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(b) = self.busy {
                ui.add(egui::Spinner::new().size(13.0).color(p.text2));
                ui.label(RichText::new(b).size(12.5).color(p.text2));
            } else if let Some(e) = self.load_error.clone() {
                ui.label(theme::icon_text(icon::WARNING_CIRCLE).size(14.0).color(p.error));
                ui.add(egui::Label::new(RichText::new(format!("workspaces.json couldn't be read, so nothing is saved: {e}")).size(12.5).color(p.error)).truncate());
            } else if let Some(s) = self.status.clone() {
                let (g, c) = if s.error { (icon::WARNING_CIRCLE, p.error) } else { (icon::CHECK_CIRCLE, p.ok) };
                ui.label(theme::icon_text(g).size(14.0).color(c));
                ui.add(egui::Label::new(RichText::new(&s.text).size(12.5).color(if s.error { p.error } else { p.text2 })).truncate());
                if let Some(path) = &s.reveal {
                    if ui.link(RichText::new(if cfg!(windows) { "Show in Explorer" } else { "Show in Finder" }).size(12.5)).clicked() {
                        crate::platform::reveal(path);
                    }
                }
            } else {
                ui.label(theme::icon_text(icon::INFO).size(14.0).color(p.text3));
                let hint = if self.draft.is_some() {
                    "Click a window in the preview or an app in the list to edit it. Changes are saved when you click Save."
                } else {
                    "Capture the windows you have open now, then launch the workspace any time to get them back."
                };
                ui.add(egui::Label::new(RichText::new(hint).size(12.5).color(p.text2)).truncate());
            }
        });
    }

    fn header(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let capturing = self.busy.is_some();
                if ui.add_enabled_ui(!capturing, |ui| widgets::primary_icon_button(ui, icon::PLUS, "Capture desktop")).inner.on_hover_text("Save the windows that are open now as a new workspace").clicked() {
                    self.start_capture(&ctx);
                }
                if ui.add_enabled_ui(!capturing, |ui| widgets::icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Import…")).inner.on_hover_text("Import PowerToys' workspaces.json").clicked() {
                    match crate::import::default_powertoys_path().filter(|p| p.exists()) {
                        Some(p) => self.dialog = Some(Dialog::Import { path: p.to_string_lossy().into_owned() }),
                        None => self.dialog = Some(Dialog::Import { path: String::new() }),
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    logo(ui, 40.0);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new("Workspaces").font(theme::font_semibold(23.0)).color(p.text));
                        ui.add(egui::Label::new(RichText::new("Save the apps and windows you have open, and bring them back in one click.").color(p.text2)).wrap());
                    });
                });
            });
        });
    }

    fn list_view(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        self.header(ui);
        if !self.permissions {
            ui.add_space(16.0);
            banner(
                ui,
                icon::LOCK_KEY,
                p.warn,
                "Accessibility permission needed",
                "PowerMac reads and moves windows through Accessibility. Allow it in System Settings → Privacy & Security → Accessibility.",
            );
        }
        if self.list.is_empty() {
            self.empty_state(ui);
            return;
        }
        ui.add_space(22.0);
        // Section title + search.
        let mut order: Vec<usize> = (0..self.list.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.list[i].last_launched_time.unwrap_or(self.list[i].creation_time)));
        let q = self.search.trim().to_lowercase();
        if !q.is_empty() {
            order.retain(|&i| {
                let w = &self.list[i];
                w.name.to_lowercase().contains(&q) || w.apps.iter().any(|a| a.name.to_lowercase().contains(&q))
            });
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Your workspaces  ·  {}", self.list.len())).font(theme::font_semibold(15.0)).color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                search_field(ui, &mut self.search, 220.0);
            });
        });
        ui.add_space(12.0);
        if order.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(format!("No workspace matches “{}”.", self.search.trim())).color(p.text2));
            });
            return;
        }
        let mut action: Option<CardAction> = None;
        let now = self.now();
        let gap = 16.0;
        let w = ui.available_width();
        let cols = (((w + gap) / (CARD_MIN_W + gap)).floor() as usize).clamp(1, 5);
        let card_w = ((w - gap * (cols as f32 - 1.0)) / cols as f32).min(CARD_MAX_W);
        let mut k = 0;
        while k < order.len() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for _ in 0..cols {
                    if k >= order.len() {
                        break;
                    }
                    let i = order[k];
                    let ws = self.list[i].clone();
                    if let Some(a) = workspace_card(ui, card_w, &ws, &mut self.icons, now) {
                        action = Some(a);
                    }
                    k += 1;
                }
            });
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }
        match action {
            Some(CardAction::Launch(id)) => {
                if let Some(ws) = self.list.iter().find(|w| w.id == id).cloned() {
                    self.start_launch(&ctx, ws, true);
                }
            }
            Some(CardAction::Edit(id)) => {
                if let Some(ws) = self.list.iter().find(|w| w.id == id).cloned() {
                    self.draft = Some(Draft::new(ws, false));
                    self.status = None;
                }
            }
            Some(CardAction::Rename(id)) => {
                if let Some(ws) = self.list.iter().find(|w| w.id == id) {
                    self.dialog = Some(Dialog::Rename { id, name: ws.name.clone() });
                }
            }
            Some(CardAction::Shortcut(id)) => {
                if let Some(ws) = self.list.iter().find(|w| w.id == id).cloned() {
                    self.create_shortcut(&ws);
                }
            }
            Some(CardAction::Delete(id)) => {
                if let Some(ws) = self.list.iter().find(|w| w.id == id) {
                    self.dialog = Some(Dialog::ConfirmDelete { id, name: ws.name.clone() });
                }
            }
            None => {}
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        ui.add_space(40.0);
        let w = ui.available_width().min(560.0);
        ui.vertical_centered(|ui| {
            // An illustration: a sample layout.
            let (rect, _) = ui.allocate_exact_size(vec2(w.min(360.0), 170.0), Sense::hover());
            let sample = sample_workspace();
            let painter = ui.painter().clone();
            painter.rect(rect, 14.0, p.card, Stroke::new(1.0, p.card_border), StrokeKind::Inside);
            let mut icons = Icons::disabled();
            preview::paint(ui, &painter, rect.shrink(12.0), &sample, &mut icons, None, None, None);
            ui.add_space(22.0);
            ui.label(RichText::new("No workspaces yet").font(theme::font_semibold(18.0)).color(p.text));
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new("Arrange the apps you use for a task — coding, writing, a meeting — then capture them. Launching the workspace later opens what's missing and puts every window back on the right display.").color(p.text2)).wrap());
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let bw = 170.0 + 112.0 + 8.0;
                ui.add_space(((ui.available_width() - bw) / 2.0).max(0.0));
                if widgets::primary_icon_button(ui, icon::PLUS, "Capture desktop").clicked() {
                    self.start_capture(&ctx);
                }
                if widgets::icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Import from PowerToys…").clicked() {
                    self.dialog = Some(Dialog::Import { path: String::new() });
                }
            });
        });
    }

    fn edit_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let Some(d) = self.draft.as_mut() else { return };
        let dirty = d.dirty();
        let mut back = false;
        let mut save = false;
        let mut launch = false;
        ui.horizontal(|ui| {
            if widgets::icon_text_button(ui, icon::CARET_LEFT, "Workspaces").clicked() {
                back = true;
            }
            ui.add_space(6.0);
            ui.label(RichText::new(if d.is_new { "New workspace" } else { "Edit workspace" }).font(theme::font_semibold(15.0)).color(p.text));
            if dirty && !d.is_new {
                ui.label(RichText::new("·  unsaved changes").size(12.5).color(p.text2));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let can_save = dirty && d.included.iter().any(|i| *i);
                if ui.add_enabled_ui(can_save, |ui| widgets::primary_icon_button(ui, icon::CHECK, if d.is_new { "Save workspace" } else { "Save" })).inner.clicked() {
                    save = true;
                }
                if widgets::icon_text_button(ui, icon::PLAY, "Launch").on_hover_text("Restore these windows now (unsaved changes included)").clicked() {
                    launch = true;
                }
            });
        });
        if back {
            if dirty {
                self.dialog = Some(Dialog::Discard);
            } else {
                self.draft = None;
            }
        }
        if save {
            self.save_draft();
        }
        if launch {
            let (ws, saved) = {
                let d = self.draft.as_ref().unwrap();
                (d.result(), !d.dirty())
            };
            if !ws.apps.is_empty() {
                self.start_launch(&ctx, ws, saved);
            }
        }
    }

    fn edit_view(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let wide = ui.available_width() >= 860.0;
        let Some(mut d) = self.draft.take() else { return };
        let mut shortcut = false;

        // Name.
        ui.horizontal(|ui| {
            logo(ui, 34.0);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    egui::TextEdit::singleline(&mut d.ws.name)
                        .font(theme::font_semibold(20.0))
                        .hint_text("Workspace name")
                        .desired_width(ui.available_width().min(420.0)),
                );
                let n_on = d.included.iter().filter(|i| **i).count();
                let mons = d.ws.monitors.len().max(1);
                let mut meta = format!("{n_on} of {} window{} · {mons} display{}", d.ws.apps.len(), if d.ws.apps.len() == 1 { "" } else { "s" }, if mons == 1 { "" } else { "s" });
                if !d.is_new {
                    meta.push_str(&format!(" · created {}", relative_time(d.ws.creation_time, self.now.unwrap_or_else(now_unix))));
                }
                ui.label(RichText::new(meta).size(12.5).color(p.text2));
            });
        });
        ui.add_space(2.0);

        let left_w = if wide { (ui.available_width() * 0.56).floor() } else { ui.available_width() };
        let gap = 20.0;
        let right_w = if wide { ui.available_width() - left_w - gap } else { ui.available_width() };

        let mut left = |ui: &mut Ui, d: &mut Draft, icons: &mut Icons, hovered: &mut Option<usize>, cfg: &Config| {
            ui.set_width(left_w);
            // Preview.
            widgets::card_with(ui, "Layout", Some("Click a window to edit it. Dimmed windows aren't saved."), |ui| {
                ui.add_space(8.0);
                let w = ui.available_width();
                let h = (w * preview::aspect(&d.ws) + 12.0).clamp(190.0, 360.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
                let painter = ui.painter().clone();
                let hits = preview::paint(ui, &painter, rect, &d.ws, icons, Some(&d.included), d.selected, *hovered);
                *hovered = resp.hover_pos().and_then(|pos| preview::hit(&hits, pos));
                if hovered.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    d.selected = *hovered;
                }
                ui.add_space(8.0);
            });
            // Options.
            widgets::card_with(ui, "When launching", Some("How this workspace treats the windows already on screen."), |ui| {
                widgets::toggle_row(
                    ui,
                    "Move existing windows",
                    Some("Reuse an app's open window and move it, instead of opening a new one."),
                    &mut d.ws.move_existing_windows,
                );
                widgets::row(ui, "Other apps' windows", Some("Apps are never quit, so nothing unsaved is lost."), |ui| {
                    widgets::segmented(ui, &mut d.ws.other_windows, &[(OtherWindows::Leave, "Leave"), (OtherWindows::Minimize, "Minimize"), (OtherWindows::Hide, "Hide")])
                });
                let shortcut_desc = if cfg!(windows) {
                    "A .lnk on the desktop that launches this workspace."
                } else if cfg.shortcut_kind == crate::ShortcutKind::Command {
                    "A .command file on the desktop that launches this workspace."
                } else {
                    "A small app on the desktop that launches this workspace."
                };
                widgets::row(ui, "Desktop shortcut", Some(shortcut_desc), |ui| {
                    if ui.add_enabled_ui(!d.is_new, |ui| widgets::icon_text_button(ui, icon::LINK, "Create")).inner.on_disabled_hover_text("Save the workspace first").clicked() {
                        shortcut = true;
                    }
                });
            });
        };

        let right = |ui: &mut Ui, d: &mut Draft, icons: &mut Icons, hovered: &mut Option<usize>, monitors: &[Monitor]| {
            ui.set_width(right_w);
            apps_card(ui, d, icons, hovered);
            if let Some(i) = d.selected.filter(|i| *i < d.ws.apps.len()) {
                app_details(ui, d, i, icons, monitors);
            }
        };

        let mut hovered = self.hovered_app;
        if wide {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| left(ui, &mut d, &mut self.icons, &mut hovered, &self.config));
                ui.vertical(|ui| right(ui, &mut d, &mut self.icons, &mut hovered, &self.monitors));
            });
        } else {
            left(ui, &mut d, &mut self.icons, &mut hovered, &self.config);
            right(ui, &mut d, &mut self.icons, &mut hovered, &self.monitors);
        }
        self.hovered_app = hovered;
        let ws_for_shortcut = shortcut.then(|| d.result());
        self.draft = Some(d);
        if let Some(ws) = ws_for_shortcut {
            self.create_shortcut(&ws);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let p = theme::pal_ctx(ctx);
        let frame = egui::Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.card_border))
            .corner_radius(14.0)
            .inner_margin(egui::Margin::same(22))
            .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(if p.dark { 140 } else { 60 }) });
        let kind = match &dialog {
            Dialog::ConfirmDelete { .. } => 0,
            Dialog::Rename { .. } => 1,
            Dialog::Import { .. } => 2,
            Dialog::Launch => 3,
            Dialog::Discard => 4,
        };
        let mut keep = true;
        let mut after: Option<AfterDialog> = None;
        let launch_state = lock(&self.shared).launch.clone();
        let modal = egui::Modal::new(egui::Id::unique(("ws-dialog", kind))).frame(frame).show(ctx, |ui| {
            ui.set_width(if kind == 3 { 460.0 } else { 400.0 });
            widgets::reset_rows(ui);
            match &mut dialog {
                Dialog::ConfirmDelete { id, name } => {
                    dialog_title(ui, icon::TRASH, &format!("Delete “{name}”?"));
                    widgets::hint(ui, "The workspace is removed from the list. Its apps and windows aren't touched, and a desktop shortcut for it stops working.");
                    ui.add_space(18.0);
                    buttons(ui, |ui| {
                        if danger_button(ui, "Delete").clicked() {
                            let id = id.clone();
                            after = Some(Box::new(move |s: &mut Self, _: &egui::Context| s.delete(&id)));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Rename { id, name } => {
                    dialog_title(ui, icon::PENCIL_SIMPLE, "Rename workspace");
                    ui.add_space(6.0);
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY).hint_text("Name"));
                    r.request_focus();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(18.0);
                    buttons(ui, |ui| {
                        if ui.add_enabled_ui(!name.trim().is_empty(), |ui| widgets::primary_button(ui, "Rename")).inner.clicked() || (enter && !name.trim().is_empty()) {
                            let (id, name) = (id.clone(), name.trim().to_string());
                            after = Some(Box::new(move |s: &mut Self, _: &egui::Context| {
                                if let Some(w) = s.list.iter_mut().find(|w| w.id == id) {
                                    w.name = name.clone();
                                }
                                if s.save_list() {
                                    s.set_status(format!("Renamed to “{name}”"), false);
                                }
                            }));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Import { path } => {
                    dialog_title(ui, icon::ARROW_SQUARE_OUT, "Import from PowerToys");
                    widgets::hint(
                        ui,
                        if cfg!(windows) {
                            "PowerToys keeps its workspaces in %LOCALAPPDATA%\\Microsoft\\PowerToys\\Workspaces\\workspaces.json."
                        } else {
                            "Copy workspaces.json from %LOCALAPPDATA%\\Microsoft\\PowerToys\\Workspaces on your PC. Windows apps are matched to Mac apps by name (Notepad → TextEdit, …); apps that aren't installed here are listed afterwards."
                        },
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(path).desired_width(ui.available_width() - 92.0).hint_text("Path to workspaces.json"));
                        if ui.button("Choose…").clicked() {
                            after = Some(Box::new(|s: &mut Self, c: &egui::Context| s.start_pick(c)));
                            keep = false;
                        }
                    });
                    let exists = Path::new(path.trim()).is_file();
                    if !path.trim().is_empty() && !exists {
                        ui.add_space(4.0);
                        widgets::error_label(ui, "No file at this path.");
                    }
                    ui.add_space(18.0);
                    buttons(ui, |ui| {
                        if ui.add_enabled_ui(exists, |ui| widgets::primary_button(ui, "Import")).inner.clicked() {
                            let pth = PathBuf::from(path.trim());
                            after = Some(Box::new(move |s: &mut Self, c: &egui::Context| s.start_import(c, pth)));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Launch => {
                    let Some(l) = launch_state.as_ref() else {
                        keep = false;
                        return;
                    };
                    dialog_title(ui, icon::ROCKET_LAUNCH, &format!("Launching “{}”", l.name));
                    let placed = l.rows.iter().filter(|r| r.1 == AppStatus::Placed).count();
                    let sub = if l.done {
                        l.error.clone().or(l.summary.clone()).unwrap_or_default()
                    } else {
                        format!("{placed} of {} windows in place…", l.rows.len())
                    };
                    widgets::hint(ui, &sub);
                    ui.add_space(10.0);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for (k, (name, st, detail)) in l.rows.iter().enumerate() {
                            let tex = l.apps.get(k).map(|a| (a, self.icons.get(ui.ctx(), a)));
                            launch_row(ui, name, st, detail.as_deref(), tex);
                        }
                    });
                    ui.add_space(14.0);
                    buttons(ui, |ui| {
                        if l.done {
                            if widgets::primary_button(ui, "Done").clicked() {
                                keep = false;
                            }
                        } else if ui.button("Hide").on_hover_text("Keep launching in the background").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Discard => {
                    dialog_title(ui, icon::WARNING, "Discard changes?");
                    widgets::hint(ui, "Your changes to this workspace haven't been saved.");
                    ui.add_space(18.0);
                    buttons(ui, |ui| {
                        if widgets::primary_button(ui, "Save").clicked() {
                            after = Some(Box::new(|s: &mut Self, _: &egui::Context| {
                                s.save_draft();
                            }));
                            keep = false;
                        }
                        if danger_button(ui, "Discard").clicked() {
                            after = Some(Box::new(|s: &mut Self, _: &egui::Context| s.draft = None));
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
        if keep {
            self.dialog = Some(dialog);
        }
        if let Some(f) = after {
            f(self, ctx);
        }
    }
}

/// Work a dialog button leaves for after the dialog is drawn.
type AfterDialog = Box<dyn FnOnce(&mut EditorApp, &egui::Context)>;

enum CardAction {
    Launch(String),
    Edit(String),
    Rename(String),
    Shortcut(String),
    Delete(String),
}

/// PowerMac-style tile for the window header.
fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    let cr = (size * 0.24).round();
    let top = Color32::from_rgb(0x5E, 0x8B, 0xFF);
    let bottom = Color32::from_rgb(0x5B, 0x3F, 0xE0);
    painter.rect_filled(rect, cr, theme::mix(top, bottom, 0.5));
    painter.rect_filled(Rect::from_min_size(rect.min, vec2(size, size * 0.5)), egui::CornerRadius { nw: cr as u8, ne: cr as u8, sw: 0, se: 0 }, theme::mix(top, bottom, 0.25));
    // Three windows.
    let inner = rect.shrink(size * 0.2);
    let w = Color32::WHITE;
    let r = size * 0.05;
    let a = Rect::from_min_size(inner.min, vec2(inner.width() * 0.62, inner.height() * 0.58));
    let b = Rect::from_min_size(pos2(inner.left() + inner.width() * 0.38, inner.top() + inner.height() * 0.3), vec2(inner.width() * 0.62, inner.height() * 0.7));
    painter.rect_filled(a, r, w.gamma_multiply(0.62));
    painter.rect_filled(b.expand(size * 0.025), r, theme::mix(top, bottom, 0.6));
    painter.rect_filled(b, r, w);
    painter.rect_filled(Rect::from_min_size(b.min, vec2(b.width(), b.height() * 0.2)), egui::CornerRadius { nw: r as u8, ne: r as u8, sw: 0, se: 0 }, theme::mix(w, bottom, 0.25));
}

fn banner(ui: &mut Ui, glyph: &str, color: Color32, title: &str, text: &str) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(theme::mix(p.card, color, if p.dark { 0.12 } else { 0.08 }))
        .stroke(Stroke::new(1.0, theme::mix(p.card_border, color, 0.35)))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon_text(glyph).size(18.0).color(color));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(p.text));
                    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text2)).wrap());
                });
            });
        });
}

fn search_field(ui: &mut Ui, text: &mut String, w: f32) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0, p.control_border))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.set_width(w);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(theme::icon_text(icon::MAGNIFYING_GLASS).size(13.0).color(p.text3));
                ui.add(
                    egui::TextEdit::singleline(text)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(0, 2))
                        .align(Align2::LEFT_CENTER)
                        .hint_text("Search workspaces or apps")
                        .desired_width(ui.available_width() - 20.0),
                );
                if !text.is_empty() && ui.add(egui::Button::new(theme::icon_text(icon::X).size(11.0).color(p.text3)).frame(false)).clicked() {
                    text.clear();
                }
            });
        });
}

fn sample_workspace() -> Workspace {
    use crate::geometry::Rect as R;
    let m = Monitor { id: "s".into(), name: String::new(), number: 0, frame: R::new(0, 0, 1600, 1000), work_area: R::new(0, 30, 1600, 970), scale: 2.0, primary: true };
    let app = |name: &str, f: R| App { name: name.into(), bundle_id: name.into(), frame: f, monitor_id: "s".into(), ..Default::default() };
    Workspace {
        monitors: vec![m],
        apps: vec![app("Notes", R::new(900, 420, 620, 480)), app("Browser", R::new(60, 90, 900, 760)), app("Mail", R::new(820, 80, 720, 420))],
        ..Default::default()
    }
}

/// One workspace in the gallery.
fn workspace_card(ui: &mut Ui, w: f32, ws: &Workspace, icons: &mut Icons, now: i64) -> Option<CardAction> {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CARD_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let mut action = None;
    let painter = ui.painter().clone();
    let hovered = resp.hovered() || ui.rect_contains_pointer(rect);
    let (fill, stroke) = if hovered { (theme::mix(p.card, p.control_hover, 0.5), Stroke::new(1.0, theme::mix(p.card_border, p.accent, 0.35))) } else { (p.card, Stroke::new(1.0, p.card_border)) };
    painter.rect_filled(rect.translate(vec2(0.0, 1.0)), 12.0, Color32::from_black_alpha(if p.dark { 40 } else { 8 }));
    painter.rect(rect, 12.0, fill, stroke, StrokeKind::Inside);
    // Preview well.
    let well = Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(w - 24.0, PREVIEW_H - 12.0));
    painter.rect(well, 8.0, if p.dark { Color32::from_rgb(0x20, 0x20, 0x23) } else { Color32::from_rgb(0xF0, 0xF0, 0xF4) }, Stroke::new(1.0, p.card_border), StrokeKind::Inside);
    preview::paint(ui, &painter, well.shrink(4.0), ws, icons, None, None, None);
    // Text.
    let tx = rect.left() + 16.0;
    let mut y = well.bottom() + 12.0;
    let name_g = ui.fonts_mut(|f| f.layout(ws.name.clone(), theme::font_semibold(14.5), p.text, w - 32.0));
    let name_g = if name_g.rows.len() > 1 {
        ui.fonts_mut(|f| {
            let mut job = egui::text::LayoutJob::simple_singleline(ws.name.clone(), theme::font_semibold(14.5), p.text);
            job.wrap = egui::text::TextWrapping::truncate_at_width(w - 32.0);
            f.layout_job(job)
        })
    } else {
        name_g
    };
    painter.galley(pos2(tx, y), name_g, p.text);
    y += 21.0;
    let apps = ws.distinct_apps();
    let mons = ws.monitors.iter().filter(|m| ws.apps.iter().any(|a| a.monitor_id == m.id)).count().max(1);
    let when = match ws.last_launched_time {
        Some(t) => format!("Launched {}", relative_time(t, now)),
        None => "Never launched".into(),
    };
    let meta = format!("{apps} app{} · {mons} display{} · {when}", if apps == 1 { "" } else { "s" }, if mons == 1 { "" } else { "s" });
    painter.text(pos2(tx, y), Align2::LEFT_TOP, meta, theme::font(12.0), p.text2);
    // Footer: app icons on the left, buttons on the right.
    let br = Rect::from_min_max(pos2(rect.left() + 12.0, rect.bottom() - 44.0), pos2(rect.right() - 12.0, rect.bottom() - 10.0));
    let mut bui = ui.new_child(UiBuilder::new().max_rect(br).layout(Layout::right_to_left(Align::Center)));
    bui.spacing_mut().item_spacing.x = 6.0;
    if widgets::primary_icon_button(&mut bui, icon::PLAY, "Launch").clicked() {
        action = Some(CardAction::Launch(ws.id.clone()));
    }
    if widgets::icon_button(&mut bui, icon::PENCIL_SIMPLE, "Edit").clicked() {
        action = Some(CardAction::Edit(ws.id.clone()));
    }
    let more = more_button(&mut bui);
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(200.0);
        card_menu(ui, ws, &mut action, false);
    });
    let buttons_left = bui.min_rect().left();
    let _ = y;
    let mut seen: Vec<String> = Vec::new();
    let distinct: Vec<&App> = ws
        .apps
        .iter()
        .filter(|a| {
            let k = crate::matching::app_key(a);
            if seen.contains(&k) {
                false
            } else {
                seen.push(k);
                true
            }
        })
        .collect();
    let isz = 22.0;
    let room = buttons_left - tx - 8.0;
    let mut max_icons = ((room + 4.0) / (isz + 4.0)).floor().max(0.0) as usize;
    if distinct.len() > max_icons && max_icons > 0 {
        max_icons -= 1; // room for "+N"
    }
    let mut x = tx;
    let cy = br.center().y;
    for a in distinct.iter().take(max_icons) {
        let r = Rect::from_min_size(pos2(x, cy - isz / 2.0), vec2(isz, isz));
        let tex = icons.get(ui.ctx(), a);
        preview::paint_app_icon(&painter, r, a, tex.as_ref(), 1.0);
        x += isz + 4.0;
    }
    if distinct.len() > max_icons {
        painter.text(pos2(x + 2.0, cy), Align2::LEFT_CENTER, format!("+{}", distinct.len() - max_icons), theme::font_semibold(12.0), p.text2);
    }
    if action.is_none() && resp.clicked() {
        action = Some(CardAction::Edit(ws.id.clone()));
    }
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        card_menu(ui, ws, &mut action, true);
    });
    action
}

fn card_menu(ui: &mut Ui, ws: &Workspace, action: &mut Option<CardAction>, full: bool) {
    let p = pal(ui);
    if full {
        if ui.button(icon_label(icon::PLAY, "Launch")).clicked() {
            *action = Some(CardAction::Launch(ws.id.clone()));
        }
        if ui.button(icon_label(icon::PENCIL_SIMPLE, "Edit")).clicked() {
            *action = Some(CardAction::Edit(ws.id.clone()));
        }
    }
    if ui.button(icon_label(icon::TEXT_T, "Rename…")).clicked() {
        *action = Some(CardAction::Rename(ws.id.clone()));
    }
    if ui.button(icon_label(icon::LINK, "Create desktop shortcut")).clicked() {
        *action = Some(CardAction::Shortcut(ws.id.clone()));
    }
    ui.separator();
    if ui.button(icon_label_colored(icon::TRASH, "Delete…", p.error)).clicked() {
        *action = Some(CardAction::Delete(ws.id.clone()));
    }
}

/// A square button with three dots (opens a menu).
fn more_button(ui: &mut Ui) -> egui::Response {
    let p = pal(ui);
    let r = ui.add(egui::Button::new("").min_size(vec2(28.0, 28.0))).on_hover_text("More");
    let c = r.rect.center();
    for dx in [-5.0, 0.0, 5.0] {
        ui.painter().circle_filled(c + vec2(dx, 0.0), 1.6, p.text);
    }
    r
}

fn icon_label(glyph: &str, text: &str) -> egui::text::LayoutJob {
    let c = Color32::PLACEHOLDER;
    theme::icon_job(glyph, text, 13.0, c)
}

fn icon_label_colored(glyph: &str, text: &str, c: Color32) -> egui::text::LayoutJob {
    theme::icon_job(glyph, text, 13.0, c)
}

fn state_chip(ui: &mut Ui, st: WindowState) {
    let p = pal(ui);
    let (g, c) = match st {
        WindowState::Normal => return,
        WindowState::Zoomed => (icon::ARROWS_LEFT_RIGHT, p.accent),
        WindowState::Minimized => (icon::ARROW_DOWN, p.text2),
        WindowState::FullScreen => (icon::MONITOR, p.accent),
    };
    let text = st.label();
    let font = theme::font(11.0);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), c).size().x);
    let (r, _) = ui.allocate_exact_size(vec2(tw + 30.0, 20.0), Sense::hover());
    ui.painter().rect_filled(r, 10.0, theme::mix(p.card, c, if p.dark { 0.2 } else { 0.12 }));
    ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::CENTER_CENTER, g, theme::icon_font(11.0), c);
    ui.painter().text(pos2(r.left() + 19.0, r.center().y), Align2::LEFT_CENTER, text, font, c);
}

/// The list of apps (windows) of the draft.
fn apps_card(ui: &mut Ui, d: &mut Draft, icons: &mut Icons, hovered: &mut Option<usize>) {
    let p = pal(ui);
    let title = if d.is_new { "Windows captured" } else { "Apps" };
    let sub = if d.is_new { "Turn off the windows you don't want in this workspace." } else { "In launch order; the first one ends up in front." };
    widgets::card_with(ui, title, Some(sub), |ui| {
        let n = d.ws.apps.len();
        if n == 0 {
            ui.add_space(10.0);
            widgets::hint(ui, "No apps left. Capture the desktop again to add some.");
            ui.add_space(10.0);
            return;
        }
        let mut select = None;
        let mut new_hover = None;
        for i in 0..n {
            if i > 0 {
                widgets::divider(ui);
            }
            let row_h = 48.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let sel = d.selected == Some(i);
            let hov = resp.hovered() || *hovered == Some(i);
            if resp.hovered() {
                new_hover = Some(i);
            }
            if sel {
                ui.painter().rect_filled(rect.expand2(vec2(10.0, -1.0)), 8.0, p.accent_soft);
            } else if hov {
                ui.painter().rect_filled(rect.expand2(vec2(10.0, -1.0)), 8.0, theme::mix(p.card, p.control_hover, 0.6));
            }
            let a = d.ws.apps[i].clone();
            let on = d.included[i];
            let alpha = if on { 1.0 } else { 0.4 };
            let ir = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(30.0, 30.0));
            let tex = icons.get(ui.ctx(), &a);
            preview::paint_app_icon(ui.painter(), ir, &a, tex.as_ref(), alpha);
            let tx = rect.left() + 42.0;
            let text_w = rect.width() - 42.0 - 150.0;
            let name = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(a.name.clone(), theme::font_semibold(13.0), if on { p.text } else { p.text3 });
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
                f.layout_job(job)
            });
            ui.painter().galley(pos2(tx, rect.center().y - 16.0), name, p.text);
            let detail = a.document_name().or_else(|| (!a.title.is_empty()).then(|| a.title.clone())).unwrap_or_else(|| a.describe_frame());
            let det = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(detail, theme::font(11.5), if on { p.text2 } else { p.text3 });
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
                f.layout_job(job)
            });
            ui.painter().galley(pos2(tx, rect.center().y + 2.0), det, p.text2);
            // State chip + switch on the right.
            let cr = Rect::from_min_max(pos2(rect.right() - 150.0, rect.top()), rect.max);
            let mut cui = ui.new_child(UiBuilder::new().max_rect(cr).layout(Layout::right_to_left(Align::Center)));
            cui.spacing_mut().item_spacing.x = 8.0;
            let mut v = d.included[i];
            if widgets::toggle_labeled(&mut cui, &mut v, &format!("Include {}", a.name)).changed() {
                d.included[i] = v;
            }
            state_chip(&mut cui, a.state);
            if resp.clicked() {
                select = Some(i);
            }
        }
        if let Some(i) = select {
            d.selected = if d.selected == Some(i) { None } else { Some(i) };
        }
        if new_hover.is_some() || !ui.ui_contains_pointer() {
            *hovered = new_hover.or(*hovered).filter(|_| new_hover.is_some());
        }
    });
}

fn monitor_label(m: &Monitor, connected: bool) -> String {
    let base = if m.name.is_empty() { m.id.clone() } else { m.name.clone() };
    let res = format!("{}×{}", m.frame.width, m.frame.height);
    if connected {
        format!("{base} ({res})")
    } else {
        format!("{base} ({res}, not connected)")
    }
}

/// Settings of the selected app.
fn app_details(ui: &mut Ui, d: &mut Draft, i: usize, icons: &mut Icons, live: &[Monitor]) {
    let p = pal(ui);
    let ctx = ui.ctx().clone();
    let a_snapshot = d.ws.apps[i].clone();
    let mut remove = false;
    let title = a_snapshot.name.clone();
    widgets::card_with(ui, &title, Some(&a_snapshot.describe_frame()), |ui| {
        // header block with icon + path
        widgets::block(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                let tex = icons.get(&ctx, &a_snapshot);
                preview::paint_app_icon(ui.painter(), r, &a_snapshot, tex.as_ref(), 1.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let id = if a_snapshot.bundle_id.is_empty() { a_snapshot.path.clone() } else { a_snapshot.bundle_id.clone() };
                    ui.add(egui::Label::new(RichText::new(id).size(12.0).color(p.text2)).truncate());
                    if !a_snapshot.title.is_empty() {
                        ui.add(egui::Label::new(RichText::new(format!("“{}”", a_snapshot.title)).size(12.0).color(p.text3)).truncate());
                    }
                    if !crate::platform::app_exists(&a_snapshot.path, &a_snapshot.bundle_id) && !a_snapshot.path.is_empty() {
                        widgets::warn_label(ui, "This app isn't installed at the saved location.");
                    }
                });
            });
        });
        let a = &mut d.ws.apps[i];
        // Display.
        let mut choices: Vec<(Monitor, bool)> = d.ws.monitors.iter().map(|m| (m.clone(), live.iter().any(|l| l.id == m.id))).collect();
        for l in live {
            if !choices.iter().any(|(m, _)| m.id == l.id) {
                choices.push((l.clone(), true));
            }
        }
        let current = choices.iter().find(|(m, _)| m.id == a.monitor_id).map(|(m, c)| monitor_label(m, *c)).unwrap_or_else(|| "Unknown display".into());
        let mut pick: Option<Monitor> = None;
        widgets::row(ui, "Display", None, |ui| {
            let r = egui::ComboBox::from_id_salt(("ws-monitor", i)).selected_text(current).width(230.0).show_ui(ui, |ui| {
                for (m, c) in &choices {
                    if ui.selectable_label(m.id == a.monitor_id, monitor_label(m, *c)).clicked() {
                        pick = Some(m.clone());
                    }
                }
            });
            named(&r.response, "Display");
        });
        if let Some(m) = pick {
            if m.id != a.monitor_id {
                let from = d.ws.monitors.iter().find(|x| x.id == a.monitor_id).map(|x| x.work_area);
                if let Some(from) = from {
                    a.frame = geometry::scale_between(&a.frame, &from, &m.work_area).fit_into(&m.work_area);
                } else {
                    a.frame = a.frame.fit_into(&m.work_area);
                }
                a.monitor_id = m.id.clone();
                if !d.ws.monitors.iter().any(|x| x.id == m.id) {
                    d.ws.monitors.push(m);
                }
            }
        }
        let a = &mut d.ws.apps[i];
        widgets::row(ui, "Position", Some("Top-left corner, in points from the main display's corner."), |ui| {
            ui.add(egui::DragValue::new(&mut a.frame.x).prefix("x  ").speed(1.0));
            ui.add(egui::DragValue::new(&mut a.frame.y).prefix("y  ").speed(1.0));
        });
        widgets::row(ui, "Size", None, |ui| {
            ui.add(egui::DragValue::new(&mut a.frame.width).prefix("w  ").range(60..=20000).speed(1.0));
            ui.add(egui::DragValue::new(&mut a.frame.height).prefix("h  ").range(40..=20000).speed(1.0));
        });
        let states: Vec<(WindowState, &str)> = WindowState::ALL.iter().map(|s| (*s, s.label())).collect();
        widgets::row(ui, "Window state", None, |ui| named(&widgets::combo(ui, &format!("ws-state-{i}"), &mut a.state, &states), "Window state"));
        widgets::row(ui, "Existing windows", Some("When the app already has a window open."), |ui| {
            let r = widgets::combo(ui, &format!("ws-existing-{i}"), &mut a.options.existing_windows, &[
                (ExistingWindows::WorkspaceDefault, "Workspace setting"),
                (ExistingWindows::Move, "Move it here"),
                (ExistingWindows::Leave, "Leave it, open a new one"),
            ]);
            named(&r, "Existing windows");
        });
        widgets::toggle_row(
            ui,
            "Launch a new instance",
            Some(if cfg!(windows) { "Always start another copy of the app." } else { "Start another copy (open -n). Most Mac apps run once; use it for the ones that don't." }),
            &mut a.options.launch_new_instance,
        );
        widgets::row(ui, "Delay before launching", None, |ui| {
            let mut secs = a.options.delay_ms as f32 / 1000.0;
            let r = ui.add(egui::DragValue::new(&mut secs).range(0.0..=60.0).speed(0.1).fixed_decimals(1).suffix(" s"));
            if r.changed() {
                a.options.delay_ms = (secs * 1000.0).round() as u32;
            }
            r
        });
        widgets::row(ui, "Document", Some("Opened with the app on launch."), |ui| {
            let mut doc = a.document.clone().unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut doc).hint_text("None").desired_width(230.0));
            if r.changed() {
                a.document = (!doc.trim().is_empty()).then(|| doc.trim().to_string());
            }
        });
        if a.url.is_some() || cfg!(not(windows)) {
            let mut url = a.url.clone().unwrap_or_default();
            widgets::row(ui, "URL", Some("Opened in the browser on launch."), |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut url).hint_text("None").desired_width(230.0));
                if r.changed() {
                    a.url = (!url.trim().is_empty()).then(|| url.trim().to_string());
                }
            });
        }
        widgets::row(
            ui,
            "Arguments",
            Some(if cfg!(windows) { "Command line arguments." } else { "Passed with open --args; only reach an app that isn't running yet. Not captured on macOS." }),
            |ui| ui.add(egui::TextEdit::singleline(&mut a.command_line_args).hint_text("None").desired_width(230.0)),
        );
        if cfg!(windows) {
            widgets::toggle_row(ui, "Run as administrator", None, &mut a.launch_as_admin);
        }
        widgets::block(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(theme::icon_job(icon::TRASH, "Remove from workspace", 13.0, p.error)).min_size(vec2(28.0, 28.0))).clicked() {
                    remove = true;
                }
            });
        });
    });
    if remove {
        d.remove(i);
    }
}

/// Give a control an accessible name (combo boxes have none of their own).
fn named(r: &egui::Response, name: &str) {
    let name = name.to_string();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name.clone()));
}

fn launch_row(ui: &mut Ui, name: &str, st: &AppStatus, detail: Option<&str>, icon_tex: Option<(&App, Option<egui::TextureHandle>)>) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(34.0);
        if let Some((a, tex)) = icon_tex {
            let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            preview::paint_app_icon(ui.painter(), r, a, tex.as_ref(), 1.0);
        }
        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        match st {
            AppStatus::Launching | AppStatus::Waiting => {
                egui::Spinner::new().size(15.0).color(p.accent).paint_at(ui, r);
            }
            _ => {
                let (g, c) = match st {
                    AppStatus::Placed => (icon::CHECK_CIRCLE, p.ok),
                    AppStatus::Failed => (icon::WARNING_CIRCLE, p.error),
                    AppStatus::Skipped => (icon::ARROW_SQUARE_OUT, p.text3),
                    _ => (icon::CLOCK, p.text3),
                };
                ui.painter().text(r.center(), Align2::CENTER_CENTER, g, theme::icon_font(16.0), c);
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).font(theme::font_semibold(13.0)).color(p.text));
                ui.label(RichText::new(st.label()).size(12.0).color(match st {
                    AppStatus::Failed => p.error,
                    AppStatus::Placed => p.ok,
                    _ => p.text2,
                }));
            });
            if let Some(d) = detail {
                ui.add(egui::Label::new(RichText::new(d).size(11.5).color(p.text2)).wrap());
            }
        });
    });
}

fn dialog_title(ui: &mut Ui, glyph: &str, title: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.label(theme::icon_text(glyph).size(18.0).color(p.accent));
        ui.label(RichText::new(title).font(theme::font_semibold(16.0)).color(p.text));
    });
    ui.add_space(4.0);
}

fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        add(ui);
    });
}

fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    let p = pal(ui);
    ui.add(egui::Button::new(RichText::new(text).font(theme::font_semibold(13.0)).color(Color32::WHITE)).fill(p.error).stroke(Stroke::NONE).min_size(vec2(28.0, 28.0)))
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    use super::*;
    use crate::geometry::Rect as R;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-editor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ws(name: &str) -> Workspace {
        let m = Monitor { id: "m".into(), name: "Display".into(), number: 0, frame: R::new(0, 0, 1440, 900), work_area: R::new(0, 25, 1440, 875), scale: 2.0, primary: true };
        let app = |n: &str, x: i32| App { id: format!("{n}{x}"), name: n.into(), bundle_id: format!("com.test.{n}"), frame: R::new(x, 40, 600, 500), monitor_id: "m".into(), ..Default::default() };
        Workspace { id: store::new_id(), name: name.into(), creation_time: 1, monitors: vec![m], apps: vec![app("Alpha", 0), app("Beta", 700)], ..Default::default() }
    }

    fn harness(app: EditorApp) -> Harness<'static, EditorApp> {
        Harness::builder().with_size([1000.0, 700.0]).build_ui_state(|ui, app: &mut EditorApp| app.draw(ui), app)
    }

    fn editor(d: &Path) -> EditorApp {
        let mut e = EditorApp::new(d.to_path_buf(), Config::default());
        e.test_setup(vec![], true, 1000, false);
        e
    }

    #[test]
    fn saving_a_capture_drops_excluded_windows() {
        let d = dir("save");
        let mut e = editor(&d);
        e.test_new_draft(ws("Captured"), &[1]);
        let mut h = harness(e);
        h.run();
        h.get_by_label_contains("Save workspace").click();
        h.run();
        let saved = store::load(&d).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "Captured");
        assert_eq!(saved[0].apps.len(), 1);
        assert_eq!(saved[0].apps[0].name, "Alpha");
        assert!(h.state().draft.is_none(), "back to the list");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_asks_first() {
        let d = dir("delete");
        let w = ws("Doomed");
        store::save(&d, std::slice::from_ref(&w)).unwrap();
        let mut e = editor(&d);
        e.test_confirm_delete(&w.id);
        let mut h = harness(e);
        h.run();
        assert_eq!(store::load(&d).unwrap().len(), 1, "nothing deleted before confirming");
        h.get_by_label("Delete").click();
        h.run();
        assert!(store::load(&d).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn editing_and_discarding_keeps_the_file() {
        let d = dir("discard");
        let w = ws("Keep");
        store::save(&d, std::slice::from_ref(&w)).unwrap();
        let before = std::fs::read_to_string(store::path_in(&d)).unwrap();
        let mut e = editor(&d);
        e.test_edit(&w.id, Some(0));
        e.draft.as_mut().unwrap().ws.apps[0].frame.x = 50;
        let mut h = harness(e);
        h.run();
        h.get_by_label_contains("Workspaces").click();
        h.run();
        h.get_by_label("Discard").click();
        h.run();
        assert!(h.state().draft.is_none());
        assert_eq!(std::fs::read_to_string(store::path_in(&d)).unwrap(), before);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn app_config_section_is_read() {
        let d = dir("cfg");
        std::fs::write(d.join("config.json"), r#"{"mwb": {}, "workspaces": {"launch": {"timeoutSecs": 42}}}"#).unwrap();
        assert_eq!(config_from_app_config(&d).unwrap().launch.timeout_secs, 42);
        std::fs::write(d.join("config.json"), r#"{"mwb": {}}"#).unwrap();
        assert!(config_from_app_config(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
