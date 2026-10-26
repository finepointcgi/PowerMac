//! The settings window: sidebar of modules (with search), one page per
//! module, and debounced auto-save of `config.json` / `fancyzones.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Id, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder,
};
use fancyzones::{AppliedLayout, CustomLayout, LayoutType, WindowSystem};
use pm_config::AppConfig;

use crate::model::*;
use crate::record::{self, Recorded};
use crate::theme::{self, icon, pal};
use crate::widgets::*;
use crate::zone_style;

/// Wait this long after the last change before writing the file.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the files are checked for changes made elsewhere (tray, editor).
const POLL: Duration = Duration::from_secs(1);
const DEFAULT_TARGET: &str = "";
/// Width of the sidebar and the maximum width of a page.
const SIDEBAR_W: f32 = 250.0;
const PAGE_MAX_W: f32 = 760.0;

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
    detail: String,
    work_area: fancyzones::Rect,
}

/// Icon, tint and one-line description of a page.
struct Meta {
    icon: &'static str,
    tint: Color32,
    blurb: &'static str,
}

fn meta(p: Page) -> Meta {
    let (icon, tint, blurb) = match p {
        Page::General => (
            icon::GEAR_SIX,
            Color32::from_rgb(0x8E, 0x8E, 0x93),
            "Startup, permissions, files and an overview of every module.",
        ),
        Page::Mwb => (
            icon::MOUSE,
            Color32::from_rgb(0x0A, 0x7A, 0xF5),
            "Use one mouse and keyboard across up to four computers.",
        ),
        Page::FancyZones => (
            icon::SQUARES_FOUR,
            Color32::from_rgb(0x5E, 0x5C, 0xE6),
            "Snap windows into zones. Hold Shift while dragging a window to show them.",
        ),
        Page::Awake => (
            icon::COFFEE,
            Color32::from_rgb(0xF5, 0x8A, 0x07),
            "Keep the computer awake without changing its power settings.",
        ),
        Page::ScrollReverser => (
            icon::ARROWS_DOWN_UP,
            Color32::from_rgb(0x1F, 0xA5, 0xD6),
            "Reverse the scroll direction of the mouse wheel or the trackpad.",
        ),
        Page::MouseUtils => (
            icon::CURSOR_CLICK,
            Color32::from_rgb(0xF2, 0x3B, 0x5F),
            "Find your pointer, highlight clicks and draw crosshairs.",
        ),
        Page::KeyboardManager => (
            icon::KEYBOARD,
            Color32::from_rgb(0x2F, 0xB3, 0x4F),
            "Remap keys and shortcuts, and paste as plain text.",
        ),
    };
    Meta { icon, tint, blurb }
}

/// A search hit: page, section and the matching settings in it.
#[derive(Debug, Clone, PartialEq)]
struct Hit {
    page: Page,
    section: String,
    labels: Vec<String>,
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
    /// When the "Saved" toast appeared (and its text).
    toast: Option<(Instant, String)>,

    // UI state
    search: String,
    recording: Option<Id>,
    show_key: bool,
    confirm_new_key: bool,
    copied_at: Option<Instant>,
    monitors: Vec<fancyzones::Monitor>,
    fz_selected: String,
    /// Text of each FancyZones hotkey field and the value it was made from.
    fz_hk_text: std::collections::BTreeMap<&'static str, (String, fancyzones::Hotkey)>,
    fz_new_app: String,
    running_apps: Option<Vec<(String, String)>>,
    accent: Option<(u8, u8, u8)>,
    login: bool,
    login_error: Option<String>,
    perms: Option<(bool, bool)>,
    perms_at: Instant,
    /// Mouse Without Borders status written by the running app.
    mwb_status: Option<pm_config::MwbStatus>,
    /// When "Search now" was clicked (shows "Searching…" until the app answers).
    search_clicked: Option<Instant>,
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
            toast: None,
            search: String::new(),
            recording: None,
            show_key: false,
            confirm_new_key: false,
            copied_at: None,
            monitors: Vec::new(),
            fz_selected: DEFAULT_TARGET.into(),
            fz_hk_text: Default::default(),
            fz_new_app: String::new(),
            running_apps: None,
            accent: fancyzones::platform::system_accent_color(),
            login: pm_config::login::is_enabled(),
            login_error: None,
            perms: crate::platform::permissions(),
            perms_at: now,
            mwb_status: pm_config::MwbStatus::load_for(&config_path),
            search_clicked: None,
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

    fn saved_now(&mut self) {
        self.last_saved = Some(Local::now());
        self.toast = Some((Instant::now(), "Saved".into()));
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
                        self.saved_now();
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
                self.saved_now();
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
            self.mwb_status = pm_config::MwbStatus::load_for(&self.config_path);
            if now - self.perms_at >= Duration::from_secs(2) {
                self.perms_at = now;
                self.perms = crate::platform::permissions();
                self.accent = fancyzones::platform::system_accent_color();
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

    fn module_on(&self, p: Page) -> Option<bool> {
        let c = &self.app.cfg;
        let mu = &c.mouse_utils;
        match p {
            Page::General => None,
            Page::Mwb => Some(c.mwb.enabled),
            Page::FancyZones => Some(c.fancyzones.enabled),
            Page::Awake => Some(c.awake.enabled),
            Page::ScrollReverser => Some(c.scroll_reverser.enabled),
            Page::MouseUtils => {
                Some(mu.find_my_mouse.enabled || mu.highlighter.enabled || mu.crosshairs.enabled)
            }
            Page::KeyboardManager => Some(c.keyboard_manager.enabled),
        }
    }

    fn set_module(&mut self, page: Page, on: bool) {
        let c = &mut self.app.cfg;
        match page {
            Page::Mwb => c.mwb.enabled = on,
            Page::FancyZones => c.fancyzones.enabled = on,
            Page::Awake => c.awake.enabled = on,
            Page::ScrollReverser => c.scroll_reverser.enabled = on,
            Page::KeyboardManager => c.keyboard_manager.enabled = on,
            Page::MouseUtils => {
                let mu = &mut c.mouse_utils;
                mu.find_my_mouse.enabled = on;
                mu.highlighter.enabled = on;
                mu.crosshairs.enabled = on;
            }
            Page::General => {}
        }
    }

    fn go(&mut self, p: Page) {
        self.page = p;
        self.recording = None;
    }

    // -----------------------------------------------------------------------
    // Chrome: sidebar, banners, toast

    fn sidebar(&mut self, ui: &mut Ui, hits: &[Hit]) {
        let p = pal(ui);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            app_logo(ui, 34.0);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("PowerMac")
                        .font(theme::font_semibold(15.5))
                        .color(p.text),
                );
                ui.label(RichText::new("Settings").size(12.0).color(p.text2));
            });
        });
        ui.add_space(16.0);
        search_field(ui, &mut self.search);
        ui.add_space(14.0);
        let searching = !self.search.trim().is_empty();
        let mut go = None;
        for page in Page::ALL {
            let bad = self.problems().any(|x| x.page == page);
            let dim = searching && !hits.iter().any(|h| h.page == page);
            let selected = self.page == page && !searching;
            if nav_row(ui, page, selected, dim, bad, self.module_on(page)).clicked() {
                go = Some(page);
            }
            ui.add_space(2.0);
        }
        if let Some(pg) = go {
            self.search.clear();
            self.go(pg);
        }
        // Save status at the bottom.
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(16.0);
            self.save_status(ui);
        });
    }

    fn save_status(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let (glyph, color, text) =
            if self.app_file.load_error.is_some() || self.fz_file.load_error.is_some() {
                (
                    icon::WARNING_CIRCLE,
                    p.error,
                    "A file couldn't be read".to_string(),
                )
            } else if self.problems().next().is_some() {
                let n = self.problems().count();
                (
                    icon::WARNING_CIRCLE,
                    p.error,
                    format!("Not saved — {}", plural(n, "problem")),
                )
            } else if self.app_file.save_error.is_some() || self.fz_file.save_error.is_some() {
                (icon::WARNING_CIRCLE, p.error, "Saving failed".to_string())
            } else if self.app_file.changed_at.is_some() || self.fz_file.changed_at.is_some() {
                (icon::ARROWS_CLOCKWISE, p.text2, "Saving…".to_string())
            } else if let Some(t) = self.last_saved {
                (
                    icon::CHECK_CIRCLE,
                    p.ok,
                    format!("All changes saved · {}", t.format("%H:%M")),
                )
            } else {
                (
                    icon::CHECK_CIRCLE,
                    p.text3,
                    "Changes save automatically".to_string(),
                )
            };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(theme::icon_text(glyph).size(14.0).color(color));
            ui.label(RichText::new(text).size(12.0).color(p.text2))
                .on_hover_text("Settings are saved as you change them; a running PowerMac applies them within about a second.");
        });
    }

    /// Load / save errors and validation problems, above the page.
    fn banners(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let mut reload = (false, false);
        let mut go = None;
        let mut lines: Vec<(String, Option<Page>)> = Vec::new();
        if let Some(e) = &self.app_file.load_error {
            lines.push((
                format!("config.json couldn't be read, so nothing is saved: {e}"),
                None,
            ));
        }
        if let Some(e) = &self.fz_file.load_error {
            lines.push((
                format!(
                    "fancyzones.json couldn't be read, so FancyZones changes aren't saved: {e}"
                ),
                None,
            ));
        }
        if let Some(e) = self
            .app_file
            .save_error
            .as_ref()
            .or(self.fz_file.save_error.as_ref())
        {
            lines.push((format!("Saving failed: {e}"), None));
        }
        for pr in self.problems() {
            lines.push((
                format!("{}: {}", pr.page.title(), pr.message),
                Some(pr.page),
            ));
        }
        if lines.is_empty() {
            return;
        }
        ui.add_space(16.0);
        egui::Frame::new()
            .fill(theme::mix(
                p.card,
                p.error,
                if p.dark { 0.14 } else { 0.06 },
            ))
            .stroke(Stroke::new(1.0, theme::mix(p.card_border, p.error, 0.35)))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        theme::icon_text(icon::WARNING_CIRCLE)
                            .size(18.0)
                            .color(p.error),
                    );
                    let n = self.problems().count();
                    let title = if n > 0 {
                        format!("Changes not saved — fix {}", plural(n, "problem"))
                    } else {
                        "Something went wrong".into()
                    };
                    ui.label(
                        RichText::new(title)
                            .font(theme::font_semibold(13.5))
                            .color(p.text),
                    );
                });
                for (text, page) in lines.iter().take(4) {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(26.0);
                        match page {
                            Some(pg) if *pg != self.page => {
                                if ui.link(RichText::new(text).size(12.5)).clicked() {
                                    go = Some(*pg);
                                }
                            }
                            _ => {
                                ui.label(RichText::new(text).size(12.5).color(p.text2));
                            }
                        }
                    });
                }
                if self.app_file.load_error.is_some() || self.fz_file.load_error.is_some() {
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        if ui.button("Reload files").clicked() {
                            reload = (
                                self.app_file.load_error.is_some(),
                                self.fz_file.load_error.is_some(),
                            );
                        }
                    });
                }
            });
        if reload.0 {
            self.reload_app();
        }
        if reload.1 {
            self.reload_fz();
        }
        if let Some(pg) = go {
            self.go(pg);
        }
    }

    fn toast(&mut self, ctx: &egui::Context, content: Rect) {
        let Some((t, text)) = self.toast.clone() else {
            return;
        };
        let age = t.elapsed().as_secs_f32();
        const SHOW: f32 = 1.8;
        if age > SHOW + 0.3 {
            self.toast = None;
            return;
        }
        let alpha = if age < 0.15 {
            age / 0.15
        } else if age > SHOW {
            1.0 - (age - SHOW) / 0.3
        } else {
            1.0
        };
        ctx.request_repaint();
        let p = theme::pal_ctx(ctx);
        let galley_w = ctx.fonts_mut(|f| {
            f.layout_no_wrap(text.clone(), theme::font_semibold(13.0), Color32::WHITE)
                .size()
                .x
        });
        let w = galley_w + 32.0 + 22.0;
        egui::Area::new(Id::unique("pm-toast"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos2(
                content.center().x - w / 2.0,
                content.bottom() - 60.0 + (1.0 - alpha) * 8.0,
            ))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha.clamp(0.0, 1.0));
                egui::Frame::new()
                    .fill(if p.dark {
                        Color32::from_rgb(0x3A, 0x3A, 0x3E)
                    } else {
                        Color32::from_rgb(0x2C, 0x2C, 0x2E)
                    })
                    .corner_radius(18.0)
                    .inner_margin(egui::Margin::symmetric(16, 8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_black_alpha(60),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                theme::icon_text(icon::CHECK_CIRCLE)
                                    .size(15.0)
                                    .color(Color32::from_rgb(0x32, 0xD1, 0x5D)),
                            );
                            ui.label(
                                RichText::new(text)
                                    .font(theme::font_semibold(13.0))
                                    .color(Color32::WHITE),
                            );
                        });
                    });
            });
    }

    // -----------------------------------------------------------------------
    // Search

    /// Draw every page invisibly, collecting its rows, and match `query`.
    fn search_hits(&mut self, ui: &mut Ui, query: &str) -> Vec<Hit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let ctx = ui.ctx().clone();
        let saved = self.page;
        let mut hits: Vec<Hit> = Vec::new();
        for p in Page::ALL {
            begin_collect(&ctx);
            let mut child = ui.new_child(
                UiBuilder::new()
                    .id_salt(("search-index", p))
                    .invisible()
                    .max_rect(Rect::from_min_size(
                        pos2(0.0, 0.0),
                        vec2(PAGE_MAX_W, 20_000.0),
                    )),
            );
            self.page = p;
            self.page_body(&mut child);
            let entries = end_collect(&ctx);
            let page_hit = p.title().to_lowercase().contains(&q);
            for (section, label, desc) in entries {
                let m = label.to_lowercase().contains(&q)
                    || desc.to_lowercase().contains(&q)
                    || section.to_lowercase().contains(&q)
                    || page_hit;
                if !m {
                    continue;
                }
                let label = label.trim().to_string();
                match hits
                    .iter_mut()
                    .find(|h| h.page == p && h.section == section)
                {
                    Some(h) => {
                        if !h.labels.contains(&label) {
                            h.labels.push(label)
                        }
                    }
                    None => hits.push(Hit {
                        page: p,
                        section,
                        labels: vec![label],
                    }),
                }
            }
        }
        self.page = saved;
        hits
    }

    fn search_results(&mut self, ui: &mut Ui, hits: &[Hit]) {
        let p = pal(ui);
        ui.add_space(30.0);
        ui.label(
            RichText::new(format!("Results for “{}”", self.search.trim()))
                .font(theme::font_semibold(22.0))
                .color(p.text),
        );
        ui.add_space(2.0);
        ui.label(
            RichText::new(plural(
                hits.iter().map(|h| h.labels.len()).sum(),
                "matching setting",
            ))
            .color(p.text2),
        );
        if hits.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    theme::icon_text(icon::MAGNIFYING_GLASS)
                        .size(36.0)
                        .color(p.text3),
                );
                ui.label(RichText::new("No settings match your search.").color(p.text2));
            });
            return;
        }
        let mut go: Option<(Page, String)> = None;
        for page in Page::ALL {
            let mine: Vec<&Hit> = hits.iter().filter(|h| h.page == page).collect();
            if mine.is_empty() {
                continue;
            }
            let m = meta(page);
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                icon_tile(ui, m.icon, m.tint, 22.0);
                ui.label(
                    RichText::new(page.title())
                        .font(theme::font_semibold(15.0))
                        .color(p.text),
                );
            });
            ui.add_space(8.0);
            card_frame(&p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                reset_rows(ui);
                for h in mine {
                    let shown: Vec<&str> = h.labels.iter().take(4).map(String::as_str).collect();
                    let more = h.labels.len().saturating_sub(4);
                    let mut desc = shown.join(" · ");
                    if more > 0 {
                        desc.push_str(&format!(" · and {more} more"));
                    }
                    let title = if h.section.is_empty() {
                        page.title().to_string()
                    } else {
                        h.section.clone()
                    };
                    let (clicked, label) = icon_row_plain(ui, &title, &desc, |ui| {
                        ui.add(
                            egui::Button::new(
                                theme::icon_text(icon::CARET_RIGHT)
                                    .size(14.0)
                                    .color(p.text2),
                            )
                            .frame(false),
                        )
                        .clicked()
                    });
                    if clicked || label.clicked() {
                        go = Some((page, h.section.clone()));
                    }
                }
            });
        }
        if let Some((pg, section)) = go {
            let term = self.search.trim().to_lowercase();
            set_highlight(ui.ctx(), Some(term));
            set_anchor(ui.ctx(), Some(section));
            self.search.clear();
            self.go(pg);
        }
    }

    // -----------------------------------------------------------------------
    // Page frame

    fn hero(&mut self, ui: &mut Ui, actions: impl FnOnce(&mut Self, &mut Ui)) {
        let page = self.page;
        let m = meta(page);
        let p = pal(ui);
        ui.add_space(30.0);
        let master = page != Page::General && page != Page::MouseUtils;
        let mut on = self.module_on(page).unwrap_or(false);
        card_frame(&p)
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icon_tile(ui, m.icon, m.tint, 56.0);
                    ui.add_space(10.0);
                    let right_w = if master { 110.0 } else { 0.0 };
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width() - right_w, 56.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(page.title())
                                    .font(theme::font_semibold(23.0))
                                    .color(p.text),
                            );
                            ui.add(
                                egui::Label::new(RichText::new(m.blurb).size(13.0).color(p.text2))
                                    .wrap(),
                            );
                        },
                    );
                    if master {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            toggle_labeled(ui, &mut on, &format!("Enable {}", page.title()));
                            ui.label(RichText::new(if on { "On" } else { "Off" }).color(p.text2));
                        });
                    }
                });
                actions(self, ui);
            });
        if master && Some(on) != self.module_on(page) {
            self.set_module(page, on);
        }
    }

    fn page(&mut self, ui: &mut Ui) {
        self.banners(ui);
        self.page_body(ui);
        ui.add_space(48.0);
    }

    fn page_body(&mut self, ui: &mut Ui) {
        match self.page {
            Page::General => self.general(ui),
            Page::Mwb => self.mwb(ui),
            Page::FancyZones => self.fancyzones(ui),
            Page::Awake => self.awake(ui),
            Page::ScrollReverser => self.scroll(ui),
            Page::MouseUtils => self.mouse_utils(ui),
            Page::KeyboardManager => self.keyboard(ui),
        }
    }

    // -----------------------------------------------------------------------
    // General

    fn general(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mut go = None;
        let mut set = None;
        card_with(
            ui,
            "Modules",
            Some("Turn modules on or off. Click a module's name to open its settings."),
            |ui| {
                for page in Page::ALL.into_iter().skip(1) {
                    let m = meta(page);
                    let mut on = self.module_on(page).unwrap_or(false);
                    let (_, label) =
                        icon_row(ui, m.icon, m.tint, page.title(), Some(m.blurb), |ui| {
                            toggle_labeled(ui, &mut on, page.title())
                        });
                    if label
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        go = Some(page);
                    }
                    if Some(on) != self.module_on(page) {
                        set = Some((page, on));
                    }
                }
            },
        );
        if let Some((pg, on)) = set {
            self.set_module(pg, on);
        }
        if let Some(pg) = go {
            self.go(pg);
        }
        card(ui, "Startup", |ui| {
            let mut on = self.login;
            let desc = if cfg!(target_os = "macos") {
                "Adds a LaunchAgent (~/Library/LaunchAgents/com.powermac.app.plist) that starts this PowerMac."
            } else {
                "Adds PowerMac to HKCU\\…\\CurrentVersion\\Run."
            };
            if toggle_row(ui, "Launch PowerMac at login", Some(desc), &mut on).changed() {
                match pm_config::login::set_enabled(on) {
                    Ok(()) => self.login_error = None,
                    Err(e) => self.login_error = Some(format!("{e:#}")),
                }
                self.login = pm_config::login::is_enabled();
            }
            if let Some(e) = &self.login_error {
                error_label(ui, e);
                ui.add_space(8.0);
            }
        });
        if let Some((ax, input)) = self.perms {
            card_with(
                ui,
                "Permissions",
                Some("Grant these to PowerMac.app in System Settings → Privacy & Security, then restart PowerMac."),
                |ui| {
                    for (name, desc, ok, pane) in [
                        ("Accessibility", "Needed to move and resize windows and to send keys.", ax, "Privacy_Accessibility"),
                        ("Input Monitoring", "Needed to see the mouse and keyboard for hotkeys and gestures.", input, "Privacy_ListenEvent"),
                    ] {
                        row(ui, name, Some(desc), |ui| {
                            status_pill(ui, ok, if ok { "Granted" } else { "Not granted" });
                            if !ok && ui.button("Open Settings…").clicked() {
                                crate::platform::open_privacy_pane(pane);
                            }
                        });
                    }
                },
            );
        }
        card(ui, "Configuration files", |ui| {
            for (name, path) in [
                ("Settings", self.config_path.clone()),
                ("FancyZones layouts", self.fz_path.clone()),
            ] {
                row(ui, name, Some(&path.display().to_string()), |ui| {
                    if icon_button(ui, icon::COPY, "Copy the path").clicked() {
                        ui.ctx().copy_text(path.display().to_string());
                        self.toast = Some((Instant::now(), "Path copied".into()));
                    }
                });
            }
            row(
                ui,
                "Folder",
                Some("Both files are watched; edits made by hand are picked up too."),
                |ui| {
                    if icon_text_button(ui, icon::FOLDER_OPEN, "Open folder").clicked() {
                        if let Some(d) = self.config_path.parent() {
                            crate::platform::open_folder(d);
                        }
                    }
                },
            );
        });
        card(ui, "About", |ui| {
            row(
                ui,
                &format!("PowerMac {}", env!("CARGO_PKG_VERSION")),
                Some("PowerToys-style utilities for macOS and Windows."),
                |_| {},
            );
        });
    }

    // -----------------------------------------------------------------------
    // Mouse Without Borders

    fn mwb(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let p = pal(ui);
        let this = self.app.this_machine();
        let status = self.mwb_status.clone().filter(|s| s.is_fresh());
        let mwb_on = self.app.cfg.mwb.enabled;
        let search_clicked = self.search_clicked.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        let mut search = false;
        let d = &mut self.app;
        let s = &mut d.cfg.mwb.settings;
        let show_key = &mut self.show_key;
        let confirm = &mut self.confirm_new_key;
        let copied_at = &mut self.copied_at;

        card_with(
            ui,
            "Security key",
            Some("Every machine in the matrix uses the same key. Copy it to the other machines."),
            |ui| {
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        let field_w = (ui.available_width() - 3.0 * 36.0 - 12.0).max(160.0);
                        secure_field(ui, &mut s.key, *show_key, field_w);
                        if icon_button(
                            ui,
                            if *show_key {
                                icon::EYE_SLASH
                            } else {
                                icon::EYE
                            },
                            if *show_key {
                                "Hide the key"
                            } else {
                                "Show the key"
                            },
                        )
                        .clicked()
                        {
                            *show_key = !*show_key;
                        }
                        if icon_button(ui, icon::COPY, "Copy the key").clicked() {
                            ui.ctx().copy_text(mwb::normalize_key(&s.key));
                            *copied_at = Some(Instant::now());
                        }
                        if icon_button(ui, icon::ARROWS_CLOCKWISE, "Generate a new key…").clicked()
                        {
                            *confirm = true;
                        }
                    });
                    if let Some(e) = validate_key(&s.key) {
                        ui.add_space(6.0);
                        error_label(ui, &e);
                    } else if copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                        ui.add_space(6.0);
                        ok_label(ui, "Copied to the clipboard.");
                        ui.ctx().request_repaint_after(Duration::from_millis(300));
                    }
                });
            },
        );
        if *confirm && ui.is_visible() {
            let r = egui::Modal::new(Id::unique("new-key")).frame(dialog_frame(ui)).show(ui.ctx(), |ui| {
                ui.set_width(380.0);
                dialog_title(ui, icon::KEY, "Generate a new security key?");
                hint(ui, "The other machines stay disconnected until you enter the new key on each of them.");
                ui.add_space(18.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "Generate").clicked() {
                        s.key = mwb::create_random_key();
                        *show_key = true;
                        *confirm = false;
                    }
                    if ui.button("Cancel").clicked() {
                        *confirm = false;
                    }
                });
            });
            if r.should_close() {
                *confirm = false;
            }
        }

        card_with(
            ui,
            "Other computers",
            Some("Computers on this network with the same security key are found and connected automatically; PowerToys on Windows then adds this machine to its layout too."),
            |ui| {
                toggle_row(
                    ui,
                    "Find other computers automatically",
                    Some("Looks for them on this Mac's local networks every minute until one is connected, then every 5 minutes."),
                    &mut s.auto_discover,
                );
                let running = status.as_ref().is_some_and(|st| st.running);
                let searching = search_clicked || status.as_ref().is_some_and(|st| st.searching);
                let desc = if !mwb_on {
                    "Turn on Mouse Without Borders to search."
                } else if !running {
                    "PowerMac isn't running, so it can't search right now."
                } else if searching {
                    "Searching this network for computers with the same key…"
                } else {
                    "Look for other computers on this network right away."
                };
                row(ui, "Search now", Some(desc), |ui| {
                    ui.add_enabled_ui(mwb_on && running && !searching, |ui| {
                        let label = if searching { "Searching…" } else { "Search now" };
                        if icon_text_button(ui, icon::MAGNIFYING_GLASS, label).clicked() {
                            search = true;
                        }
                    });
                });
                block(ui, |ui| {
                    machine_list(ui, status.as_ref(), mwb_on);
                });
            },
        );

        card_with(
            ui,
            "Machine layout",
            Some("Up to four machines, in the order the cursor moves between them. Drag a screen to reorder; use the same layout on every machine."),
            |ui| {
                row(ui, "Arrangement", None, |ui| {
                    segmented(ui, &mut s.one_row, &[(true, "One row"), (false, "Two rows (2×2)")]);
                });
                block(ui, |ui| {
                    ui.add_space(4.0);
                    machine_matrix(ui, &mut d.matrix, s.one_row, &this);
                });
                if !d.matrix.iter().any(|m| is_this_machine(m, &this)) {
                    let can = d.matrix.iter().any(|m| m.trim().is_empty());
                    let add = row(ui, &format!("{this} isn't in the layout"), Some("This machine has to be in the layout to share its mouse and keyboard."), |ui| {
                        ui.add_enabled(can, egui::Button::new("Add this machine")).clicked()
                    });
                    if add {
                        matrix_add(&mut d.matrix, &this);
                    }
                }
                let errs = validate_matrix(&d.matrix);
                if !errs.is_empty() {
                    block(ui, |ui| {
                        for e in errs {
                            error_label(ui, &e);
                        }
                    });
                }
            },
        );

        card(ui, "This machine", |ui| {
            row(
                ui,
                "Machine name",
                Some("How the other machines see this one. Leave empty to use the host name."),
                |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut s.machine_name)
                            .hint_text(this.clone())
                            .desired_width(200.0),
                    );
                },
            );
            if let Some(e) = validate_machine_name(&s.machine_name) {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
            row(ui, "Announced as", None, |ui| {
                ui.label(
                    RichText::new(&this)
                        .font(theme::font_semibold(13.5))
                        .color(p.text),
                );
            });
        });

        card_with(ui, "Machine addresses", Some("Machines missing here are found by name. Add an address (host, IP, or host:port) when a name doesn't resolve."), |ui| {
            let mut remove = None;
            for i in 0..d.peers.len() {
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 84.0) / 2.0;
                        ui.add(egui::TextEdit::singleline(&mut d.peers[i].name).hint_text("Machine name").desired_width(w));
                        ui.add(egui::TextEdit::singleline(&mut d.peers[i].address).hint_text("192.168.1.20").desired_width(w));
                        if icon_button(ui, icon::TRASH, "Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                    if let Some(e) = validate_peer(&d.peers, i) {
                        error_label(ui, &e);
                    }
                });
            }
            if let Some(i) = remove {
                d.peers.remove(i);
            }
            block(ui, |ui| {
                if icon_text_button(ui, icon::PLUS, "Add address").clicked() {
                    d.peers.push(PeerRow::default());
                }
            });
        });

        card(ui, "Behavior", |ui| {
            toggle_row(
                ui,
                "Easy Mouse",
                Some("Switch machines when the cursor reaches a screen edge."),
                &mut s.easy_mouse,
            );
            toggle_row(
                ui,
                "Wrap around",
                Some("Leave the last machine to reach the first one."),
                &mut s.wrap_around,
            );
            toggle_row(
                ui,
                "Hide the cursor while away",
                Some("Hide the cursor here while controlling another machine."),
                &mut s.hide_cursor_when_away,
            );
            toggle_row(
                ui,
                "Move the mouse relatively",
                Some("Try this if the cursor jumps when it moves to another machine."),
                &mut s.move_mouse_relatively,
            );
            toggle_row(ui, "Share clipboard text", None, &mut s.share_clipboard);
            toggle_row(
                ui,
                "Swap Cmd and Ctrl",
                Some("So Cmd+C here arrives as Ctrl+C on Windows."),
                &mut s.swap_cmd_ctrl,
            );
            toggle_row(
                ui,
                "Accept layouts from other machines",
                Some("Use the machine layout sent by another machine."),
                &mut s.accept_remote_matrix,
            );
        });

        card(ui, "Network", |ui| {
            let msg = format!(
                "Messages use TCP {} (port + 1); allow it through the firewall on every machine. PowerToys' default is 15100.",
                s.port.saturating_add(1)
            );
            row(ui, "Port", Some(&msg), |ui| {
                ui.add_sized(
                    vec2(72.0, 24.0),
                    egui::DragValue::new(&mut s.port).range(1..=65534),
                );
            });
            row(
                ui,
                "Listen address",
                Some("The local address to listen on (0.0.0.0 for all)."),
                |ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.bind_address).desired_width(160.0));
                },
            );
        });
        if search {
            // Save first so a just-changed setting is in effect when it searches.
            self.save_app();
            match pm_config::DiscoverRequest::write_for(&self.config_path) {
                Ok(()) => {
                    self.search_clicked = Some(Instant::now());
                    self.toast = Some((Instant::now(), "Searching the local network…".into()));
                }
                Err(e) => self.app_file.save_error = Some(format!("{e:#}")),
            }
        }
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
                label: "All monitors".into(),
                detail: "Zones span every display".into(),
                work_area: wa,
            });
        }
        for m in &self.monitors {
            let mut detail = format!("{} × {}", m.frame.width(), m.frame.height());
            if m.primary {
                detail.push_str(" · main");
            }
            out.push(FzTarget {
                id: m.id.clone(),
                label: m.name.clone(),
                detail,
                work_area: m.work_area,
            });
        }
        for id in self.fz.applied_layouts.keys() {
            if !out.iter().any(|t| &t.id == id) {
                out.push(FzTarget {
                    id: id.clone(),
                    label: id.clone(),
                    detail: "Not connected".into(),
                    work_area: fancyzones::Rect::new(0, 0, 1920, 1080),
                });
            }
        }
        out.push(FzTarget {
            id: DEFAULT_TARGET.into(),
            label: "Other monitors".into(),
            detail: "The default layout".into(),
            work_area: fancyzones::Rect::new(0, 0, 1920, 1080),
        });
        out
    }

    /// The layout shown in the appearance preview: the selected monitor's.
    fn preview_layout(&self) -> (Vec<Rect>, (i32, i32)) {
        let targets = self.fz_targets();
        let t = targets
            .iter()
            .find(|t| t.id == self.fz_selected)
            .or(targets.first());
        let wa = t
            .map(|t| t.work_area)
            .unwrap_or(fancyzones::Rect::new(0, 0, 1920, 1080));
        let l = self
            .fz
            .layout_for(t.map(|t| t.id.as_str()).unwrap_or(DEFAULT_TARGET));
        let zones = match fancyzones::Layout::new(&l, wa, &self.fz.custom_layouts) {
            Ok(layout) if !layout.zones.is_empty() => layout.zones,
            _ => match fancyzones::Layout::new(
                &AppliedLayout::template(LayoutType::PriorityGrid, 3),
                wa,
                &[],
            ) {
                Ok(l) => l.zones,
                Err(_) => vec![],
            },
        };
        let (w, h) = (wa.width().max(1) as f32, wa.height().max(1) as f32);
        let norm = zones
            .iter()
            .map(|z| {
                Rect::from_min_max(
                    pos2((z.left - wa.left) as f32 / w, (z.top - wa.top) as f32 / h),
                    pos2(
                        (z.right - wa.left) as f32 / w,
                        (z.bottom - wa.top) as f32 / h,
                    ),
                )
            })
            .collect();
        (norm, (wa.width(), wa.height()))
    }

    fn zone_appearance(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let (zones, work) = self.preview_layout();
        let accent = self.accent;
        let fz_on = self.app.cfg.fancyzones.enabled;
        let mut flash = false;
        let s = &mut self.fz.settings;
        let mac = cfg!(target_os = "macos");
        card_with(ui, "Zone appearance", Some("How the zones look while you hold Shift and drag a window. Changes apply right away."), |ui| {
            block(ui, |ui| {
                ui.add_space(4.0);
                let wide = ui.available_width() > 600.0;
                let pw = if wide { (ui.available_width() * 0.52).min(400.0) } else { ui.available_width() };
                let ph = (pw * 0.64).round();
                let colors = zone_style::effective(s, accent);
                let numbers = s.show_zone_number;
                let draw_preview = |ui: &mut Ui| {
                    ui.vertical(|ui| {
                        ui.set_width(pw);
                        zone_style::desktop_preview(ui, vec2(pw, ph), work, &zones, &colors, numbers, 0);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(theme::icon_text(icon::CURSOR).size(12.0).color(p.text3));
                            ui.label(RichText::new("Live preview · point at a zone to highlight it").size(11.5).color(p.text2));
                        });
                    });
                };
                let controls = |ui: &mut Ui, s: &mut fancyzones::Settings| {
                    ui.vertical(|ui| {
                        reset_rows(ui);
                        let accent_on = s.use_system_accent;
                        let from_accent = Some("From the system accent");
                        ui.add_enabled_ui(!accent_on, |ui| {
                            row(ui, "Zone", if accent_on { from_accent } else { None }, |ui| hex_color_edit(ui, &mut s.zone_color));
                            row(ui, "Highlight", if accent_on { from_accent } else { Some("Zone under the pointer") }, |ui| {
                                hex_color_edit(ui, &mut s.zone_highlight_color)
                            });
                        });
                        row(ui, "Border", None, |ui| hex_color_edit(ui, &mut s.zone_border_color));
                        ui.add_enabled_ui(s.show_zone_number, |ui| {
                            row(ui, "Number", None, |ui| hex_color_edit(ui, &mut s.zone_number_color));
                        });
                        row(ui, "Opacity", None, |ui| {
                            slider(ui, &mut s.zone_highlight_opacity, 0..=100, " %");
                        });
                    });
                };
                if wide {
                    ui.horizontal_top(|ui| {
                        draw_preview(ui);
                        ui.add_space(22.0);
                        controls(ui, s);
                    });
                } else {
                    draw_preview(ui);
                    ui.add_space(8.0);
                    controls(ui, s);
                }
            });
            continue_rows(ui);
            block(ui, |ui| {
                ui.label(RichText::new("Presets").font(theme::font_semibold(13.0)).color(p.text));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let gap = 8.0;
                    ui.spacing_mut().item_spacing = vec2(gap, gap);
                    let cols = if ui.available_width() > 560.0 { 4.0 } else { 2.0 };
                    let cw = ((ui.available_width() - gap * (cols - 1.0)) / cols).floor();
                    for preset in zone_style::PRESETS {
                        if zone_style::preset_chip(ui, cw, preset.name, &preset.colors(), preset.matches(s))
                            .on_hover_text(format!("Zone {} · highlight {} · {} % opacity", preset.zone, preset.highlight, preset.opacity))
                            .clicked()
                        {
                            preset.apply(s);
                        }
                    }
                    let mut acc = zone_style::effective(s, accent);
                    if let Some(a) = accent {
                        let z = fancyzones::engine::accent_zone_color(a);
                        acc.highlight = Color32::from_rgb(a.0, a.1, a.2);
                        acc.zone = Color32::from_rgb(z.0, z.1, z.2);
                    }
                    let tip = if accent.is_some() { "Use the system accent color for the zones" } else { "The system accent color isn't available here" };
                    let r = ui.add_enabled_ui(accent.is_some(), |ui| zone_style::preset_chip(ui, cw, "System accent", &acc, s.use_system_accent)).inner;
                    if r.on_hover_text(tip).clicked() {
                        zone_style::apply_system_accent(s, accent);
                    }
                });
            });
            let accent_desc = if mac {
                "Zones follow the accent color in System Settings → Appearance."
            } else {
                "Zones follow the accent color in Settings → Personalization → Colors."
            };
            let mut use_accent = s.use_system_accent;
            if toggle_row(ui, "Use system accent color", Some(accent_desc), &mut use_accent).changed() {
                if use_accent {
                    zone_style::apply_system_accent(s, accent);
                } else {
                    s.use_system_accent = false;
                }
            }
            toggle_row(ui, "Show zone numbers", None, &mut s.show_zone_number);
            toggle_row(ui, "Flash zones when the active layout changes", None, &mut s.zone_set_change_flash_zones);
            let desc = if fz_on {
                "Briefly shows the zones on your displays with these colors."
            } else {
                "Turn on FancyZones to preview the zones on your displays."
            };
            row(ui, "Preview on screen", Some(desc), |ui| {
                ui.add_enabled_ui(fz_on, |ui| {
                    if icon_text_button(ui, icon::LIGHTNING, "Show on screen").clicked() {
                        flash = true;
                    }
                });
            });
        });
        if flash {
            // Save first so the running app has the new colors when it flashes.
            self.save_fz();
            let monitor = (self.fz_selected != DEFAULT_TARGET
                && self.monitors.iter().any(|m| m.id == self.fz_selected))
            .then(|| self.fz_selected.clone());
            match request_zone_flash(&self.config_path, monitor) {
                Ok(()) => self.toast = Some((Instant::now(), "Showing the zones on screen".into())),
                Err(e) => self.fz_file.save_error = Some(format!("{e:#}")),
            }
        }
    }

    fn fancyzones(&mut self, ui: &mut Ui) {
        let config_path = self.config_path.clone();
        self.hero(ui, |me, ui| {
            ui.add_space(14.0);
            divider(ui);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let p = pal(ui);
                ui.label(
                    RichText::new("Create custom layouts, edit zones and assign layout hotkeys.")
                        .color(p.text2),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_icon_button(ui, icon::LAYOUT, "Open layout editor").clicked() {
                        let monitor =
                            (me.fz_selected != DEFAULT_TARGET).then(|| me.fz_selected.clone());
                        if let Err(e) =
                            crate::editor::open_editor_process(&config_path, monitor.as_deref())
                        {
                            log::warn!("layout editor: {e:#}");
                        }
                    }
                });
            });
        });
        let targets = self.fz_targets();
        if !targets.iter().any(|t| t.id == self.fz_selected) {
            self.fz_selected = DEFAULT_TARGET.into();
        }

        self.zone_appearance(ui);

        let mut refresh = false;
        card_with(
            ui,
            "Layouts",
            Some("The layout each monitor uses. Open the layout editor to create custom layouts."),
            |ui| {
                block(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let list_w = 210.0;
                        ui.vertical(|ui| {
                            ui.set_width(list_w);
                            ui.spacing_mut().item_spacing.y = 4.0;
                            for t in &targets {
                                let has_own = t.id == DEFAULT_TARGET
                                    || self.fz.applied_layouts.contains_key(&t.id);
                                let glyph = if t.id == DEFAULT_TARGET {
                                    icon::STACK
                                } else {
                                    icon::MONITOR
                                };
                                let detail = if has_own {
                                    t.detail.clone()
                                } else {
                                    format!("{} · default", t.detail)
                                };
                                if list_item(
                                    ui,
                                    glyph,
                                    &t.label,
                                    &detail,
                                    self.fz_selected == t.id,
                                    list_w,
                                )
                                .clicked()
                                {
                                    self.fz_selected = t.id.clone();
                                }
                            }
                            ui.add_space(4.0);
                            let p = pal(ui);
                            if ui
                                .add(
                                    egui::Button::new(theme::icon_job(
                                        icon::ARROWS_CLOCKWISE,
                                        "Refresh monitors",
                                        12.0,
                                        p.accent,
                                    ))
                                    .frame(false),
                                )
                                .clicked()
                            {
                                refresh = true;
                            }
                        });
                        ui.add_space(20.0);
                        ui.vertical(|ui| {
                            let Some(t) = targets.iter().find(|t| t.id == self.fz_selected) else {
                                return;
                            };
                            let custom = self.fz.custom_layouts.clone();
                            let fz = &mut self.fz;
                            let l = fz.layout_for(&t.id);
                            zones_preview(ui, &l, t.work_area, &custom, &fz.settings);
                            ui.add_space(4.0);
                            reset_rows(ui);
                            if t.id == DEFAULT_TARGET {
                                layout_editor(
                                    ui,
                                    "default",
                                    &mut fz.settings.default_layout,
                                    &custom,
                                );
                                hint(ui, "Used by monitors without a layout of their own.");
                            } else if fz.applied_layouts.contains_key(&t.id) {
                                let mut reset = false;
                                if let Some(l) = fz.applied_layouts.get_mut(&t.id) {
                                    layout_editor(ui, &t.id, l, &custom);
                                }
                                ui.add_space(8.0);
                                if ui.button("Use the default layout instead").clicked() {
                                    reset = true;
                                }
                                if reset {
                                    fz.applied_layouts.remove(&t.id);
                                }
                            } else {
                                ui.add_space(8.0);
                                hint(ui, "This monitor uses the default layout.");
                                ui.add_space(6.0);
                                if ui.button("Give this monitor its own layout").clicked() {
                                    fz.applied_layouts
                                        .insert(t.id.clone(), fz.settings.default_layout.clone());
                                }
                            }
                            if let Some(e) = validate_layout(&l, &custom) {
                                error_label(ui, &format!("This layout can't be used: {e}"));
                            }
                        });
                    });
                });
            },
        );
        if refresh {
            self.refresh_monitors();
        }

        let s = &mut self.fz.settings;
        let mods = mod_names();
        let rec = &mut self.recording;
        let bufs = &mut self.fz_hk_text;
        let mac = cfg!(target_os = "macos");
        card(ui, "Activation", |ui| {
            toggle_row(
                ui,
                "Hold Shift to show zones",
                Some(if s.shift_drag {
                    "Zones appear while you hold Shift and drag a window."
                } else {
                    "Zones show whenever you drag a window; holding Shift hides them."
                }),
                &mut s.shift_drag,
            );
            toggle_row(
                ui,
                "Right-click toggles zones",
                Some("Press the right mouse button while dragging to show or hide zones."),
                &mut s.mouse_switch,
            );
            toggle_row(
                ui,
                "Middle-click snaps across zones",
                Some("Press the middle mouse button while dragging to snap across multiple zones."),
                &mut s.mouse_middle_click_spanning_multiple_zones,
            );
            toggle_row(
                ui,
                "Show zones on all monitors while dragging",
                None,
                &mut s.show_zones_on_all_monitors,
            );
            toggle_row(
                ui,
                "Allow zones to span across monitors",
                None,
                &mut s.span_zones_across_monitors,
            );
            ui.add_enabled_ui(!mac, |ui| {
                toggle_row(
                    ui,
                    "Make the dragged window transparent",
                    if mac { Some("Windows only.") } else { None },
                    &mut s.make_dragged_window_transparent,
                );
            });
            use fancyzones::layout::OverlappingZonesAlgorithm as O;
            row(
                ui,
                "When zones overlap",
                Some("Which zone a window snaps to when several are under the pointer."),
                |ui| {
                    combo(
                        ui,
                        "overlap",
                        &mut s.overlapping_zones_algorithm,
                        &[
                            (O::Smallest, "Smallest zone"),
                            (O::Largest, "Largest zone"),
                            (O::Positional, "Split by position"),
                            (O::ClosestCenter, "Closest center"),
                        ],
                    );
                },
            );
            let err = row(
                ui,
                "Open the layout editor",
                Some("Opens the editor on the monitor under the pointer."),
                |ui| fz_hotkey_edit(ui, rec, bufs, "editor", &mut s.editor_hotkey),
            );
            if let Some(e) = err {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
        });

        card(ui, "Window behavior", |ui| {
            toggle_row(
                ui,
                "Keep windows in their zones when displays change",
                Some("When the screen resolution or the connected displays change."),
                &mut s.display_change_move_windows,
            );
            toggle_row(
                ui,
                "Move windows when a monitor's layout changes",
                None,
                &mut s.zone_set_change_move_windows,
            );
            toggle_row(
                ui,
                "Move new windows to their last zone",
                None,
                &mut s.app_last_zone_move_windows,
            );
            toggle_row(
                ui,
                "Open new windows on the active monitor",
                Some("The monitor with the mouse pointer."),
                &mut s.open_window_on_active_monitor,
            );
            toggle_row(
                ui,
                "Restore the original size when a window leaves a zone",
                None,
                &mut s.restore_size,
            );
            toggle_row(
                ui,
                if mac {
                    "Allow dialogs to be snapped"
                } else {
                    "Allow child windows to be snapped"
                },
                None,
                &mut s.allow_snap_child_windows,
            );
            toggle_row(
                ui,
                if mac {
                    "Allow floating panels to be snapped"
                } else {
                    "Allow popup windows to be snapped"
                },
                None,
                &mut s.allow_snap_popup_windows,
            );
            ui.add_enabled_ui(!mac, |ui| {
                toggle_row(
                    ui,
                    "Disable round corners when a window is snapped",
                    Some("Windows 11 only."),
                    &mut s.disable_round_corners,
                );
            });
            toggle_row(
                ui,
                if mac {
                    "Use a separate layout on each Space"
                } else {
                    "Use a separate layout on each virtual desktop"
                },
                None,
                &mut s.layouts_per_virtual_desktop,
            );
        });

        card(ui, "Keyboard snapping", |ui| {
            toggle_row(
                ui,
                "Snap the focused window with the keyboard",
                Some("Use a modifier + the arrow keys."),
                &mut s.override_snap_hotkeys,
            );
            ui.add_enabled_ui(s.override_snap_hotkeys, |ui| {
                row(ui, "Snap", Some("Modifiers + arrow keys"), |ui| {
                    modifiers_edit(ui, &mut s.snap_modifiers, &mods)
                });
                if s.snap_modifiers == fancyzones::Modifiers::default() {
                    error_label(
                        ui,
                        "Pick at least one modifier for snapping, or the hotkeys won't fire.",
                    );
                    ui.add_space(8.0);
                }
                row(
                    ui,
                    "Extend",
                    Some("Modifiers + arrow keys, when moving by position"),
                    |ui| modifiers_edit(ui, &mut s.extend_modifiers, &mods),
                );
                row(ui, "Arrow keys move", None, |ui| {
                    segmented(
                        ui,
                        &mut s.move_windows_based_on_position,
                        &[(false, "By zone number"), (true, "By position")],
                    );
                });
                toggle_row(
                    ui,
                    "Move windows between monitors",
                    None,
                    &mut s.move_window_across_monitors,
                );
            });
        });

        card(ui, "Edge snapping", |ui| {
            let desc = if cfg!(windows) {
                "Windows Snap already does this; leave it off unless you've turned Snap off."
            } else {
                "Drag a window to a screen edge or corner to snap it (when zones are hidden)."
            };
            toggle_row(
                ui,
                "Snap windows at screen edges",
                Some(desc),
                &mut s.edge_snap_enabled,
            );
            ui.add_enabled_ui(s.edge_snap_enabled, |ui| {
                row(ui, "Edge distance", None, |ui| {
                    slider(ui, &mut s.edge_snap_margin, 1..=50, " px")
                });
                row(ui, "Corner size", None, |ui| {
                    slider(ui, &mut s.edge_snap_corner_size, 0..=200, " px")
                });
                row(ui, "Gap between windows", None, |ui| {
                    slider(ui, &mut s.edge_snap_gap, 0..=50, " px")
                });
                use fancyzones::{BottomEdgeAction as B, TopEdgeAction as T};
                row(ui, "Top edge", None, |ui| {
                    segmented(
                        ui,
                        &mut s.edge_snap_top,
                        &[
                            (T::Maximize, "Maximize"),
                            (T::TopHalf, "Top half"),
                            (T::None, "Nothing"),
                        ],
                    )
                });
                row(ui, "Bottom edge", None, |ui| {
                    segmented(
                        ui,
                        &mut s.edge_snap_bottom,
                        &[
                            (B::BottomHalf, "Bottom half"),
                            (B::Thirds, "Thirds"),
                            (B::None, "Nothing"),
                        ],
                    )
                });
            });
        });

        card(ui, "Switching between windows in a zone", |ui| {
            toggle_row(
                ui,
                "Cycle through windows in the same zone",
                Some("With a shortcut, like tabs."),
                &mut s.window_switching,
            );
            ui.add_enabled_ui(s.window_switching, |ui| {
                for (label, key, hk) in [
                    ("Next window", "next-tab", &mut s.next_tab_hotkey),
                    ("Previous window", "prev-tab", &mut s.prev_tab_hotkey),
                ] {
                    if let Some(e) =
                        row(ui, label, None, |ui| fz_hotkey_edit(ui, rec, bufs, key, hk))
                    {
                        error_label(ui, &e);
                        ui.add_space(8.0);
                    }
                }
            });
        });

        card(ui, "Layout switching", |ui| {
            let qs = format!(
                "{}+0…9 applies the layout bound to that number (set it in the layout editor).",
                quick_switch_mods()
            );
            toggle_row(
                ui,
                "Switch layouts with quick keys",
                Some(&qs),
                &mut s.quick_layout_switch,
            );
            ui.add_enabled_ui(s.quick_layout_switch, |ui| {
                toggle_row(
                    ui,
                    "Flash zones when switching layouts",
                    None,
                    &mut s.flash_zones_on_quick_switch,
                );
            });
            toggle_row(
                ui,
                "Scroll the mouse wheel while dragging to switch layouts",
                None,
                &mut s.mouse_wheel_layout_switch,
            );
            toggle_row(
                ui,
                "Show the layout name when switching",
                None,
                &mut s.layout_name_label_enabled,
            );
            ui.add_enabled_ui(s.layout_name_label_enabled, |ui| {
                use fancyzones::LayoutNameLabelPlacement as P;
                row(ui, "Position", None, |ui| {
                    combo(
                        ui,
                        "label-pos",
                        &mut s.layout_name_label_placement,
                        &[
                            (P::TopLeft, "Top left"),
                            (P::TopCenter, "Top center"),
                            (P::TopRight, "Top right"),
                            (P::Center, "Center"),
                            (P::BottomLeft, "Bottom left"),
                            (P::BottomCenter, "Bottom center"),
                            (P::BottomRight, "Bottom right"),
                        ],
                    )
                });
                row(ui, "Text color", None, |ui| {
                    hex_color_edit(ui, &mut s.layout_name_label_text_color)
                });
                row(ui, "Background color", None, |ui| {
                    hex_color_edit(ui, &mut s.layout_name_label_background_color)
                });
                row(ui, "Font size", None, |ui| {
                    slider(ui, &mut s.layout_name_label_font_size, 6..=200, " pt")
                });
                row(ui, "Padding", None, |ui| {
                    slider(ui, &mut s.layout_name_label_padding, 0..=100, " pt")
                });
                row(ui, "Show for", None, |ui| {
                    slider(
                        ui,
                        &mut s.layout_name_label_duration_ms,
                        200..=10_000,
                        " ms",
                    )
                });
            });
        });

        card(ui, "Monitor rotation", |ui| {
            toggle_row(
                ui,
                "Move all windows to the next or previous monitor",
                Some("Hold the shortcut, then press ← or →."),
                &mut s.monitor_rotation,
            );
            ui.add_enabled_ui(s.monitor_rotation, |ui| {
                if let Some(e) = row(ui, "Shortcut", None, |ui| {
                    fz_hotkey_edit(ui, rec, bufs, "rotation", &mut s.monitor_rotation_hotkey)
                }) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
            });
        });

        let new_app = &mut self.fz_new_app;
        let running = &mut self.running_apps;
        let excluded_desc = if mac {
            "Windows of these apps are never snapped. Use bundle ids (com.apple.Safari), app names (Safari) or bundle names (Safari.app)."
        } else {
            "Windows of these apps are never snapped. Use exe names (notepad.exe), a part of the path, or a part of the window title."
        };
        card_with(ui, "Excluded apps", Some(excluded_desc), |ui| {
            let mut remove = None;
            for (i, a) in s.excluded_apps.iter().enumerate() {
                if row(ui, a, None, |ui| {
                    icon_button(ui, icon::TRASH, "Remove").clicked()
                }) {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                s.excluded_apps.remove(i);
            }
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(new_app)
                            .desired_width(240.0)
                            .hint_text(if mac { "com.example.App" } else { "app.exe" }),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (icon_text_button(ui, icon::PLUS, "Add").clicked() || enter)
                        && !new_app.trim().is_empty()
                    {
                        add_excluded(&mut s.excluded_apps, new_app.trim());
                        new_app.clear();
                    }
                    if mac {
                        let resp = ui.menu_button(
                            theme::icon_job(icon::APP_WINDOW, "Running apps…", 13.0, pal(ui).text),
                            |ui| {
                                let apps =
                                    running.get_or_insert_with(crate::platform::running_apps);
                                egui::ScrollArea::vertical()
                                    .max_height(300.0)
                                    .show(ui, |ui| {
                                        for (name, id) in apps.iter() {
                                            let taken = s
                                                .excluded_apps
                                                .iter()
                                                .any(|a| a.eq_ignore_ascii_case(id));
                                            if ui
                                                .add_enabled(
                                                    !taken,
                                                    egui::Button::new(format!("{name}  —  {id}"))
                                                        .frame(false),
                                                )
                                                .clicked()
                                            {
                                                add_excluded(&mut s.excluded_apps, id);
                                                ui.close();
                                            }
                                        }
                                        if apps.is_empty() {
                                            ui.weak("No running apps found.");
                                        }
                                    });
                            },
                        );
                        if resp.response.clicked() {
                            *running = None; // refresh the list each time it opens
                        }
                    }
                });
            });
        });

        card(ui, "Shortcuts", |ui| {
            let m = |x: &fancyzones::Modifiers| fz_mods_text(x);
            let mut line = |action: &str, keys: String, on: bool| {
                row(ui, action, if on { None } else { Some("Off") }, |ui| {
                    ui.add_enabled_ui(on, |ui| keycaps(ui, &keys));
                });
            };
            let arrows = if s.move_windows_based_on_position {
                "Arrows"
            } else {
                "←/→"
            };
            line(
                "Snap the focused window",
                format!("{}+{arrows}", m(&s.snap_modifiers)),
                s.override_snap_hotkeys,
            );
            line(
                "Extend across zones",
                format!("{}+Arrows", m(&s.extend_modifiers)),
                s.override_snap_hotkeys && s.move_windows_based_on_position,
            );
            line(
                "Open the layout editor",
                hk_text(&s.editor_hotkey),
                s.editor_hotkey.code != 0,
            );
            line(
                "Next window in zone",
                hk_text(&s.next_tab_hotkey),
                s.window_switching,
            );
            line(
                "Previous window in zone",
                hk_text(&s.prev_tab_hotkey),
                s.window_switching,
            );
            line(
                "Apply layout 0–9",
                format!("{}+0…9", quick_switch_mods()),
                s.quick_layout_switch,
            );
            line(
                "Move all windows to the next monitor",
                format!("{}+←/→", hk_text(&s.monitor_rotation_hotkey)),
                s.monitor_rotation,
            );
        });
    }

    // -----------------------------------------------------------------------
    // Awake

    fn awake(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let a = &mut self.app.awake;
        card_with(
            ui,
            "Mode",
            Some("The tray's Awake menu changes the current state without editing these settings."),
            |ui| {
                row(ui, "Keep awake", None, |ui| {
                    segmented(
                        ui,
                        &mut a.kind,
                        &[
                            (AwakeKind::Off, "Off"),
                            (AwakeKind::Indefinite, "Indefinitely"),
                            (AwakeKind::Timed, "For a while"),
                            (AwakeKind::Until, "Until"),
                        ],
                    );
                });
                match a.kind {
                    AwakeKind::Off => {
                        block(ui, |ui| {
                            hint(ui, "The computer sleeps according to its power settings.")
                        });
                    }
                    AwakeKind::Indefinite => {
                        block(ui, |ui| {
                            hint(ui, "The computer stays awake until you change this.")
                        });
                    }
                    AwakeKind::Timed => {
                        row(
                            ui,
                            "Duration",
                            Some("Counted from when the setting is applied."),
                            |ui| {
                                ui.add_sized(
                                    vec2(72.0, 24.0),
                                    egui::DragValue::new(&mut a.hours)
                                        .range(0..=999)
                                        .suffix(" h"),
                                );
                                ui.add_sized(
                                    vec2(72.0, 24.0),
                                    egui::DragValue::new(&mut a.minutes)
                                        .range(0..=59)
                                        .suffix(" min"),
                                );
                            },
                        );
                        if let Err(e) = a.to_config(Local::now()) {
                            error_label(ui, &e);
                            ui.add_space(8.0);
                        }
                    }
                    AwakeKind::Until => {
                        let now = Local::now();
                        let resolved = match a.to_config(now) {
                            Ok(c) => match c.mode {
                                awake::AwakeMode::Expirable(t) => {
                                    Some(format!("Until {}", t.format("%a %e %b %Y, %H:%M")))
                                }
                                _ => None,
                            },
                            Err(_) => None,
                        };
                        row(
                            ui,
                            "Until",
                            Some(resolved.as_deref().unwrap_or("HH:MM or YYYY-MM-DD HH:MM")),
                            |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut a.until)
                                        .hint_text("HH:MM")
                                        .desired_width(160.0),
                                );
                            },
                        );
                        if let Err(e) = a.to_config(now) {
                            error_label(ui, &e);
                            ui.add_space(8.0);
                        }
                        if let Some(w) = a.warning(now) {
                            warn_label(ui, &w);
                            ui.add_space(8.0);
                        }
                    }
                }
                toggle_row(
                    ui,
                    "Keep the screen on",
                    Some("Otherwise only the system stays awake and the display may sleep."),
                    &mut a.keep_display_on,
                );
            },
        );
    }

    // -----------------------------------------------------------------------
    // Scroll Reverser

    fn scroll(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let s = &mut self.app.cfg.scroll_reverser;
        card(ui, "Mouse wheel", |ui| {
            toggle_row(
                ui,
                "Reverse vertical scrolling",
                None,
                &mut s.reverse_mouse_vertical,
            );
            toggle_row(
                ui,
                "Reverse horizontal scrolling",
                None,
                &mut s.reverse_mouse_horizontal,
            );
        });
        card(ui, "Trackpad", |ui| {
            toggle_row(
                ui,
                "Reverse vertical scrolling",
                None,
                &mut s.reverse_trackpad_vertical,
            );
            toggle_row(
                ui,
                "Reverse horizontal scrolling",
                None,
                &mut s.reverse_trackpad_horizontal,
            );
            if cfg!(windows) {
                toggle_row(
                    ui,
                    "Treat small wheel steps as touchpad scrolling",
                    Some("Windows doesn't say which device scrolled. Precision touchpads send small steps, but so do some high-resolution mice."),
                    &mut s.windows_touchpad_heuristic,
                );
            }
        });
        if cfg!(target_os = "macos") {
            let natural = scroll_reverser::system_natural_scrolling();
            card(ui, "macOS natural scrolling", |ui| {
                let text = match natural {
                    Some(true) => "On in System Settings, so these switches flip that: reversing the mouse wheel gives classic wheel scrolling while the trackpad stays natural.",
                    Some(false) => "Off in System Settings, so reversing a device here makes it scroll the natural way.",
                    None => "Reversal is relative to the System Settings natural scrolling switch.",
                };
                let on = natural
                    .map(|n| if n { "On" } else { "Off" })
                    .unwrap_or("Unknown");
                row(ui, "Natural scrolling", Some(text), |ui| {
                    let p = pal(ui);
                    ui.label(RichText::new(on).color(p.text2));
                });
                row(ui, "Magic Mouse", Some("The Magic Mouse counts as a trackpad, as does Mouse Without Borders input."), |_| {});
            });
        }
    }

    // -----------------------------------------------------------------------
    // Mouse Utilities

    fn mouse_utils(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let rec = &mut self.recording;
        let d = &mut self.app;
        let m = &mut d.cfg.mouse_utils;

        card(ui, "Find My Mouse", |ui| {
            let f = &mut m.find_my_mouse;
            toggle_row(
                ui,
                "Find My Mouse",
                Some("Dim the screen around the pointer to find it quickly."),
                &mut f.enabled,
            );
            ui.add_enabled_ui(f.enabled, |ui| {
                use mouse_utils::ActivationMethod as A;
                row(ui, "Activate by", None, |ui| {
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
                    )
                });
                if f.activation_method == A::Shortcut {
                    row(ui, "Shortcut", None, |ui| {
                        hotkey_edit(
                            ui,
                            rec,
                            Id::unique("find-hk"),
                            &mut d.find_hotkey,
                            RecMode::Combo,
                        )
                    });
                    if let Err(e) = parse_hotkey(&d.find_hotkey) {
                        error_label(ui, &e);
                        ui.add_space(8.0);
                    }
                }
                row(ui, "Spotlight radius", None, |ui| {
                    slider(ui, &mut f.spotlight_radius, 20..=500, " pt")
                });
                row(ui, "Backdrop color", None, |ui| {
                    color_edit(ui, &mut f.backdrop_color, false)
                });
                row(ui, "Backdrop opacity", None, |ui| {
                    percent_slider(ui, &mut f.overlay_opacity)
                });
                row(ui, "Initial zoom", None, |ui| {
                    slider(ui, &mut f.initial_zoom, 1..=15, "×")
                });
                row(ui, "Animation", None, |ui| {
                    ui.add_sized(
                        vec2(72.0, 24.0),
                        egui::DragValue::new(&mut f.animation_duration_ms)
                            .range(0..=5000)
                            .suffix(" ms"),
                    )
                });
                row(ui, "Hide after idle", None, |ui| {
                    ui.add_sized(
                        vec2(72.0, 24.0),
                        egui::DragValue::new(&mut f.idle_timeout_ms)
                            .range(0..=10000)
                            .suffix(" ms"),
                    )
                });
                if f.activation_method.uses_shake() {
                    row(ui, "Shake distance", None, |ui| {
                        ui.add_sized(
                            vec2(72.0, 24.0),
                            egui::DragValue::new(&mut f.shake_minimum_distance)
                                .range(100..=10000)
                                .suffix(" px"),
                        )
                    });
                    row(ui, "Shake time window", None, |ui| {
                        ui.add_sized(
                            vec2(72.0, 24.0),
                            egui::DragValue::new(&mut f.shake_interval_ms)
                                .range(100..=10000)
                                .suffix(" ms"),
                        )
                    });
                    row(ui, "Shake factor", None, |ui| {
                        ui.add_sized(
                            vec2(72.0, 24.0),
                            egui::DragValue::new(&mut f.shake_factor)
                                .range(100..=2000)
                                .suffix(" %"),
                        )
                    });
                }
            });
        });

        card(ui, "Mouse Highlighter", |ui| {
            let h = &mut m.highlighter;
            toggle_row(
                ui,
                "Mouse Highlighter",
                Some("Show a circle where you click."),
                &mut h.enabled,
            );
            ui.add_enabled_ui(h.enabled, |ui| {
                row(ui, "Toggle shortcut", None, |ui| {
                    hotkey_edit(
                        ui,
                        rec,
                        Id::unique("hl-hk"),
                        &mut d.highlighter_hotkey,
                        RecMode::Combo,
                    )
                });
                if let Err(e) = parse_hotkey(&d.highlighter_hotkey) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
                row(ui, "Left button color", None, |ui| {
                    color_edit(ui, &mut h.left_button_color, true)
                });
                row(ui, "Right button color", None, |ui| {
                    color_edit(ui, &mut h.right_button_color, true)
                });
                row(ui, "Radius", None, |ui| {
                    slider(ui, &mut h.radius, 5..=100, " pt")
                });
                row(ui, "Fade delay", None, |ui| {
                    ui.add_sized(
                        vec2(72.0, 24.0),
                        egui::DragValue::new(&mut h.fade_delay_ms)
                            .range(0..=5000)
                            .suffix(" ms"),
                    )
                });
                row(ui, "Fade duration", None, |ui| {
                    ui.add_sized(
                        vec2(72.0, 24.0),
                        egui::DragValue::new(&mut h.fade_duration_ms)
                            .range(0..=5000)
                            .suffix(" ms"),
                    )
                });
                toggle_row(
                    ui,
                    "Highlight the cursor when no button is pressed",
                    None,
                    &mut h.highlight_cursor,
                );
                if h.highlight_cursor {
                    row(ui, "Cursor color", None, |ui| {
                        color_edit(ui, &mut h.always_color, true)
                    });
                }
                toggle_row(
                    ui,
                    "Turn on when PowerMac starts",
                    None,
                    &mut h.auto_activate,
                );
            });
        });

        card(ui, "Mouse Pointer Crosshairs", |ui| {
            let c = &mut m.crosshairs;
            toggle_row(
                ui,
                "Crosshairs",
                Some("Draw lines through the pointer."),
                &mut c.enabled,
            );
            ui.add_enabled_ui(c.enabled, |ui| {
                use mouse_utils::Orientation as O;
                row(ui, "Toggle shortcut", None, |ui| {
                    hotkey_edit(
                        ui,
                        rec,
                        Id::unique("ch-hk"),
                        &mut d.crosshairs_hotkey,
                        RecMode::Combo,
                    )
                });
                if let Err(e) = parse_hotkey(&d.crosshairs_hotkey) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
                row(ui, "Color", None, |ui| color_edit(ui, &mut c.color, false));
                row(ui, "Border color", None, |ui| {
                    color_edit(ui, &mut c.border_color, false)
                });
                row(ui, "Opacity", None, |ui| percent_slider(ui, &mut c.opacity));
                row(ui, "Center gap", None, |ui| {
                    slider(ui, &mut c.center_radius, 0..=500, " pt")
                });
                row(ui, "Thickness", None, |ui| {
                    slider(ui, &mut c.thickness, 1..=50, " pt")
                });
                row(ui, "Border size", None, |ui| {
                    slider(ui, &mut c.border_size, 0..=20, " pt")
                });
                row(ui, "Lines", None, |ui| {
                    segmented(
                        ui,
                        &mut c.orientation,
                        &[
                            (O::Both, "Both"),
                            (O::Vertical, "Vertical"),
                            (O::Horizontal, "Horizontal"),
                        ],
                    )
                });
                let mut fixed = c.fixed_length > 0;
                let desc = if fixed {
                    None
                } else {
                    Some("Lines reach the screen edges.")
                };
                if toggle_row(ui, "Fixed line length", desc, &mut fixed).changed() {
                    c.fixed_length = if fixed { 100 } else { 0 };
                }
                if fixed {
                    row(ui, "Line length", None, |ui| {
                        ui.add_sized(
                            vec2(72.0, 24.0),
                            egui::DragValue::new(&mut c.fixed_length)
                                .range(1..=5000)
                                .suffix(" pt"),
                        )
                    });
                }
                toggle_row(
                    ui,
                    "Turn on when PowerMac starts",
                    None,
                    &mut c.auto_activate,
                );
            });
        });
    }

    // -----------------------------------------------------------------------
    // Keyboard Manager

    fn keyboard(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let rec = &mut self.recording;
        let d = &mut self.app;

        card_with(
            ui,
            "Remap keys",
            Some("Press Record and the key to remap, or click the keys to type their names."),
            |ui| {
                remap_table(ui, rec, &mut d.key_remaps, false);
                block(ui, |ui| {
                    if icon_text_button(ui, icon::PLUS, "Add key remap").clicked() {
                        d.key_remaps.push(RemapRow::new());
                    }
                });
            },
        );
        card_with(ui, "Remap shortcuts", Some("A shortcut can send other keys, type text, open a URL or run a program — in every app or just one."), |ui| {
            remap_table(ui, rec, &mut d.shortcut_remaps, true);
            block(ui, |ui| {
                if icon_text_button(ui, icon::PLUS, "Add shortcut remap").clicked() {
                    d.shortcut_remaps.push(RemapRow::new());
                }
            });
        });

        // Runtime warnings (duplicates etc.) for the rows that parse.
        let km = keyboard_manager::Config {
            remap_keys: d
                .key_remaps
                .iter()
                .filter_map(|r| r.key_remap().ok())
                .collect(),
            remap_shortcuts: d
                .shortcut_remaps
                .iter()
                .filter_map(|r| r.shortcut_remap().ok())
                .collect(),
            ..Default::default()
        };
        let warnings = km.validate();
        if !warnings.is_empty() {
            ui.add_space(10.0);
            for w in warnings {
                warn_label(ui, &w);
            }
        }

        card(ui, "Paste as plain text", |ui| {
            let p = &mut d.cfg.keyboard_manager.paste_plain_text;
            toggle_row(
                ui,
                "Paste as plain text",
                Some("Works independently of the Keyboard Manager switch above."),
                &mut p.enabled,
            );
            ui.add_enabled_ui(p.enabled, |ui| {
                row(ui, "Shortcut", None, |ui| {
                    hotkey_edit(
                        ui,
                        rec,
                        Id::unique("paste-hk"),
                        &mut d.paste_hotkey,
                        RecMode::Combo,
                    )
                });
                if let Err(e) = parse_shortcut(&d.paste_hotkey) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
                toggle_row(
                    ui,
                    "Put the original clipboard back afterwards",
                    None,
                    &mut p.restore_clipboard,
                );
                if p.restore_clipboard {
                    row(ui, "Restore after", None, |ui| {
                        ui.add_sized(
                            vec2(72.0, 24.0),
                            egui::DragValue::new(&mut p.restore_delay_ms)
                                .range(50..=10000)
                                .suffix(" ms"),
                        )
                    });
                }
            });
        });

        card(ui, "Key names", |ui| {
            block(ui, |ui| {
                hint(
                    ui,
                    "A–Z, 0–9, F1–F24, Esc, Tab, Space, Enter, Backspace, Delete, Left/Right/Up/Down, Home, End, PageUp, PageDown, CapsLock, \
                     punctuation (; = , - . / ` [ \\ ] '). Modifiers: Ctrl, Shift, Alt/Option, Win/Cmd, with L/R for one side (LCtrl, ROption). \
                     Shortcuts join keys with +, like Ctrl+Shift+V.",
                );
            });
        });
    }
}

#[cfg(test)]
impl SettingsApp {
    pub fn set_page(&mut self, p: Page) {
        self.page = p;
    }
    pub fn set_search_for_test(&mut self, s: &str) {
        self.search = s.into();
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
        theme::ensure(&ctx);
        let p = pal(ui);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.flush();
        }
        if !self.search.trim().is_empty() {
            self.recording = None;
        }
        let query = self.search.clone();
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            self.search_hits(ui, &query)
        };
        egui::Panel::left("nav")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0, p.card_border)),
            )
            .show(ui, |ui| self.sidebar(ui, &hits));
        let mut content = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                content = ui.max_rect();
                egui::ScrollArea::vertical()
                    .id_salt(("page-scroll", self.page))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        let col = (w - 64.0).clamp(300.0, PAGE_MAX_W);
                        let left = ((w - col) / 2.0).max(16.0);
                        ui.horizontal(|ui| {
                            ui.add_space(left);
                            ui.vertical(|ui| {
                                ui.set_width(col);
                                ui.set_max_width(col);
                                if self.search.trim().is_empty() {
                                    self.page(ui);
                                } else {
                                    self.search_results(ui, &hits);
                                    ui.add_space(48.0);
                                }
                            });
                        });
                    });
            });
        self.toast(&ctx, content);
        // The search highlight lasts a few seconds.
        let hl = Id::unique("pm-highlight-until");
        if has_highlight(&ctx) {
            let until = ctx.data_mut(|d| {
                *d.get_temp_mut_or_insert_with(hl, || Instant::now() + Duration::from_secs(3))
            });
            if Instant::now() > until {
                set_highlight(&ctx, None);
                ctx.data_mut(|d| d.remove::<Instant>(hl));
            } else {
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }
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

/// The app icon: a blue gradient tile with a zones glyph.
pub(crate) fn app_logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_app_logo(ui.painter(), rect);
}

pub(crate) fn paint_app_logo(painter: &egui::Painter, rect: Rect) {
    let size = rect.width();
    let cr = (size * 0.24).round();
    let top = Color32::from_rgb(0x45, 0xA0, 0xFF);
    let bottom = Color32::from_rgb(0x2B, 0x5C, 0xE6);
    painter.add(gradient_rounded_rect(rect, cr, top, bottom));
    let inner = rect.shrink(size * 0.22);
    let g = size * 0.035;
    let w = Color32::WHITE;
    let left = Rect::from_min_max(inner.min, pos2(inner.center().x - g, inner.max.y));
    let tr = Rect::from_min_max(
        pos2(inner.center().x + g, inner.min.y),
        pos2(inner.max.x, inner.center().y - g),
    );
    let br = Rect::from_min_max(pos2(inner.center().x + g, inner.center().y + g), inner.max);
    let r = size * 0.06;
    painter.rect_filled(left, r, w);
    painter.rect_filled(tr, r, w.gamma_multiply(0.7));
    painter.rect_filled(br, r, w.gamma_multiply(0.7));
}

/// A rounded rect with a vertical gradient (top → bottom).
pub(crate) fn gradient_rounded_rect(
    rect: Rect,
    r: f32,
    top: Color32,
    bottom: Color32,
) -> egui::Shape {
    // Outline points of the rounded rect, then a fan from the center.
    let mut pts = Vec::new();
    let n = 6;
    let corners = [
        (pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (c, start) in corners {
        for k in 0..=n {
            let a = (start + 90.0 * k as f32 / n as f32).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * r);
        }
    }
    let mut mesh = egui::Mesh::default();
    let col = |p: egui::Pos2| {
        theme::mix(
            top,
            bottom,
            ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
        )
    };
    mesh.colored_vertex(rect.center(), col(rect.center()));
    for p in &pts {
        mesh.colored_vertex(*p, col(*p));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    egui::Shape::mesh(mesh)
}

fn search_field(ui: &mut Ui, text: &mut String) {
    let p = pal(ui);
    let w = ui.available_width();
    let focused_id = Id::unique("pm-search-te");
    let focused = ui.ctx().memory(|m| m.has_focus(focused_id));
    egui::Frame::new()
        .fill(if p.dark {
            Color32::from_rgb(0x2F, 0x2F, 0x33)
        } else {
            Color32::from_rgb(0xFF, 0xFF, 0xFF)
        })
        .stroke(Stroke::new(
            if focused { 1.5 } else { 1.0 },
            if focused { p.accent } else { p.card_border },
        ))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.set_width(w - 18.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(
                    theme::icon_text(icon::MAGNIFYING_GLASS)
                        .size(14.0)
                        .color(p.text2),
                );
                let clear_w = if text.is_empty() { 0.0 } else { 22.0 };
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .id(focused_id)
                        .hint_text(RichText::new("Search settings").color(p.text3))
                        .align(Align2::LEFT_CENTER)
                        .margin(egui::Margin::symmetric(0, 3))
                        .frame(egui::Frame::NONE)
                        .background_color(Color32::TRANSPARENT)
                        .desired_width(ui.available_width() - clear_w),
                );
                if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    text.clear();
                }
                if !text.is_empty()
                    && ui
                        .add(
                            egui::Button::new(theme::icon_text(icon::X).size(12.0).color(p.text2))
                                .frame(false),
                        )
                        .on_hover_text("Clear")
                        .clicked()
                {
                    text.clear();
                }
            });
        });
}

fn nav_row(
    ui: &mut Ui,
    page: Page,
    selected: bool,
    dim: bool,
    bad: bool,
    on: Option<bool>,
) -> egui::Response {
    let p = pal(ui);
    let m = meta(page);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 8.0, p.accent);
    } else if resp.hovered() {
        painter.rect_filled(
            rect,
            8.0,
            if p.dark {
                Color32::from_white_alpha(14)
            } else {
                Color32::from_black_alpha(12)
            },
        );
    }
    let alpha = if dim { 0.35 } else { 1.0 };
    let tile = Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(24.0, 24.0));
    if selected {
        painter.rect_filled(tile, 6.0, Color32::from_white_alpha(46));
        painter.text(
            tile.center(),
            Align2::CENTER_CENTER,
            m.icon,
            theme::icon_font(14.0),
            Color32::WHITE,
        );
    } else {
        paint_icon_tile(painter, tile, m.icon, m.tint.gamma_multiply(alpha));
    }
    let tc = if selected {
        p.on_accent
    } else {
        p.text.gamma_multiply(alpha)
    };
    let font = if selected {
        theme::font_semibold(13.5)
    } else {
        theme::font(13.5)
    };
    let dot_c = pos2(rect.right() - 14.0, rect.center().y);
    let clip = Rect::from_min_max(rect.min, pos2(dot_c.x - 8.0, rect.max.y));
    painter.with_clip_rect(clip).text(
        pos2(tile.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        page.title(),
        font,
        tc,
    );
    if bad {
        painter.text(
            dot_c,
            Align2::CENTER_CENTER,
            icon::WARNING_CIRCLE,
            theme::icon_font(14.0),
            if selected { Color32::WHITE } else { p.error },
        );
    } else if let Some(on) = on {
        let c = match (on, selected) {
            (true, true) => Color32::WHITE,
            (true, false) => p.ok,
            (false, true) => Color32::from_white_alpha(90),
            (false, false) => p.off,
        };
        painter.circle_filled(dot_c, 3.5, c.gamma_multiply(alpha));
    }
    resp.on_hover_text(match on {
        Some(true) => format!("{} is on", page.title()),
        Some(false) => format!("{} is off", page.title()),
        None => page.title().to_string(),
    })
}

/// A selectable list item with an icon, a title and a detail line.
fn list_item(
    ui: &mut Ui,
    glyph: &str,
    title: &str,
    detail: &str,
    selected: bool,
    w: f32,
) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 46.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if selected {
        painter.rect(
            rect,
            9.0,
            p.accent_soft,
            Stroke::new(1.0, p.accent.gamma_multiply(0.5)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 9.0, p.control);
    }
    let ic = if selected { p.accent } else { p.text2 };
    painter.text(
        pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        glyph,
        theme::icon_font(18.0),
        ic,
    );
    let tw = w - 46.0;
    let clip = Rect::from_min_size(
        pos2(rect.left() + 38.0, rect.top()),
        vec2(tw, rect.height()),
    );
    let pc = painter.with_clip_rect(clip);
    pc.text(
        pos2(clip.left(), rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        title,
        theme::font_semibold(13.0),
        p.text,
    );
    pc.text(
        pos2(clip.left(), rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        detail,
        theme::font(11.5),
        p.text2,
    );
    resp
}

/// The machines the running app is connected to or found on the network,
/// from `mwb-status.json`.
fn machine_list(ui: &mut Ui, status: Option<&pm_config::MwbStatus>, mwb_on: bool) {
    let p = pal(ui);
    ui.label(RichText::new("Computers").font(theme::font_semibold(13.0)).color(p.text));
    ui.add_space(4.0);
    let Some(st) = status.filter(|s| s.running) else {
        hint(
            ui,
            if mwb_on {
                "The connection status appears here while PowerMac is running."
            } else {
                "Mouse Without Borders is off."
            },
        );
        return;
    };
    if st.machines.is_empty() {
        hint(
            ui,
            if st.searching {
                "Searching… No other computers connected yet."
            } else {
                "No other computers connected yet. Check that they use the same security key and allow TCP 15101 through their firewall."
            },
        );
    }
    for m in &st.machines {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            ui.label(theme::icon_text(icon::MONITOR).size(18.0).color(if m.connected { p.accent } else { p.text3 }));
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(&m.name).font(theme::font_semibold(13.0)).color(p.text));
                let mut detail = if m.address.is_empty() { "Address unknown".to_string() } else { m.address.clone() };
                if m.discovered {
                    detail.push_str(" · found automatically");
                }
                ui.label(RichText::new(detail).size(11.5).color(p.text2));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if m.connected {
                    status_pill(ui, true, "Connected");
                } else {
                    tag_pill(ui, p.warn, icon::ARROWS_CLOCKWISE, "Reconnecting");
                }
            });
        });
    }
    for a in &st.other_key {
        ui.add_space(4.0);
        warn_label(ui, &format!("{a} runs Mouse Without Borders with a different security key."));
    }
}

/// A small rounded label in `c`.
fn tag_pill(ui: &mut Ui, c: Color32, glyph: &str, text: &str) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(c.gamma_multiply(if p.dark { 0.22 } else { 0.12 }))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(theme::icon_text(glyph).size(13.0).color(c));
                ui.label(RichText::new(text).size(12.0).color(c));
            });
        });
}

fn status_pill(ui: &mut Ui, ok: bool, text: &str) {
    let p = pal(ui);
    let c = if ok { p.ok } else { p.error };
    egui::Frame::new()
        .fill(c.gamma_multiply(if p.dark { 0.22 } else { 0.12 }))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    theme::icon_text(if ok {
                        icon::CHECK_CIRCLE
                    } else {
                        icon::WARNING_CIRCLE
                    })
                    .size(13.0)
                    .color(c),
                );
                ui.label(RichText::new(text).size(12.0).color(c));
            });
        });
}

/// The frame of modal dialogs.
pub(crate) fn dialog_frame(ui: &Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_border))
        .corner_radius(14.0)
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: Color32::from_black_alpha(if p.dark { 140 } else { 60 }),
        })
}

pub(crate) fn dialog_title(ui: &mut Ui, glyph: &str, title: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
        ui.painter().rect_filled(r, 8.0, p.accent_soft);
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            glyph,
            theme::icon_font(18.0),
            p.accent,
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(title)
                .font(theme::font_semibold(16.0))
                .color(p.text),
        );
    });
    ui.add_space(6.0);
}

/// The security key: a monospace field with a lock, masked unless `show`.
fn secure_field(ui: &mut Ui, key: &mut String, show: bool, w: f32) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0, p.control_border))
        .corner_radius(7.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(theme::icon_text(icon::LOCK_KEY).size(15.0).color(p.text2));
                ui.add(
                    egui::TextEdit::singleline(key)
                        .password(!show)
                        .frame(egui::Frame::NONE)
                        .background_color(Color32::TRANSPARENT)
                        .font(egui::TextStyle::Monospace)
                        .margin(egui::Margin::symmetric(2, 4))
                        .align(Align2::LEFT_CENTER)
                        .desired_width(w - 60.0),
                );
            });
        });
}

/// Machine matrix as monitor tiles: drag a tile onto another slot to move
/// it, or use the arrows; the name is edited below each tile.
fn machine_matrix(ui: &mut Ui, slots: &mut [String; MAX_MACHINE], one_row: bool, this: &str) {
    let p = pal(ui);
    let (cols, rows) = if one_row { (4, 1) } else { (2, 2) };
    let gap = 16.0;
    let avail = ui.available_width();
    let tile_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).min(if one_row {
        160.0
    } else {
        190.0
    });
    let screen_h = (tile_w * 0.6).round();
    let tile_h = screen_h + 14.0; // + stand
    let edit_h = 30.0;
    let cell_h = tile_h + 8.0 + edit_h;
    let total = vec2(
        cols as f32 * tile_w + (cols as f32 - 1.0) * gap,
        rows as f32 * cell_h + (rows as f32 - 1.0) * gap,
    );
    let (area, _) = ui.allocate_exact_size(vec2(avail, total.y), Sense::hover());
    let origin = pos2(area.left() + (avail - total.x) / 2.0, area.top());
    let cell = |i: usize| {
        let (c, r) = (i % cols, i / cols);
        Rect::from_min_size(
            origin + vec2(c as f32 * (tile_w + gap), r as f32 * (cell_h + gap)),
            vec2(tile_w, tile_h),
        )
    };
    let drag_id = ui.scope_id().with("matrix-drag");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let pointer = ui.ctx().pointer_interact_pos();
    let mut action: Option<(usize, usize)> = None;
    #[allow(clippy::needless_range_loop)] // `slots[i]` is edited in the loop
    for i in 0..MAX_MACHINE {
        let r = cell(i);
        let resp = ui.interact(r, ui.scope_id().with(("tile", i)), Sense::click_and_drag());
        let name = slots[i].trim().to_string();
        if resp.drag_started() && !name.is_empty() {
            dragging = Some(i);
        }
        let hovered_target =
            dragging.is_some_and(|d| d != i) && pointer.is_some_and(|pp| r.contains(pp));
        let lifted = dragging == Some(i);
        paint_monitor_tile(
            ui.painter(),
            r,
            i,
            &name,
            is_this_machine(&name, this),
            hovered_target,
            lifted,
            &p,
            screen_h,
        );
        if !name.is_empty() && resp.hovered() && dragging.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            // Arrows to move the tile.
            let screen = Rect::from_min_size(r.min, vec2(r.width(), screen_h));
            for (k, glyph, to) in [
                (0, icon::CARET_LEFT, i.checked_sub(1)),
                (1, icon::CARET_RIGHT, (i + 1 < MAX_MACHINE).then_some(i + 1)),
            ] {
                let Some(to) = to else { continue };
                let c = if k == 0 {
                    pos2(screen.left() + 16.0, screen.bottom() - 16.0)
                } else {
                    pos2(screen.right() - 16.0, screen.bottom() - 16.0)
                };
                let br = Rect::from_center_size(c, vec2(22.0, 22.0));
                let b = ui.interact(br, ui.scope_id().with(("arrow", i, k)), Sense::click());
                ui.painter().circle_filled(
                    c,
                    11.0,
                    if b.hovered() {
                        Color32::from_black_alpha(150)
                    } else {
                        Color32::from_black_alpha(90)
                    },
                );
                ui.painter().text(
                    c,
                    Align2::CENTER_CENTER,
                    glyph,
                    theme::icon_font(13.0),
                    Color32::WHITE,
                );
                if b.clicked() {
                    action = Some((i, to));
                }
            }
        }
        // Name field under the tile.
        let er = Rect::from_min_size(pos2(r.left(), r.bottom() + 8.0), vec2(tile_w, edit_h - 2.0));
        let mut eui = ui.new_child(
            UiBuilder::new()
                .id_salt(("matrix-name", i))
                .max_rect(er)
                .layout(Layout::left_to_right(Align::Center)),
        );
        eui.add(
            egui::TextEdit::singleline(&mut slots[i])
                .hint_text("Empty slot")
                .desired_width(tile_w - 12.0)
                .align(Align2::CENTER_CENTER),
        );
    }
    if let Some(from) = dragging {
        if ui.input(|i| i.pointer.any_released()) {
            if let Some(pp) = pointer {
                if let Some(to) = (0..MAX_MACHINE).find(|&j| cell(j).expand(gap / 2.0).contains(pp))
                {
                    action = Some((from, to));
                }
            }
            dragging = None;
        } else if let Some(pp) = pointer {
            // The lifted tile follows the pointer.
            let r = cell(from);
            let ghost = Rect::from_center_size(pp, r.size());
            let layer = egui::LayerId::new(egui::Order::Tooltip, drag_id);
            let painter = ui.ctx().layer_painter(layer);
            painter.rect_filled(
                ghost.translate(vec2(0.0, 6.0)).shrink2(vec2(4.0, 10.0)),
                12.0,
                Color32::from_black_alpha(45),
            );
            let name = slots[from].trim().to_string();
            paint_monitor_tile(
                &painter,
                ghost,
                from,
                &name,
                is_this_machine(&name, this),
                false,
                false,
                &p,
                screen_h,
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    ui.data_mut(|d| match dragging {
        Some(i) => {
            d.insert_temp(drag_id, i);
        }
        None => d.remove::<usize>(drag_id),
    });
    if let Some((from, to)) = action {
        matrix_move(slots, from, to);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_monitor_tile(
    painter: &egui::Painter,
    r: Rect,
    i: usize,
    name: &str,
    me: bool,
    target: bool,
    lifted: bool,
    p: &theme::Palette,
    screen_h: f32,
) {
    let screen = Rect::from_min_size(r.min, vec2(r.width(), screen_h));
    let empty = name.is_empty();
    if lifted {
        painter.rect_stroke(screen, 10.0, Stroke::new(1.5, p.text3), StrokeKind::Inside);
        return;
    }
    // Stand.
    let stand = if p.dark {
        Color32::from_rgb(0x58, 0x58, 0x5E)
    } else {
        Color32::from_rgb(0xC2, 0xC2, 0xC8)
    };
    if !empty {
        let neck = Rect::from_center_size(
            pos2(screen.center().x, screen.bottom() + 5.0),
            vec2(14.0, 9.0),
        );
        let foot = Rect::from_center_size(
            pos2(screen.center().x, screen.bottom() + 11.5),
            vec2(46.0, 4.0),
        );
        painter.rect_filled(neck, 1.0, stand);
        painter.rect_filled(foot, 2.0, stand);
    }
    if empty {
        let c = if target { p.accent } else { p.text3 };
        painter.rect(
            screen,
            10.0,
            if target {
                p.accent_soft
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.2, c),
            StrokeKind::Inside,
        );
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            icon::PLUS,
            theme::icon_font(18.0),
            c,
        );
    } else {
        let bezel = if p.dark {
            Color32::from_rgb(0x0E, 0x0E, 0x10)
        } else {
            Color32::from_rgb(0x26, 0x26, 0x2A)
        };
        painter.rect_filled(screen, 10.0, bezel);
        let glass = screen.shrink(4.0);
        let (a, b) = if me {
            (
                Color32::from_rgb(0x4C, 0xA4, 0xFF),
                Color32::from_rgb(0x5B, 0x55, 0xE0),
            )
        } else {
            (
                Color32::from_rgb(0x6A, 0x78, 0x96),
                Color32::from_rgb(0x3C, 0x45, 0x5E),
            )
        };
        painter.add(gradient_rounded_rect(glass, 6.5, a, b));
        let tp = painter.with_clip_rect(glass.shrink(4.0));
        tp.text(
            glass.center() - vec2(0.0, if me { 7.0 } else { 0.0 }),
            Align2::CENTER_CENTER,
            name,
            theme::font_semibold(13.0),
            Color32::WHITE,
        );
        if me {
            tp.text(
                glass.center() + vec2(0.0, 10.0),
                Align2::CENTER_CENTER,
                "This machine",
                theme::font(10.5),
                Color32::from_white_alpha(210),
            );
        }
        if target {
            painter.rect_stroke(
                screen.expand(2.0),
                12.0,
                Stroke::new(2.0, p.accent),
                StrokeKind::Outside,
            );
        }
    }
    // Slot number badge.
    let c = screen.left_top() + vec2(15.0, 15.0);
    painter.circle_filled(
        c,
        9.0,
        if empty {
            p.control
        } else {
            Color32::from_white_alpha(56)
        },
    );
    painter.text(
        c,
        Align2::CENTER_CENTER,
        (i + 1).to_string(),
        theme::font_semibold(10.5),
        if empty { p.text2 } else { Color32::WHITE },
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecMode {
    /// Modifiers + key.
    Combo,
    /// One key, no modifiers.
    Key,
}

/// A shortcut field: keycap chips (click to type the keys instead) plus a
/// "Record" button that captures the next key press in this window.
fn hotkey_edit(ui: &mut Ui, rec: &mut Option<Id>, id: Id, text: &mut String, mode: RecMode) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
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
            let label = if mode == RecMode::Key {
                "Press a key…"
            } else {
                "Press a shortcut…"
            };
            let t = ui.input(|i| i.time);
            let pulse = 0.55 + 0.45 * ((t * 4.0).sin() as f32).abs();
            let mut job = egui::text::LayoutJob::default();
            job.append(
                icon::RECORD,
                0.0,
                egui::TextFormat::simple(theme::icon_font(13.0), p.error.gamma_multiply(pulse)),
            );
            job.append(
                label,
                6.0,
                egui::TextFormat::simple(theme::font(13.0), p.text),
            );
            job.append(
                "Esc cancels",
                10.0,
                egui::TextFormat::simple(theme::font(11.5), p.text2),
            );
            if ui
                .add(
                    egui::Button::new(job)
                        .fill(p.accent_soft)
                        .stroke(Stroke::new(1.5, p.accent)),
                )
                .clicked()
            {
                *rec = None;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(50));
            return;
        }
        let typing_id = id.with("typing");
        let typing: u8 = ui.data(|d| d.get_temp(typing_id)).unwrap_or(0);
        if typing > 0 {
            let r = ui.add(
                egui::TextEdit::singleline(text)
                    .id(typing_id.with("te"))
                    .desired_width(150.0)
                    .hint_text("Ctrl+Shift+V"),
            );
            if typing == 1 {
                r.request_focus();
                ui.data_mut(|d| d.insert_temp(typing_id, 2u8));
            } else if !r.has_focus() {
                ui.data_mut(|d| d.remove::<u8>(typing_id));
            }
        } else {
            let r = keycaps(ui, text)
                .on_hover_text("Click to type the keys")
                .on_hover_cursor(egui::CursorIcon::Text);
            if r.clicked() {
                ui.data_mut(|d| d.insert_temp(typing_id, 1u8));
            }
        }
        if icon_text_button(ui, icon::RECORD, "Record")
            .on_hover_text("Press the keys in this window")
            .clicked()
        {
            *rec = Some(id);
            ui.data_mut(|d| d.remove::<u8>(typing_id));
        }
    });
}

fn remap_table(ui: &mut Ui, rec: &mut Option<Id>, rows: &mut Vec<RemapRow>, shortcuts: bool) {
    let p = pal(ui);
    if rows.is_empty() {
        block(ui, |ui| hint(ui, "Nothing remapped yet."));
    }
    let mut remove = None;
    let salt = if shortcuts { "sc" } else { "key" };
    for (i, r) in rows.iter_mut().enumerate() {
        let id = Id::unique((salt, i));
        block(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                let from_mode = if shortcuts {
                    RecMode::Combo
                } else {
                    RecMode::Key
                };
                hotkey_edit(ui, rec, id.with("from"), &mut r.from, from_mode);
                ui.label(RichText::new("→").size(15.0).color(p.text3));
                egui::ComboBox::from_id_salt(id.with("kind"))
                    .selected_text(r.kind.label())
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for k in TargetKind::ALL {
                            ui.selectable_value(&mut r.kind, k, k.label());
                        }
                    });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icon::TRASH, "Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
            let indent = 0.0;
            let has_target = !matches!(r.kind, TargetKind::Disabled | TargetKind::PastePlainText);
            if has_target || shortcuts {
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    match r.kind {
                        TargetKind::Keys => hotkey_edit(ui, rec, id.with("to"), &mut r.value, RecMode::Combo),
                        TargetKind::Text => {
                            ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("Text to type").desired_width(220.0));
                        }
                        TargetKind::OpenUri => {
                            ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("https://… or a path").desired_width(220.0));
                        }
                        TargetKind::Run => {
                            ui.add(egui::TextEdit::singleline(&mut r.value).hint_text("/Applications/App.app").desired_width(200.0));
                            ui.add(egui::TextEdit::singleline(&mut r.args).hint_text("Arguments").desired_width(110.0));
                        }
                        TargetKind::Disabled | TargetKind::PastePlainText => {}
                    }
                    if shortcuts {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(egui::TextEdit::singleline(&mut r.app).hint_text("All apps").desired_width(130.0))
                                .on_hover_text("Only in this app: a bundle id or app name (macOS), or a process name like chrome.exe (Windows)");
                            ui.label(RichText::new("In").size(12.0).color(p.text2));
                        });
                    }
                });
            }
            if !r.is_blank() {
                let res = if shortcuts {
                    r.shortcut_remap().map(|_| ())
                } else {
                    r.key_remap().map(|_| ())
                };
                if let Err(e) = res {
                    error_label(ui, &e);
                }
            }
        });
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
        fancyzones::layout::find_custom(custom, &l.uuid)
            .map(|c| c.name.clone())
            .unwrap_or("Custom (missing)".into())
    } else {
        kinds.iter().find(|(k, _)| *k == l.kind).map(|(_, n)| n.to_string()).unwrap_or_default()
    };
    row(ui, "Layout", None, |ui| {
        egui::ComboBox::from_id_salt(("kind", salt))
            .selected_text(current)
            .width(170.0)
            .show_ui(ui, |ui| {
                for (k, name) in kinds {
                    if ui.selectable_label(l.kind == k, name).clicked() && l.kind != k {
                        let count = if l.kind == LayoutType::Custom || l.zone_count < 1 {
                            3
                        } else {
                            l.zone_count
                        };
                        let (show, spacing, radius) =
                            (l.show_spacing, l.spacing, l.sensitivity_radius);
                        *l = AppliedLayout::template(k, count).with_spacing(show, spacing);
                        l.sensitivity_radius = radius;
                    }
                }
                if !custom.is_empty() {
                    ui.separator();
                }
                for c in custom {
                    let on = l.kind == LayoutType::Custom
                        && fancyzones::layout::guid_eq(&l.uuid, &c.uuid);
                    if ui.selectable_label(on, &c.name).clicked() {
                        *l = AppliedLayout::custom(c);
                    }
                }
            });
    });
    let template = !matches!(l.kind, LayoutType::Blank | LayoutType::Custom);
    ui.add_enabled_ui(template, |ui| {
        row(ui, "Zones", None, |ui| {
            ui.add_sized(
                vec2(72.0, 24.0),
                egui::DragValue::new(&mut l.zone_count).range(1..=40),
            )
        });
    });
    row(ui, "Space around zones", None, |ui| {
        ui.add_enabled(
            l.show_spacing,
            egui::DragValue::new(&mut l.spacing)
                .range(fancyzones::layout::MAX_NEGATIVE_SPACING..=100)
                .suffix(" px"),
        );
        toggle_labeled(ui, &mut l.show_spacing, "Space around zones");
    });
    row(ui, "Highlight distance", None, |ui| {
        ui.add_sized(
            vec2(72.0, 24.0),
            egui::DragValue::new(&mut l.sensitivity_radius)
                .range(0..=200)
                .suffix(" px"),
        )
    });
}

/// The zones of `l` on `work_area`, scaled into a small picture in the
/// configured zone colors.
fn zones_preview(
    ui: &mut Ui,
    l: &AppliedLayout,
    work_area: fancyzones::Rect,
    custom: &[CustomLayout],
    s: &fancyzones::Settings,
) {
    let (w, h) = (
        work_area.width().max(1) as f32,
        work_area.height().max(1) as f32,
    );
    let pw = ui.available_width().clamp(160.0, 380.0);
    let ph = (pw * h / w).clamp(60.0, 250.0);
    let (rect, _) = ui.allocate_exact_size(vec2(pw, ph), Sense::hover());
    let pal = pal(ui);
    let p = ui.painter_at(rect);
    zone_style::paint_wallpaper(&p, rect);
    if let Ok(layout) = fancyzones::Layout::new(l, work_area, custom) {
        let zones: Vec<Rect> = layout
            .zones
            .iter()
            .map(|z| {
                Rect::from_min_max(
                    pos2(
                        (z.left - work_area.left) as f32 / w,
                        (z.top - work_area.top) as f32 / h,
                    ),
                    pos2(
                        (z.right - work_area.left) as f32 / w,
                        (z.bottom - work_area.top) as f32 / h,
                    ),
                )
            })
            .collect();
        let colors = zone_style::effective(s, None);
        zone_style::paint_zones(
            &p,
            rect.shrink(4.0),
            &zones,
            &colors,
            None,
            s.show_zone_number,
            (pw / w * 2.5).min(1.0),
        );
        if layout.zones.is_empty() {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No zones",
                theme::font(13.0),
                Color32::from_white_alpha(220),
            );
        }
    }
    // Round the corners off.
    ui.painter().rect_stroke(
        rect.expand(3.0),
        11.0,
        Stroke::new(6.0, pal.card),
        StrokeKind::Inside,
    );
    ui.painter().rect_stroke(
        rect,
        8.0,
        Stroke::new(1.0, pal.card_border),
        StrokeKind::Inside,
    );
}

/// A FancyZones hotkey field (keycaps + Record). Invalid text keeps the
/// last valid value; returns why it's invalid.
fn fz_hotkey_edit(
    ui: &mut Ui,
    rec: &mut Option<Id>,
    bufs: &mut std::collections::BTreeMap<&'static str, (String, fancyzones::Hotkey)>,
    key: &'static str,
    hk: &mut fancyzones::Hotkey,
) -> Option<String> {
    let entry = bufs
        .entry(key)
        .or_insert_with(|| (format_fz_hotkey(hk), *hk));
    if entry.1 != *hk {
        *entry = (format_fz_hotkey(hk), *hk); // changed elsewhere (file reload)
    }
    hotkey_edit(
        ui,
        rec,
        Id::unique(("fz-hk", key)),
        &mut entry.0,
        RecMode::Combo,
    );
    match parse_fz_hotkey(&entry.0) {
        Ok(h) => {
            *hk = h;
            entry.1 = h;
            None
        }
        Err(e) => Some(e),
    }
}

fn hk_text(h: &fancyzones::Hotkey) -> String {
    if h.code == 0 {
        String::new()
    } else {
        format_fz_hotkey(h)
    }
}

fn fz_mods_text(m: &fancyzones::Modifiers) -> String {
    let names = mod_names();
    let mut v = Vec::new();
    for (on, i) in [(m.ctrl, 0), (m.alt, 1), (m.shift, 2), (m.meta, 3)] {
        if on {
            v.push(names[i].1);
        }
    }
    v.join("+")
}

/// Quick layout switch modifiers (fixed, like PowerToys' Win+Ctrl+Alt).
fn quick_switch_mods() -> String {
    fz_mods_text(&fancyzones::Modifiers::new(true, true, false, true))
}

fn add_excluded(list: &mut Vec<String>, app: &str) {
    if !list.iter().any(|a| a.eq_ignore_ascii_case(app)) {
        list.push(app.to_string());
    }
}

/// Modifier chips that toggle on and off.
fn modifiers_edit(
    ui: &mut Ui,
    m: &mut fancyzones::Modifiers,
    names: &[(keyboard_manager::keys::ModFamily, &str); 4],
) {
    use keyboard_manager::keys::ModFamily as F;
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        for (f, n) in names {
            let v = match f {
                F::Ctrl => &mut m.ctrl,
                F::Alt => &mut m.alt,
                F::Shift => &mut m.shift,
                F::Win => &mut m.meta,
            };
            let text = RichText::new(*n)
                .size(12.5)
                .color(if *v { p.on_accent } else { p.text });
            let b = egui::Button::new(text)
                .corner_radius(13.0)
                .min_size(vec2(0.0, 26.0))
                .fill(if *v { p.accent } else { p.control })
                .stroke(if *v {
                    Stroke::NONE
                } else {
                    Stroke::new(1.0, p.control_border)
                });
            if ui.add(b).clicked() {
                *v = !*v;
            }
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
        fz.settings.window_switching = true;
        fz.settings.monitor_rotation = true;
        fz.settings.excluded_apps = vec!["com.apple.Safari".into(), "notepad.exe".into()];
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
        app.confirm_new_key = false;
        // Searching renders every page and finds settings on other pages.
        app.search = "opacity".into();
        frame(&mut app);
        let mut hits = Vec::new();
        ctx.run_ui(input(), |ui| hits = app.search_hits(ui, "opacity"))
            .textures_delta
            .clear();
        assert!(
            hits.iter()
                .any(|h| h.page == Page::FancyZones && h.section == "Zone appearance"),
            "{hits:?}"
        );
        assert!(hits.iter().any(|h| h.page == Page::MouseUtils), "{hits:?}");
        app.search.clear();
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
        assert_eq!(fz_back.settings.excluded_apps.len(), 2);
        assert_eq!(fz_back.settings.editor_hotkey, fancyzones::Hotkey::default_editor());

        // Typing a hotkey updates the setting; a bad one is not applied.
        app.fz_hk_text.get_mut("rotation").unwrap().0 = "Ctrl+Shift+R".into();
        app.page = Page::FancyZones;
        frame(&mut app);
        assert_eq!(format_fz_hotkey(&app.fz.settings.monitor_rotation_hotkey), parse_fz_hotkey("Ctrl+Shift+R").map(|h| format_fz_hotkey(&h)).unwrap());
        app.fz_hk_text.get_mut("rotation").unwrap().0 = "Ctrl+Bogus".into();
        frame(&mut app);
        assert_eq!(app.fz.settings.monitor_rotation_hotkey.code, 0x52);
        app.flush();
        assert_eq!(load_fancyzones(&fancyzones_path(&path)).unwrap().settings.monitor_rotation_hotkey.code, 0x52);
        assert!(fz_back.applied_layouts.contains_key("gone"));

        // A preset changes the zone colors and is saved.
        zone_style::PRESETS[1].apply(&mut app.fz.settings);
        frame(&mut app);
        app.flush();
        let s = load_fancyzones(&fancyzones_path(&path)).unwrap().settings;
        assert!(zone_style::PRESETS[1].matches(&s));

        // "Show on screen" writes a flash request next to the config.
        request_zone_flash(&path, None).unwrap();
        assert!(dir.join(fancyzones::FLASH_REQUEST_FILE).exists());

        // An invalid edit blocks saving and is reported on its page.
        app.app.cfg.mwb.settings.key = "short".into();
        frame(&mut app);
        app.flush();
        assert!(app.problems().any(|p| p.page == Page::Mwb));
        assert_ne!(AppConfig::load_or_default(&path).unwrap().mwb.settings.key, "short");
        std::fs::remove_dir_all(&dir).ok();
    }
}
