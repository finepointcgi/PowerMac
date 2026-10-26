//! The PowerRename window: options on the left, the preview list (original
//! → new name, changed parts highlighted) on the right, Apply / Undo below.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use pm_ui::theme::{self, icon as picon, pal, Palette};
use pm_ui::widgets::{self, block, card, hint, row, segmented, toggle_row};

use crate::config::Config;
use crate::rename::{
    self, diff, ApplyReport, ApplyTo, Journal, Plan, RenameItem, RenameOptions, Status, TextCase,
    TimeSource,
};
use crate::ui_common::{file_icon, icon, pretty_path, tint};

const OPTIONS_W: f32 = 352.0;
const ROW_H: f32 = 30.0;

enum JobKind {
    Apply,
    Undo,
}

struct Job {
    kind: JobKind,
    progress: Arc<AtomicUsize>,
    total: usize,
    rx: mpsc::Receiver<anyhow::Result<ApplyReport>>,
}

enum Outcome {
    Ok(String),
    Errors(String, Vec<(PathBuf, String)>),
}

/// The window's state.
pub struct RenameApp {
    items: Vec<RenameItem>,
    /// The paths the window was opened with (followed through renames).
    roots: Vec<PathBuf>,
    opts: RenameOptions,
    plan: Plan,
    planned_opts: Option<RenameOptions>,
    cfg: Config,
    cfg_path: Option<PathBuf>,
    journal_path: Option<PathBuf>,
    has_undo: bool,
    only_changed: bool,
    show_help: bool,
    job: Option<Job>,
    outcome: Option<Outcome>,
    focus_search: bool,
}

impl RenameApp {
    /// A window for `paths`, with settings in `cfg_path` and the undo journal
    /// in `journal_path` (`None`: not persisted).
    pub fn new(
        paths: Vec<PathBuf>,
        cfg_path: Option<PathBuf>,
        journal_path: Option<PathBuf>,
    ) -> Self {
        let cfg = cfg_path
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_default();
        let opts = if cfg.rename.remember_options {
            cfg.rename.options.clone()
        } else {
            RenameOptions::default()
        };
        let has_undo = journal_path.as_deref().and_then(Journal::load).is_some();
        let roots: Vec<PathBuf> = paths
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect();
        let mut s = Self {
            items: rename::collect_items(&roots),
            roots,
            only_changed: cfg.rename.show_only_changed,
            opts,
            plan: Plan::default(),
            planned_opts: None,
            cfg,
            cfg_path,
            journal_path,
            has_undo,
            show_help: false,
            job: None,
            outcome: None,
            focus_search: true,
        };
        s.replan();
        s
    }

    fn replan(&mut self) {
        self.plan = rename::plan(&self.items, &self.opts);
        self.planned_opts = Some(self.opts.clone());
    }

    fn save_cfg(&mut self) {
        self.cfg.rename.options = self.opts.clone();
        self.cfg.rename.show_only_changed = self.only_changed;
        if let Some(p) = &self.cfg_path {
            if let Err(e) = self.cfg.save_to(p) {
                log::warn!("{}: {e}", p.display());
            }
        }
    }

    fn start_apply(&mut self, ctx: &egui::Context) {
        if self.plan.rename_count == 0 || self.job.is_some() {
            return;
        }
        let n = self.cfg.rename.mru_size;
        Config::push_mru(&mut self.cfg.rename.search_mru, &self.opts.search, n);
        Config::push_mru(&mut self.cfg.rename.replace_mru, &self.opts.replace, n);
        self.save_cfg();
        let items = self.items.clone();
        let plan = self.plan.clone();
        let journal = self.journal_path.clone();
        let progress = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        let p2 = progress.clone();
        let ctx = ctx.clone();
        let total = plan.rename_count;
        std::thread::spawn(move || {
            let r = rename::apply(&items, &plan, journal.as_deref(), &mut |d| {
                p2.store(d, Ordering::Relaxed);
                ctx.request_repaint();
            });
            let _ = tx.send(Ok(r));
            ctx.request_repaint();
        });
        self.job = Some(Job {
            kind: JobKind::Apply,
            progress,
            total,
            rx,
        });
    }

    fn start_undo(&mut self, ctx: &egui::Context) {
        let Some(j) = self.journal_path.clone() else {
            return;
        };
        if self.job.is_some() {
            return;
        }
        let total = Journal::load(&j).map(|j| j.len()).unwrap_or(0);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(rename::undo_last(&j));
            ctx.request_repaint();
        });
        self.job = Some(Job {
            kind: JobKind::Undo,
            progress: Arc::new(AtomicUsize::new(0)),
            total,
            rx,
        });
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        let Ok(r) = job.rx.try_recv() else { return };
        let undo = matches!(job.kind, JobKind::Undo);
        self.job = None;
        match r {
            Ok(rep) => {
                // Follow the selected paths through what was renamed.
                for op in rep.journal.levels.iter().flatten() {
                    for r in &mut self.roots {
                        if *r == op.from {
                            *r = op.to.clone();
                        } else if let Ok(rest) = r.strip_prefix(&op.from) {
                            *r = op.to.join(rest);
                        }
                    }
                }
                let n = rep.renamed;
                let what = if undo { "Restored" } else { "Renamed" };
                let msg = format!("{what} {n} item{}", if n == 1 { "" } else { "s" });
                self.outcome = Some(if rep.errors.is_empty() {
                    Outcome::Ok(msg)
                } else {
                    Outcome::Errors(format!("{msg}; {} failed", rep.errors.len()), rep.errors)
                });
            }
            Err(e) => self.outcome = Some(Outcome::Errors(e.to_string(), Vec::new())),
        }
        self.has_undo = self
            .journal_path
            .as_deref()
            .and_then(Journal::load)
            .is_some();
        self.items = rename::collect_items(&self.roots);
        self.replan();
    }

    /// One frame of the window.
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        self.poll_job();
        let p = pal(ui);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_cfg();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.job.is_none() {
            self.save_cfg();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            self.start_apply(&ctx);
        }

        egui::Panel::bottom("footer")
            .exact_size(56.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .stroke(Stroke::new(1.0, p.card_border))
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| self.footer(ui, &p));
        egui::Panel::left("options")
            .exact_size(OPTIONS_W)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .stroke(Stroke::new(1.0, p.card_border)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("opts-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin {
                                left: 16,
                                right: 16,
                                top: 4,
                                bottom: 20,
                            })
                            .show(ui, |ui| {
                                ui.add_enabled_ui(self.job.is_none(), |ui| self.options(ui));
                            });
                    });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| self.list(ui, &p));

        if self.planned_opts.as_ref() != Some(&self.opts) {
            self.outcome = None;
            self.replan();
        }
    }

    // -----------------------------------------------------------------
    // Options

    fn text_field(
        ui: &mut Ui,
        id: &str,
        text: &mut String,
        hint_text: &str,
        mru: &[String],
        focus: bool,
    ) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let w = ui.available_width() - 36.0;
            let r = ui.add_sized(
                vec2(w, 30.0),
                egui::TextEdit::singleline(text)
                    .id_salt(id)
                    .hint_text(RichText::new(hint_text).color(p.text3))
                    .margin(vec2(8.0, 6.0)),
            );
            if focus {
                r.request_focus();
            }
            focus_ring(ui, &r, &p);
            let b = ui
                .add_enabled_ui(!mru.is_empty(), |ui| {
                    ui.spacing_mut().button_padding = vec2(4.0, 4.0);
                    ui.add_sized(
                        vec2(30.0, 30.0),
                        egui::Button::new(theme::icon_text(picon::CLOCK).size(15.0).color(p.text2)),
                    )
                })
                .inner;
            let b = b.on_hover_text("Recently used");
            egui::Popup::menu(&b).width(w).show(|ui| {
                for t in mru {
                    if ui
                        .add(
                            egui::Button::new(t.as_str())
                                .frame(false)
                                .min_size(vec2(w - 8.0, 24.0)),
                        )
                        .clicked()
                    {
                        *text = t.clone();
                        ui.close();
                    }
                }
            });
        });
    }

    fn options(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let focus = std::mem::take(&mut self.focus_search);
        let search_mru = self.cfg.rename.search_mru.clone();
        let replace_mru = self.cfg.rename.replace_mru.clone();
        let error = self.plan.error.clone();
        let o = &mut self.opts;

        card(ui, "Search for", |ui| {
            block(ui, |ui| {
                Self::text_field(
                    ui,
                    "search",
                    &mut o.search,
                    if o.use_regex {
                        "Regular expression"
                    } else {
                        "Text to find"
                    },
                    &search_mru,
                    focus,
                );
                if let Some(e) = &error {
                    let first = e.lines().next().unwrap_or(e);
                    widgets::error_label(ui, &format!("Invalid expression: {first}"));
                }
            });
            toggle_row(ui, "Use regular expressions", None, &mut o.use_regex);
            toggle_row(ui, "Match all occurrences", None, &mut o.match_all);
            toggle_row(ui, "Case sensitive", None, &mut o.case_sensitive);
        });

        card_with_help(ui, "Replace with", &mut self.show_help, |ui| {
            block(ui, |ui| {
                Self::text_field(
                    ui,
                    "replace",
                    &mut o.replace,
                    "Replace with",
                    &replace_mru,
                    false,
                );
            });
            toggle_row(
                ui,
                "Enumerate items",
                Some("${} counts 0, 1, 2… in the replacement"),
                &mut o.enumerate,
            );
            toggle_row(
                ui,
                "Random values",
                Some("${rstringalnum=9}, ${ruuidv4} …"),
                &mut o.randomize,
            );
        });
        if self.show_help {
            cheat_sheet(ui);
        }

        card(ui, "Apply to", |ui| {
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    segmented(
                        ui,
                        &mut o.apply_to,
                        &[
                            (ApplyTo::NameAndExtension, "Name + extension"),
                            (ApplyTo::NameOnly, "Name"),
                            (ApplyTo::ExtensionOnly, "Extension"),
                        ],
                    );
                });
            });
            row(ui, "Text formatting", None, |ui| {
                small_combo(
                    ui,
                    "case",
                    &mut o.text_case,
                    &[
                        (TextCase::None, "None"),
                        (TextCase::Upper, "UPPERCASE"),
                        (TextCase::Lower, "lowercase"),
                        (TextCase::Title, "Title Case"),
                        (TextCase::Capitalize, "Capitalize Each Word"),
                    ],
                )
            });
            row(ui, "Date source", None, |ui| {
                small_combo(
                    ui,
                    "time",
                    &mut o.time_source,
                    &[
                        (TimeSource::Created, "Date created"),
                        (TimeSource::Modified, "Date modified"),
                        (TimeSource::Accessed, "Date accessed"),
                    ],
                )
            });
        });

        card(ui, "Include", |ui| {
            let mut files = !o.exclude_files;
            let mut folders = !o.exclude_folders;
            let mut sub = !o.exclude_subfolder_items;
            toggle_row(ui, "Files", None, &mut files);
            toggle_row(ui, "Folders", None, &mut folders);
            toggle_row(
                ui,
                "Subfolder items",
                Some("The contents of selected folders"),
                &mut sub,
            );
            o.exclude_files = !files;
            o.exclude_folders = !folders;
            o.exclude_subfolder_items = !sub;
        });
        ui.add_space(4.0);
        let _ = p;
    }

    // -----------------------------------------------------------------
    // List

    fn visible(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| {
                !self.only_changed || {
                    let s = self.plan.items[i].status;
                    s == Status::Rename || s.is_error()
                }
            })
            .collect()
    }

    fn list(&mut self, ui: &mut Ui, p: &Palette) {
        // Title row.
        ui.horizontal(|ui| {
            widgets::icon_tile(ui, picon::PENCIL_SIMPLE, tint::PURPLE, 30.0);
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("PowerRename")
                        .font(theme::font_semibold(17.0))
                        .color(p.text),
                );
                let files = self.items.iter().filter(|i| !i.is_dir).count();
                let dirs = self.items.len() - files;
                let mut s = format!("{files} file{}", if files == 1 { "" } else { "s" });
                if dirs > 0 {
                    s += &format!(", {dirs} folder{}", if dirs == 1 { "" } else { "s" });
                }
                ui.label(RichText::new(s).size(12.0).color(p.text2));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut f = self.only_changed;
                segmented(ui, &mut f, &[(false, "All items"), (true, "Renamed only")]);
                if f != self.only_changed {
                    self.only_changed = f;
                }
            });
        });
        ui.add_space(12.0);

        let frame = widgets::card_frame(p).inner_margin(egui::Margin::same(0));
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let full = ui.available_rect_before_wrap();
            let w = full.width();
            let col = Columns::new(full.left(), w);
            // Header.
            let (hr, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
            let painter = ui.painter().clone();
            let selectable: Vec<usize> = (0..self.items.len()).collect();
            let all = selectable.iter().all(|&i| self.items[i].selected);
            let none = selectable.iter().all(|&i| !self.items[i].selected);
            let cb = Rect::from_center_size(pos2(col.check + 9.0, hr.center().y), vec2(16.0, 16.0));
            let cbr = ui.interact(cb.expand(4.0), ui.scope_id().with("all"), Sense::click());
            paint_check(
                &painter,
                cb,
                if all {
                    Some(true)
                } else if none {
                    Some(false)
                } else {
                    None
                },
                cbr.hovered(),
                p,
            );
            if cbr.clicked() && self.job.is_none() {
                let v = !all;
                for it in &mut self.items {
                    it.selected = v;
                }
                self.replan();
            }
            let hf = theme::font_semibold(12.0);
            painter.text(
                pos2(col.name, hr.center().y),
                Align2::LEFT_CENTER,
                "Original",
                hf.clone(),
                p.text2,
            );
            painter.text(
                pos2(col.new_name, hr.center().y),
                Align2::LEFT_CENTER,
                "Renamed",
                hf,
                p.text2,
            );
            painter.hline(
                hr.x_range(),
                hr.bottom() - 0.5,
                Stroke::new(1.0, p.separator),
            );

            let vis = self.visible();
            if vis.is_empty() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(theme::icon_text(icon::FUNNEL).size(28.0).color(p.text3));
                    ui.label(
                        RichText::new(if self.items.is_empty() {
                            "No files"
                        } else {
                            "Nothing to rename with these options"
                        })
                        .color(p.text2),
                    );
                });
                return;
            }
            let mut toggled = None;
            egui::ScrollArea::vertical()
                .id_salt("list")
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, vis.len(), |ui, range| {
                    for vi in range {
                        let i = vis[vi];
                        if self.row(ui, i, vi, &col, p) {
                            toggled = Some(i);
                        }
                    }
                });
            if let Some(i) = toggled {
                if self.job.is_none() {
                    self.items[i].selected = !self.items[i].selected;
                    self.replan();
                }
            }
        });
    }

    /// One list row; returns whether its checkbox was clicked.
    fn row(&self, ui: &mut Ui, i: usize, vi: usize, col: &Columns, p: &Palette) -> bool {
        let it = &self.items[i];
        let pl = &self.plan.items[i];
        let (r, resp) = ui.allocate_exact_size(vec2(col.width, ROW_H), Sense::hover());
        let painter = ui.painter_at(r);
        if vi % 2 == 1 {
            painter.rect_filled(
                r,
                0.0,
                if p.dark {
                    Color32::from_white_alpha(6)
                } else {
                    Color32::from_black_alpha(6)
                },
            );
        }
        if resp.hovered() {
            painter.rect_filled(r, 0.0, theme::mix(p.card, p.accent_soft, 0.6));
        }
        let cy = r.center().y;
        let cb = Rect::from_center_size(pos2(col.check + 9.0, cy), vec2(16.0, 16.0));
        let cbr = ui.interact(
            cb.expand(5.0),
            ui.scope_id().with(("cb", i)),
            Sense::click(),
        );
        paint_check(&painter, cb, Some(it.selected), cbr.hovered(), p);

        let indent = (it.depth as f32 * 16.0).min(160.0);
        let ix = col.name + indent;
        let (ext, dir) = (
            rename::split_ext(&it.name)
                .1
                .trim_start_matches('.')
                .to_lowercase(),
            it.is_dir,
        );
        let (glyph, t) = file_icon(&ext, dir);
        let dim = !it.selected || pl.status == Status::Skipped;
        let ic = if dim {
            p.text3
        } else if dir {
            t
        } else {
            theme::mix(t, p.text2, 0.35)
        };
        painter.text(
            pos2(ix + 8.0, cy),
            Align2::CENTER_CENTER,
            glyph,
            theme::icon_font(16.0),
            ic,
        );
        let name_x = ix + 24.0;
        let font = theme::font(13.0);
        let max_old = col.arrow - 10.0 - name_x;
        let max_new = col.width + col.left - 34.0 - col.new_name;

        let base = if dim { p.text3 } else { p.text };
        match (&pl.new_name, pl.status) {
            (Some(new), st) => {
                let (old_spans, new_spans) = diff::diff(&rename::nfc(&it.name), new);
                let removed_bg = theme::mix(p.card, p.error, if p.dark { 0.28 } else { 0.16 });
                let added_bg = if st.is_error() {
                    theme::mix(p.card, p.error, if p.dark { 0.28 } else { 0.16 })
                } else {
                    theme::mix(p.card, p.ok, if p.dark { 0.30 } else { 0.18 })
                };
                let g = spans_galley(ui, &old_spans, &font, base, removed_bg, max_old);
                painter.galley(pos2(name_x, cy - g.size().y / 2.0), g, base);
                painter.text(
                    pos2(col.arrow + 6.0, cy),
                    Align2::CENTER_CENTER,
                    icon::ARROW_RIGHT,
                    theme::icon_font(13.0),
                    p.text3,
                );
                let fg = if st.is_error() { p.error } else { p.text };
                let g = spans_galley(ui, &new_spans, &font, fg, added_bg, max_new);
                painter.galley(pos2(col.new_name, cy - g.size().y / 2.0), g, fg);
                if st.is_error() {
                    let ir = Rect::from_center_size(
                        pos2(col.left + col.width - 18.0, cy),
                        vec2(18.0, 18.0),
                    );
                    painter.text(
                        ir.center(),
                        Align2::CENTER_CENTER,
                        picon::WARNING_CIRCLE,
                        theme::icon_font(15.0),
                        p.error,
                    );
                    ui.interact(ir, ui.scope_id().with(("err", i)), Sense::hover())
                        .on_hover_text(st.message());
                }
            }
            (None, _) => {
                let g = truncate_galley(ui, &it.name, &font, base, max_old);
                painter.galley(pos2(name_x, cy - g.size().y / 2.0), g, base);
            }
        }
        if resp.hovered() {
            resp.on_hover_text_at_pointer(pretty_path(&it.path));
        }
        cbr.clicked()
    }

    // -----------------------------------------------------------------
    // Footer

    fn footer(&mut self, ui: &mut Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            if let Some(job) = &self.job {
                let done = job.progress.load(Ordering::Relaxed);
                let frac = if job.total == 0 {
                    0.0
                } else {
                    done as f32 / job.total as f32
                };
                let what = match job.kind {
                    JobKind::Apply => format!("Renaming {done} of {}…", job.total),
                    JobKind::Undo => "Undoing the last rename…".to_string(),
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(280.0)
                        .desired_height(8.0)
                        .fill(p.accent)
                        .corner_radius(4.0),
                );
                ui.label(RichText::new(what).size(12.5).color(p.text2));
            } else {
                match &self.outcome {
                    Some(Outcome::Ok(m)) => widgets::ok_label(ui, m),
                    Some(Outcome::Errors(m, errs)) => {
                        let r = ui.add(egui::Label::new(error_job(ui, m, p)).sense(Sense::hover()));
                        if !errs.is_empty() {
                            r.on_hover_ui(|ui| {
                                ui.set_max_width(520.0);
                                for (path, e) in errs.iter().take(12) {
                                    ui.label(RichText::new(pretty_path(path)).strong());
                                    ui.label(RichText::new(e).color(p.error).size(12.0));
                                }
                                if errs.len() > 12 {
                                    ui.label(format!("… and {} more", errs.len() - 12));
                                }
                            });
                        }
                    }
                    None => {
                        let n = self.plan.rename_count;
                        let total = self.items.iter().filter(|i| i.selected).count();
                        ui.label(
                            RichText::new(format!(
                                "{n} of {total} item{} will be renamed",
                                if total == 1 { "" } else { "s" }
                            ))
                            .size(12.5)
                            .color(p.text2),
                        );
                        if self.plan.error_count > 0 {
                            let e = self.plan.error_count;
                            widgets::warn_label(
                                ui,
                                &format!("{e} conflict{} — skipped", if e == 1 { "" } else { "s" }),
                            );
                        }
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self.job.is_some();
                let apply = ui.add_enabled_ui(!busy && self.plan.rename_count > 0, |ui| {
                    widgets::primary_icon_button(ui, picon::CHECK, "Apply")
                });
                if apply.inner.on_hover_text("Rename (⌘↩)").clicked() {
                    self.start_apply(&ctx);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Close").min_size(vec2(72.0, 28.0)))
                    .clicked()
                {
                    self.save_cfg();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if self.has_undo {
                    ui.add_space(6.0);
                    let r = ui.add_enabled_ui(!busy, |ui| {
                        widgets::icon_text_button(
                            ui,
                            picon::ARROW_COUNTER_CLOCKWISE,
                            "Undo last rename",
                        )
                    });
                    if r.inner.clicked() {
                        self.start_undo(&ctx);
                    }
                }
            });
        });
    }
}

impl eframe::App for RenameApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
impl RenameApp {
    pub(crate) fn set_options(&mut self, f: impl FnOnce(&mut RenameOptions)) {
        f(&mut self.opts);
        self.replan();
    }
    pub(crate) fn set_only_changed(&mut self, v: bool) {
        self.only_changed = v;
    }
    pub(crate) fn set_help(&mut self, v: bool) {
        self.show_help = v;
    }
    pub(crate) fn deselect(&mut self, i: usize) {
        self.items[i].selected = false;
        self.replan();
    }
    pub(crate) fn set_outcome_errors(&mut self) {
        self.outcome = Some(Outcome::Errors(
            "Renamed 3 items; 1 failed".into(),
            vec![(PathBuf::from("/x"), "denied".into())],
        ));
    }
    pub(crate) fn set_progress(&mut self, done: usize, total: usize) {
        let (_tx, rx) = mpsc::channel();
        std::mem::forget(_tx);
        self.job = Some(Job {
            kind: JobKind::Apply,
            progress: Arc::new(AtomicUsize::new(done)),
            total,
            rx,
        });
    }
    pub(crate) fn apply_now(&mut self, ctx: &egui::Context) {
        self.start_apply(ctx);
        if let Some(j) = &self.job {
            // Wait for the worker.
            let r =
                j.rx.recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
            let (tx, rx) = mpsc::channel();
            tx.send(r).unwrap();
            self.job.as_mut().unwrap().rx = rx;
        }
        self.poll_job();
    }
    pub(crate) fn plan_for_test(&self) -> &Plan {
        &self.plan
    }
}

/// x positions of the list columns.
struct Columns {
    left: f32,
    width: f32,
    check: f32,
    name: f32,
    arrow: f32,
    new_name: f32,
}

impl Columns {
    fn new(left: f32, width: f32) -> Self {
        let check = left + 14.0;
        let name = check + 30.0;
        let arrow = left + 44.0 + (width - 44.0) * 0.5 - 12.0;
        Self {
            left,
            width,
            check,
            name,
            arrow,
            new_name: arrow + 22.0,
        }
    }
}

/// An accent outline around a focused text field (the theme's focus stroke
/// is the selection text color).
pub(crate) fn focus_ring(ui: &Ui, r: &egui::Response, p: &Palette) {
    if r.has_focus() {
        ui.painter()
            .rect_stroke(r.rect, 7.0, Stroke::new(1.5, p.accent), StrokeKind::Inside);
    }
}

/// A checkbox: `Some(on)` or `None` for mixed.
fn paint_check(painter: &egui::Painter, r: Rect, state: Option<bool>, hover: bool, p: &Palette) {
    match state {
        Some(true) | None => {
            let fill = if hover {
                theme::mix(p.accent, Color32::BLACK, 0.1)
            } else {
                p.accent
            };
            painter.rect_filled(r, 4.0, fill);
            if state == Some(true) {
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    picon::CHECK,
                    theme::icon_font(12.0),
                    p.on_accent,
                );
            } else {
                painter.hline(
                    (r.left() + 4.0)..=(r.right() - 4.0),
                    r.center().y,
                    Stroke::new(2.0, p.on_accent),
                );
            }
        }
        Some(false) => {
            painter.rect(
                r,
                4.0,
                p.input,
                Stroke::new(1.0, if hover { p.accent } else { p.control_border }),
                StrokeKind::Inside,
            );
        }
    }
}

/// A galley of `spans` with changed runs on `changed_bg`, cut to `max_w`.
fn spans_galley(
    ui: &Ui,
    spans: &[diff::Span],
    font: &FontId,
    color: Color32,
    changed_bg: Color32,
    max_w: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    for s in spans {
        let mut f = egui::TextFormat::simple(font.clone(), color);
        if s.changed {
            f.background = changed_bg;
        }
        job.append(&s.text, 0.0, f);
    }
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(20.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts_mut(|f| f.layout_job(job))
}

fn truncate_galley(
    ui: &Ui,
    text: &str,
    font: &FontId,
    color: Color32,
    max_w: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(20.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts_mut(|f| f.layout_job(job))
}

fn error_job(ui: &Ui, text: &str, p: &Palette) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        picon::WARNING_CIRCLE,
        0.0,
        egui::TextFormat::simple(theme::icon_font(13.0), p.error),
    );
    job.append(
        text,
        5.0,
        egui::TextFormat::simple(theme::font(12.5), p.error),
    );
    let _ = ui;
    job
}

fn small_combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    items: &[(T, &str)],
) -> egui::Response {
    let current = items
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, n)| *n)
        .unwrap_or("?");
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(150.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(150.0);
            for (v, n) in items {
                ui.selectable_value(value, *v, *n);
            }
        })
        .response
}

/// A card whose title row has a "?" toggle on the right.
fn card_with_help(ui: &mut Ui, title: &str, help: &mut bool, add: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    let top = ui.cursor().top();
    card(ui, title, add);
    // The title sits 18 pt below the top; put the button at its right end.
    let r = Rect::from_min_size(
        pos2(ui.max_rect().right() - 24.0, top + 14.0),
        vec2(24.0, 24.0),
    );
    let resp = ui.interact(r, ui.scope_id().with(("help", title)), Sense::click());
    let c = if *help || resp.hovered() {
        p.accent
    } else {
        p.text2
    };
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        picon::INFO,
        theme::icon_font(15.0),
        c,
    );
    if resp.on_hover_text("Replacement cheat sheet").clicked() {
        *help = !*help;
    }
}

fn cheat_sheet(ui: &mut Ui) {
    let p = pal(ui);
    let rows: &[(&str, &str)] = &[
        ("$1 $2 …", "Capture group (regex)"),
        ("$0  $&", "The whole match"),
        ("$<name>", "Named group"),
        ("$$", "A literal $"),
        ("${}", "Counter from 0"),
        ("${start=1,padding=3}", "Counter: 001, 002…"),
        ("${increment=5}", "Counter step"),
        ("${rstringalnum=9}", "Random letters + digits"),
        ("${rstringalpha=6}", "Random letters"),
        ("${rstringdigit=4}", "Random digits"),
        ("${ruuidv4}", "Random UUID"),
        ("$YYYY $YY $MM $MMM $MMMM", "Year, month"),
        ("$DD $DDD $DDDD", "Day, weekday"),
        ("$hh $mm $ss $fff", "Hour, min, sec, ms"),
        ("$HH $TT", "Hour (12 h), AM/PM"),
    ];
    card(ui, "", |ui| {
        ui.add_space(8.0);
        for (k, v) in rows {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.allocate_ui_with_layout(
                    vec2(146.0, 16.0),
                    Layout::left_to_right(Align::Min),
                    |ui| {
                        ui.set_width(146.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(*k)
                                    .font(FontId::monospace(11.5))
                                    .color(p.accent),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.add(egui::Label::new(RichText::new(*v).size(11.5).color(p.text2)).wrap());
            });
            ui.add_space(-3.0);
        }
        ui.add_space(8.0);
        hint(
            ui,
            "Date variables use the file's date (see “Date source”).",
        );
        ui.add_space(6.0);
    });
}

/// Open the PowerRename window for `paths` and block until it is closed.
/// Settings are in `<config dir>/PowerMac/file-tools.json`, the undo journal
/// in `<config dir>/PowerMac/rename-undo.json`. Must run on the main thread.
pub fn run_rename_window(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    run_rename_window_with(paths, Config::default_path(), Journal::default_path())
}

/// [`run_rename_window`] with explicit settings / journal files.
pub fn run_rename_window_with(
    paths: Vec<PathBuf>,
    cfg_path: Option<PathBuf>,
    journal_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PowerRename")
            .with_app_id("com.powermac.powerrename")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([820.0, 520.0])
            .with_icon(crate::ui_common::app_icon([0xAF, 0x52, 0xDE], false))
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "PowerRename",
        options,
        Box::new(move |_cc| {
            crate::ui_common::become_foreground_app();
            Ok(Box::new(RenameApp::new(paths, cfg_path, journal_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("PowerRename window: {e}"))
}
