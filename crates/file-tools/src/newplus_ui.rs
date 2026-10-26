//! The New+ picker: the templates with icons, a name field (prefilled, the
//! name part selected like Finder's rename) and Create. Enter creates, the
//! arrow keys move through the list, Escape closes.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};
use pm_ui::theme::{self, icon as picon, pal, Palette};
use pm_ui::widgets;

use crate::config::Config;
use crate::newplus::{self, NewOptions, Template};
use crate::rename::split_ext;
use crate::ui_common::{file_icon, icon, pretty_path, tint};

const ROW_H: f32 = 48.0;

/// The picker's state.
pub struct NewApp {
    target: PathBuf,
    templates_dir: PathBuf,
    templates: Vec<Template>,
    /// Item count of folder templates (by index).
    counts: Vec<usize>,
    selected: usize,
    name: String,
    name_for: Option<usize>,
    select_stem: bool,
    cfg: Config,
    error: Option<String>,
    created: Option<PathBuf>,
    /// Reveal / open the new item and close (off in tests).
    live: bool,
    scroll_to_selected: bool,
}

impl NewApp {
    /// A picker creating into `target`, with settings from `cfg_path`.
    /// The templates folder is seeded on first use.
    pub fn new(target: PathBuf, cfg_path: Option<&Path>) -> Self {
        let cfg = cfg_path.map(Config::load_from).unwrap_or_default();
        let dir = newplus::templates_dir_for(&cfg.new_plus);
        Self::with_templates_dir(target, dir, cfg)
    }

    pub fn with_templates_dir(target: PathBuf, templates_dir: PathBuf, cfg: Config) -> Self {
        if let Err(e) = newplus::ensure_templates(&templates_dir) {
            log::warn!("templates {}: {e}", templates_dir.display());
        }
        let mut s = Self {
            target,
            templates_dir,
            templates: Vec::new(),
            counts: Vec::new(),
            selected: 0,
            name: String::new(),
            name_for: None,
            select_stem: true,
            cfg,
            error: None,
            created: None,
            live: true,
            scroll_to_selected: false,
        };
        s.reload();
        s
    }

    fn reload(&mut self) {
        self.templates = newplus::list_templates(&self.templates_dir).unwrap_or_default();
        self.counts = self
            .templates
            .iter()
            .map(|t| {
                if t.is_dir {
                    std::fs::read_dir(&t.path)
                        .map(|r| {
                            r.flatten()
                                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                                .count()
                        })
                        .unwrap_or(0)
                } else {
                    0
                }
            })
            .collect();
        self.selected = self.selected.min(self.templates.len().saturating_sub(1));
        self.name_for = None;
    }

    fn opts(&self) -> NewOptions {
        NewOptions::from_config(&self.cfg.new_plus)
    }

    fn refill_name(&mut self) {
        if self.name_for == Some(self.selected) {
            return;
        }
        if let Some(t) = self.templates.get(self.selected) {
            self.name = newplus::default_name(t, &self.target, &self.opts());
            self.select_stem = true;
        }
        self.name_for = Some(self.selected);
    }

    fn create(&mut self, ctx: &egui::Context) {
        let Some(t) = self.templates.get(self.selected) else {
            return;
        };
        let opts = NewOptions {
            name: Some(self.name.clone()),
            ..self.opts()
        };
        match newplus::new_from_template_with(&t.path, &self.target, &opts) {
            Ok(p) => {
                self.error = None;
                if self.live {
                    if self.cfg.new_plus.reveal_after_create {
                        newplus::reveal(&p);
                    }
                    if self.cfg.new_plus.open_after_create {
                        newplus::open_path(&p);
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                self.created = Some(p);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn kind(&self, i: usize) -> String {
        let t = &self.templates[i];
        if t.is_dir {
            let n = self.counts[i];
            return format!("Folder · {n} item{}", if n == 1 { "" } else { "s" });
        }
        let ext = t.extension();
        let k = match ext.as_str() {
            "txt" => "Plain text".to_string(),
            "md" | "markdown" => "Markdown".into(),
            "rtf" => "Rich text".into(),
            "sh" | "zsh" | "bash" | "command" => "Shell script".into(),
            "html" | "htm" => "Web page".into(),
            "" => "File".into(),
            e => format!("{} file", e.to_uppercase()),
        };
        if ext.is_empty() {
            k
        } else {
            format!("{k} · .{ext}")
        }
    }

    /// One frame of the window.
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        let p = pal(ui);
        self.refill_name();
        let n = self.templates.len();
        let (up, down, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if n > 0 && (up || down) {
            self.selected = if up {
                self.selected.saturating_sub(1)
            } else {
                (self.selected + 1).min(n - 1)
            };
            self.scroll_to_selected = true;
            self.refill_name();
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
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if widgets::icon_text_button(ui, picon::FOLDER_OPEN, "Templates folder")
                        .on_hover_text(pretty_path(&self.templates_dir))
                        .clicked()
                    {
                        newplus::open_path(&self.templates_dir);
                    }
                    if widgets::icon_button(ui, picon::ARROWS_CLOCKWISE, "Reload templates")
                        .clicked()
                    {
                        self.reload();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ok = n > 0 && !self.name.trim().is_empty();
                        let r = ui.add_enabled_ui(ok, |ui| {
                            widgets::primary_icon_button(ui, picon::PLUS, "Create")
                        });
                        if r.inner.clicked() {
                            self.create(&ctx);
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(vec2(72.0, 28.0)))
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(18, 16)),
            )
            .show(ui, |ui| {
                // Header.
                ui.horizontal(|ui| {
                    widgets::icon_tile(ui, icon::FILE_PLUS, tint::BLUE, 30.0);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            RichText::new("New from template")
                                .font(theme::font_semibold(17.0))
                                .color(p.text),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(theme::icon_text(icon::FOLDER).size(12.0).color(p.text2));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(pretty_path(&self.target))
                                        .size(12.0)
                                        .color(p.text2),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
                ui.add_space(12.0);

                // Name field + error, measured first so the list fills the rest.
                let bottom_h = if self.error.is_some() { 96.0 } else { 70.0 };
                let list_h = (ui.available_height() - bottom_h).max(ROW_H * 2.0);
                let frame = widgets::card_frame(&p).inner_margin(egui::Margin::same(6));
                let mut create = false;
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_height(list_h - 12.0);
                    if n == 0 {
                        ui.add_space(list_h / 2.0 - 50.0);
                        ui.vertical_centered(|ui| {
                            ui.label(theme::icon_text(icon::FILES).size(30.0).color(p.text3));
                            ui.label(RichText::new("No templates yet").color(p.text));
                            ui.label(
                                RichText::new("Put files or folders in the templates folder.")
                                    .size(12.0)
                                    .color(p.text2),
                            );
                        });
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("templates")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for i in 0..n {
                                let (clicked, dbl) = self.template_row(ui, i, &p);
                                if clicked {
                                    self.selected = i;
                                    self.refill_name();
                                }
                                if dbl {
                                    create = true;
                                }
                            }
                        });
                });
                if create {
                    self.create(&ctx);
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Name")
                            .font(theme::font_semibold(13.0))
                            .color(p.text),
                    );
                    let id = egui::Id::unique("newplus-name");
                    let r = ui.add_enabled(
                        n > 0,
                        egui::TextEdit::singleline(&mut self.name)
                            .id(id)
                            .desired_width(ui.available_width())
                            .margin(vec2(8.0, 6.0)),
                    );
                    crate::rename_ui::focus_ring(ui, &r, &p);
                    if self.select_stem && n > 0 {
                        self.select_stem = false;
                        r.request_focus();
                        let is_dir = self.templates.get(self.selected).is_some_and(|t| t.is_dir);
                        let stem = if is_dir {
                            self.name.as_str()
                        } else {
                            split_ext(&self.name).0
                        };
                        let len = stem.chars().count();
                        if let Some(mut st) = egui::text_edit::TextEditState::load(&ctx, id) {
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(len),
                            )));
                            st.store(&ctx, id);
                        } else {
                            // First frame: no state yet; select next frame.
                            self.select_stem = true;
                        }
                    }
                    if r.changed() {
                        self.error = None;
                    }
                    if enter && (r.lost_focus() || !r.has_focus()) && n > 0 {
                        self.create(&ctx);
                    }
                });
                if let Some(e) = &self.error {
                    ui.add_space(6.0);
                    widgets::error_label(ui, e);
                }
            });
    }

    /// Returns (clicked, double-clicked).
    fn template_row(&mut self, ui: &mut Ui, i: usize, p: &Palette) -> (bool, bool) {
        let t = &self.templates[i];
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
        if self.scroll_to_selected && i == self.selected {
            resp.scroll_to_me(None);
            self.scroll_to_selected = false;
        }
        let sel = i == self.selected;
        let painter = ui.painter();
        if sel {
            painter.rect(
                r,
                8.0,
                p.accent_soft,
                Stroke::new(1.0, p.accent.gamma_multiply(0.55)),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            painter.rect_filled(r, 8.0, p.control);
        }
        let (glyph, tintc) = file_icon(&t.extension(), t.is_dir);
        let tile =
            Rect::from_min_size(pos2(r.left() + 10.0, r.center().y - 15.0), vec2(30.0, 30.0));
        widgets::paint_icon_tile(painter, tile, glyph, tintc);
        let name = t.display_name(
            self.cfg.new_plus.hide_extensions,
            self.cfg.new_plus.hide_starting_digits,
        );
        let x = tile.right() + 12.0;
        painter.text(
            pos2(x, r.center().y - 8.0),
            Align2::LEFT_CENTER,
            name,
            theme::font_semibold(13.5),
            p.text,
        );
        painter.text(
            pos2(x, r.center().y + 9.0),
            Align2::LEFT_CENTER,
            self.kind(i),
            theme::font(11.5),
            p.text2,
        );
        if sel {
            painter.text(
                pos2(r.right() - 16.0, r.center().y),
                Align2::CENTER_CENTER,
                "↩",
                theme::font(13.0),
                p.accent,
            );
        }
        let _ = Color32::TRANSPARENT;
        (resp.clicked(), resp.double_clicked())
    }
}

impl eframe::App for NewApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
impl NewApp {
    pub(crate) fn test_mode(&mut self) {
        self.live = false;
    }
    pub(crate) fn select(&mut self, i: usize) {
        self.selected = i;
        self.refill_name();
    }
    pub(crate) fn set_error(&mut self, e: &str) {
        self.error = Some(e.into());
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn create_now(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        self.create(ctx);
        self.created.clone()
    }
}

/// Open the New+ picker for `target_dir` and block until it is closed.
/// Must run on the main thread.
pub fn run_new_window(target_dir: PathBuf) -> anyhow::Result<()> {
    anyhow::ensure!(
        target_dir.is_dir(),
        "not a folder: {}",
        target_dir.display()
    );
    let cfg_path = Config::default_path();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("New from template")
            .with_app_id("com.powermac.newplus")
            .with_inner_size([480.0, 560.0])
            .with_min_inner_size([380.0, 360.0])
            .with_icon(crate::ui_common::app_icon([0x0A, 0x84, 0xFF], true))
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "New from template",
        options,
        Box::new(move |_cc| {
            crate::ui_common::become_foreground_app();
            Ok(Box::new(NewApp::new(target_dir, cfg_path.as_deref())))
        }),
    )
    .map_err(|e| anyhow::anyhow!("New+ window: {e}"))
}
