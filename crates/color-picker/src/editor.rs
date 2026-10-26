//! The Color Picker editor window (egui, PowerMac theme): the history on the
//! left; on the right a big swatch, PowerToys' lighter / darker variations,
//! hue / saturation / brightness adjustment, and every visible format with a
//! copy button. Runs in its own process via [`run_editor`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Id, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use pm_ui::theme::{self, icon, pal, Palette};
use pm_ui::widgets;

use crate::formats::{self, format_color, Rgb};
use crate::{history, Config};

/// Phosphor "eyedropper".
const EYEDROPPER: &str = "\u{E568}";
const SIDEBAR_W: f32 = 232.0;

fn c32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Open the editor window (blocks until it is closed). `data_dir` holds the
/// history file; `config` gives the formats to show.
pub fn run_editor(data_dir: impl Into<PathBuf>, config: Config) -> anyhow::Result<()> {
    let app = EditorApp::new(data_dir.into(), config);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Color Picker")
            .with_app_id("com.powermac.color-picker")
            .with_inner_size([820.0, 580.0])
            .with_min_inner_size([640.0, 440.0])
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "Color Picker",
        options,
        Box::new(move |_cc| {
            become_foreground_app();
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("editor window: {e}"))
}

fn become_foreground_app() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

/// Editor state; [`EditorApp::draw`] draws one frame.
pub struct EditorApp {
    data_dir: PathBuf,
    config: Config,
    history: Vec<Rgb>,
    stamp: Option<SystemTime>,
    last_poll: Instant,
    selected: usize,
    /// An adjusted color not saved to the history yet.
    draft: Option<Rgb>,
    /// Hue / saturation / value of the slider state (kept as floats so the
    /// hue survives gray colors).
    hsv: (f64, f64, f64),
    hsv_for: Option<Rgb>,
    hex_edit: String,
    hex_for: Option<Rgb>,
    toast: Option<(Instant, String)>,
    pick: Arc<Mutex<Option<Option<Rgb>>>>,
    picking: bool,
}

impl EditorApp {
    pub fn new(data_dir: PathBuf, config: Config) -> Self {
        let history = history::load(&data_dir);
        let stamp = history::stamp(&data_dir);
        Self {
            data_dir,
            config,
            history,
            stamp,
            last_poll: Instant::now(),
            selected: 0,
            draft: None,
            hsv: (0.0, 0.0, 0.0),
            hsv_for: None,
            hex_edit: String::new(),
            hex_for: None,
            toast: None,
            pick: Arc::new(Mutex::new(None)),
            picking: false,
        }
    }

    /// The color shown on the right.
    pub fn current(&self) -> Option<Rgb> {
        self.draft
            .or_else(|| self.history.get(self.selected).copied())
    }

    pub fn history(&self) -> &[Rgb] {
        &self.history
    }

    /// Select history entry `i` (tests / screenshots).
    pub fn select(&mut self, i: usize) {
        self.selected = i.min(self.history.len().saturating_sub(1));
        self.draft = None;
    }

    /// Show an adjusted, unsaved color (tests / screenshots).
    pub fn set_draft(&mut self, c: Option<Rgb>) {
        self.draft = c;
    }

    pub fn show_toast_for_test(&mut self, text: &str) {
        self.toast = Some((Instant::now(), text.to_string()));
    }

    fn save(&mut self) {
        if let Err(e) = history::save(&self.data_dir, &self.history) {
            log::warn!("color-picker: saving history: {e:#}");
        }
        self.stamp = history::stamp(&self.data_dir);
    }

    fn add(&mut self, c: Rgb) {
        history::push(&mut self.history, c, self.config.history_limit);
        self.selected = 0;
        self.draft = None;
        self.save();
    }

    fn remove(&mut self, i: usize) {
        if i < self.history.len() {
            self.history.remove(i);
            if self.selected > i || self.selected >= self.history.len() {
                self.selected = self.selected.saturating_sub(1);
            }
            self.draft = None;
            self.save();
        }
    }

    fn copy(&mut self, ctx: &egui::Context, text: String) {
        ctx.copy_text(text.clone());
        self.toast = Some((Instant::now(), format!("Copied {text}")));
    }

    /// Pick up history changes made by the picker in the tray app.
    fn poll(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed() >= Duration::from_millis(800) {
            self.last_poll = Instant::now();
            let s = history::stamp(&self.data_dir);
            if s != self.stamp {
                self.stamp = s;
                let before = self.history.first().copied();
                self.history = history::load(&self.data_dir);
                if self.history.first().copied() != before {
                    self.selected = 0;
                    self.draft = None;
                } else {
                    self.selected = self.selected.min(self.history.len().saturating_sub(1));
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(800));
        let picked = self.pick.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(res) = picked {
            self.picking = false;
            if let Some(c) = res {
                self.add(c);
                let text = self.config.copied_text(c);
                self.copy(ctx, text);
            }
        }
    }

    fn start_pick(&mut self, ctx: &egui::Context) {
        if self.picking {
            return;
        }
        self.picking = true;
        let slot = self.pick.clone();
        let ctx = ctx.clone();
        crate::platform::pick_async(Box::new(move |c| {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
            ctx.request_repaint();
        }));
    }

    /// One frame of the whole window.
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        self.poll(&ctx);
        let p = pal(ui);
        egui::Panel::left("history")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0, p.card_border)),
            )
            .show(ui, |ui| self.sidebar(ui));
        let mut content = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                content = ui.max_rect();
                match self.current() {
                    None => self.empty_state(ui),
                    Some(c) => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin {
                                        left: 24,
                                        right: 24,
                                        top: 22,
                                        bottom: 24,
                                    })
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        self.details(ui, c);
                                    });
                            });
                    }
                }
            });
        self.draw_toast(&ctx, content);
    }

    // ------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("History")
                    .font(theme::font_semibold(17.0))
                    .color(p.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.picking { "Picking…" } else { "Pick" };
                if widgets::primary_icon_button(ui, EYEDROPPER, label)
                    .on_hover_text("Pick a color from the screen")
                    .clicked()
                {
                    self.start_pick(&ctx);
                }
            });
        });
        ui.add_space(2.0);
        let n = self.history.len();
        ui.label(
            RichText::new(if n == 1 {
                "1 color".to_string()
            } else {
                format!("{n} colors")
            })
            .size(12.0)
            .color(p.text2),
        );
        ui.add_space(10.0);
        let mut remove = None;
        let mut select = None;
        let mut copy = None;
        let bottom_h = if n > 0 { 40.0 } else { 0.0 };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - bottom_h)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if self.history.is_empty() {
                    widgets::hint(ui, "Colors you pick appear here, newest first.");
                }
                for (i, c) in self.history.iter().copied().enumerate() {
                    let selected = self.draft.is_none() && i == self.selected;
                    match history_row(ui, &p, c, selected, &self.config) {
                        RowAction::None => {}
                        RowAction::Select => select = Some(i),
                        RowAction::Remove => remove = Some(i),
                        RowAction::Copy => copy = Some(c),
                    }
                }
            });
        if n == 0 {
            ui.add_space(4.0);
        }
        if let Some(i) = select {
            self.select(i);
        }
        if let Some(i) = remove {
            self.remove(i);
        }
        if let Some(c) = copy {
            let t = self.config.copied_text(c);
            self.copy(&ctx, t);
        }
        if n > 0 {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::Button::new(theme::icon_job(icon::TRASH, "Clear history", 12.5, p.text2))
                        .frame(false)
                        .min_size(vec2(0.0, 26.0)),
                );
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.history.clear();
                    self.selected = 0;
                    self.draft = None;
                    self.save();
                }
            });
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        let top = rect.center().y - 110.0;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(rect.left(), top), rect.max)),
            |ui| {
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                    ui.painter().rect_filled(r, 18.0, p.accent_soft);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, EYEDROPPER, theme::icon_font(32.0), p.accent);
                    ui.add_space(14.0);
                    ui.label(RichText::new("No colors yet").font(theme::font_semibold(18.0)).color(p.text));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Pick a color anywhere on the screen. It's copied\nto the clipboard and saved here.")
                            .size(13.0)
                            .color(p.text2),
                    );
                    ui.add_space(14.0);
                    if widgets::primary_icon_button(ui, EYEDROPPER, "Pick a color").clicked() {
                        self.start_pick(&ctx);
                    }
                });
            },
        );
    }

    // ------------------------------------------------------------- details

    fn details(&mut self, ui: &mut Ui, c: Rgb) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();

        // Hero swatch.
        let w = ui.available_width();
        let (hero, resp) = ui.allocate_exact_size(vec2(w, 148.0), Sense::click());
        let painter = ui.painter();
        painter.rect(
            hero,
            16.0,
            c32(c),
            Stroke::new(1.0, theme::mix(c32(c), Color32::BLACK, 0.12)),
            StrokeKind::Inside,
        );
        let fg = if c.is_light() {
            Color32::from_black_alpha(220)
        } else {
            Color32::WHITE
        };
        let fg2 = if c.is_light() {
            Color32::from_black_alpha(150)
        } else {
            Color32::from_white_alpha(190)
        };
        let mut y = hero.bottom() - 22.0;
        painter.text(
            pos2(hero.left() + 22.0, y),
            Align2::LEFT_BOTTOM,
            c.hex(),
            theme::font_semibold(30.0),
            fg,
        );
        y -= 40.0;
        let name = formats::color_name(c);
        let sub = if self.config.show_color_name {
            format!(
                "{name}  ·  {}",
                format_color(c, formats::builtin_format("RGB").unwrap())
            )
        } else {
            format_color(c, formats::builtin_format("RGB").unwrap())
        };
        painter.text(
            pos2(hero.left() + 23.0, y),
            Align2::LEFT_BOTTOM,
            sub,
            theme::font(13.5),
            fg2,
        );
        // Copy chip in the corner.
        let chip = Rect::from_min_size(
            pos2(hero.right() - 16.0 - 92.0, hero.top() + 16.0),
            vec2(92.0, 30.0),
        );
        let hovered = resp.hovered();
        let chip_fill = if c.is_light() {
            Color32::from_black_alpha(if hovered { 34 } else { 22 })
        } else {
            Color32::from_white_alpha(if hovered { 60 } else { 40 })
        };
        painter.rect_filled(chip, 15.0, chip_fill);
        let mut job = egui::text::LayoutJob::default();
        job.append(
            icon::COPY,
            0.0,
            egui::TextFormat::simple(theme::icon_font(14.0), fg),
        );
        job.append(
            &self.config.copied_format,
            6.0,
            egui::TextFormat::simple(theme::font_semibold(12.5), fg),
        );
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        painter.galley(chip.center() - galley.size() / 2.0, galley, fg);
        if self.draft.is_some() {
            let badge = Rect::from_min_size(
                pos2(hero.left() + 16.0, hero.top() + 16.0),
                vec2(84.0, 26.0),
            );
            painter.rect_filled(badge, 13.0, chip_fill);
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                "Adjusted",
                theme::font_semibold(12.0),
                fg,
            );
        }
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Copy")
            .clicked()
        {
            let t = self.config.copied_text(c);
            self.copy(&ctx, t);
        }

        // Variations: two lighter, the color, two darker.
        ui.add_space(12.0);
        let [l2, l1, d1, d2] = editor_variations(c);
        let chips = [
            (l2, "Lighter"),
            (l1, "Light"),
            (c, "Current"),
            (d1, "Dark"),
            (d2, "Darker"),
        ];
        let (strip, _) = ui.allocate_exact_size(vec2(w, 46.0), Sense::hover());
        let gap = 8.0;
        let cw = (w - gap * 4.0) / 5.0;
        let mut chosen = None;
        for (i, (col, label)) in chips.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(strip.left() + i as f32 * (cw + gap), strip.top()),
                vec2(cw, 46.0),
            );
            let resp = ui.interact(r, ui.scope_id().with(("variation", i)), Sense::click());
            let lift = if resp.hovered() && i != 2 { 1.0 } else { 0.0 };
            let r = r.translate(vec2(0.0, -lift));
            ui.painter().rect(
                r,
                10.0,
                c32(*col),
                Stroke::new(1.0, theme::mix(c32(*col), Color32::BLACK, 0.12)),
                StrokeKind::Inside,
            );
            let t = if col.is_light() {
                Color32::from_black_alpha(190)
            } else {
                Color32::from_white_alpha(230)
            };
            ui.painter().text(
                r.center() - vec2(0.0, 7.0),
                Align2::CENTER_CENTER,
                *label,
                theme::font(11.0),
                t,
            );
            ui.painter().text(
                r.center() + vec2(0.0, 8.0),
                Align2::CENTER_CENTER,
                col.hex(),
                theme::font_semibold(11.5),
                t,
            );
            if i == 2 {
                ui.painter().rect_stroke(
                    r.expand(2.5),
                    12.0,
                    Stroke::new(2.0, p.accent),
                    StrokeKind::Outside,
                );
            }
            if i != 2
                && resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Use this color")
                    .clicked()
            {
                chosen = Some(*col);
            }
        }
        if let Some(col) = chosen {
            self.draft = Some(col);
        }

        self.formats_card(ui, c);
        self.adjust_card(ui, c);
    }

    fn adjust_card(&mut self, ui: &mut Ui, c: Rgb) {
        let p = pal(ui);
        if self.hsv_for != Some(c) {
            let (h, s, v) = formats::to_hsv(c);
            // Keep the slider hue when the color has none (grays).
            let h = if s == 0.0 || v == 0.0 { self.hsv.0 } else { h };
            self.hsv = (h, s, v);
            self.hsv_for = Some(c);
        }
        if self.hex_for != Some(c) {
            self.hex_edit = c.hex();
            self.hex_for = Some(c);
        }
        let mut changed = false;
        let (mut h, mut s, mut v) = self.hsv;
        widgets::card(ui, "Adjust", |ui| {
            ui.add_space(6.0);
            let (h0, s0, v0) = (h, s, v);
            changed |= gradient_slider(
                ui,
                &p,
                "Hue",
                &mut h,
                0.0..=360.0,
                |t| formats::from_hsv(t * 360.0, 1.0, 1.0),
                format!("{}°", h0.round()),
            );
            changed |= gradient_slider(
                ui,
                &p,
                "Saturation",
                &mut s,
                0.0..=1.0,
                |t| formats::from_hsv(h0, t, v0.max(0.15)),
                format!("{}%", (s0 * 100.0).round()),
            );
            changed |= gradient_slider(
                ui,
                &p,
                "Brightness",
                &mut v,
                0.0..=1.0,
                |t| formats::from_hsv(h0, s0, t),
                format!("{}%", (v0 * 100.0).round()),
            );
            ui.add_space(4.0);
            widgets::divider(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Hex").color(p.text2));
                ui.add_space(4.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.hex_edit)
                        .desired_width(110.0)
                        .font(theme::font_semibold(13.5))
                        .char_limit(9),
                );
                if r.changed() {
                    if let Some(n) = Rgb::from_hex(&self.hex_edit) {
                        if Some(n) != self.current() {
                            self.draft = Some(n);
                            self.hex_for = Some(n);
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.draft.is_some() {
                        if widgets::primary_icon_button(ui, icon::PLUS, "Save to history").clicked()
                        {
                            if let Some(d) = self.draft {
                                self.add(d);
                            }
                        }
                        if widgets::icon_text_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Reset")
                            .clicked()
                        {
                            self.draft = None;
                        }
                    } else {
                        widgets::hint(ui, "Drag a slider to adjust the color");
                    }
                });
            });
            ui.add_space(8.0);
        });
        if changed {
            self.hsv = (h, s, v);
            let n = formats::from_hsv(h, s, v);
            self.hsv_for = Some(n);
            if Some(n) != self.history.get(self.selected).copied() || self.draft.is_some() {
                self.draft = Some(n);
            }
        }
    }

    fn formats_card(&mut self, ui: &mut Ui, c: Rgb) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let mut copy = None;
        let visible = self.config.visible();
        let name_w = visible
            .iter()
            .map(|(n, _)| {
                ui.fonts_mut(|f| {
                    f.layout_no_wrap(n.clone(), theme::font(13.0), p.text2)
                        .size()
                        .x
                })
            })
            .fold(64.0f32, f32::max)
            + 18.0;
        widgets::card(ui, "Formats", |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (name, template)) in visible.iter().enumerate() {
                if i > 0 {
                    widgets::divider(ui);
                }
                let value = format_color(c, template);
                let (row, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(row.expand2(vec2(10.0, -3.0)), 8.0, p.control);
                }
                let painter = ui.painter();
                painter.text(
                    pos2(row.left(), row.center().y),
                    Align2::LEFT_CENTER,
                    name,
                    theme::font(13.0),
                    p.text2,
                );
                let max_w = row.width() - name_w - 44.0;
                let galley = ui.fonts_mut(|f| {
                    let mut job = egui::text::LayoutJob::single_section(
                        value.clone(),
                        egui::TextFormat::simple(theme::font_semibold(13.5), p.text),
                    );
                    job.wrap.max_width = max_w;
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    f.layout_job(job)
                });
                painter.galley(
                    pos2(row.left() + name_w, row.center().y - galley.size().y / 2.0),
                    galley,
                    p.text,
                );
                if *name == self.config.copied_format {
                    let tag = Rect::from_min_size(
                        pos2(row.right() - 36.0 - 58.0, row.center().y - 10.0),
                        vec2(54.0, 20.0),
                    );
                    if !resp.hovered() {
                        painter.rect_filled(tag, 10.0, p.accent_soft);
                        painter.text(
                            tag.center(),
                            Align2::CENTER_CENTER,
                            "Default",
                            theme::font_semibold(11.0),
                            p.accent,
                        );
                    }
                }
                let btn = Rect::from_center_size(
                    pos2(row.right() - 14.0, row.center().y),
                    vec2(28.0, 28.0),
                );
                let bresp =
                    ui.interact(btn, ui.scope_id().with(("copy-format", i)), Sense::click());
                if bresp.hovered() {
                    ui.painter().rect_filled(btn, 7.0, p.control_hover);
                }
                ui.painter().text(
                    btn.center(),
                    Align2::CENTER_CENTER,
                    icon::COPY,
                    theme::icon_font(15.0),
                    if resp.hovered() || bresp.hovered() {
                        p.accent
                    } else {
                        p.text2
                    },
                );
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    || bresp.clicked()
                {
                    copy = Some(value);
                }
            }
        });
        if let Some(t) = copy {
            self.copy(&ctx, t);
        }
        ui.add_space(6.0);
    }

    fn draw_toast(&mut self, ctx: &egui::Context, content: Rect) {
        let Some((t, text)) = self.toast.clone() else {
            return;
        };
        let age = t.elapsed().as_secs_f32();
        const SHOW: f32 = 1.6;
        if age > SHOW + 0.3 {
            self.toast = None;
            return;
        }
        let alpha = if age < 0.12 {
            age / 0.12
        } else if age > SHOW {
            1.0 - (age - SHOW) / 0.3
        } else {
            1.0
        };
        ctx.request_repaint();
        let p = theme::pal_ctx(ctx);
        let text_w = ctx.fonts_mut(|f| {
            f.layout_no_wrap(text.clone(), theme::font_semibold(13.0), Color32::WHITE)
                .size()
                .x
        });
        let w = (text_w + 32.0 + 34.0).min(content.width() - 32.0);
        egui::Area::new(Id::unique("cp-toast"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos2(
                content.center().x - w / 2.0,
                content.bottom() - 58.0 + (1.0 - alpha) * 8.0,
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
                        ui.set_max_width(w - 32.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::icon_text(icon::CHECK_CIRCLE).size(15.0).color(p.ok));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text)
                                        .font(theme::font_semibold(13.0))
                                        .color(Color32::WHITE),
                                )
                                .truncate(),
                            );
                        });
                    });
            });
    }
}

/// PowerToys' variations, except that a "lighter" variant that is hardly
/// lighter (the color is already at full brightness) becomes a tint, and a
/// "darker" one of an almost black color a shade, so all five chips differ.
fn editor_variations(c: Rgb) -> [Rgb; 4] {
    let dist = |a: Rgb, b: Rgb| {
        (a.r as i32 - b.r as i32).abs()
            + (a.g as i32 - b.g as i32).abs()
            + (a.b as i32 - b.b as i32).abs()
    };
    let mut v = formats::variations(c);
    for (i, t) in [(0usize, 0.36), (1, 0.18)] {
        if dist(v[i], c) < 24 * (2 - i as i32) {
            v[i] = formats::mix_white_black(c, t);
        }
    }
    for (i, t) in [(2usize, 0.2), (3, 0.36)] {
        if dist(v[i], c) < 12 {
            v[i] = formats::mix_white_black(c, -t);
        }
    }
    v
}

enum RowAction {
    None,
    Select,
    Remove,
    Copy,
}

fn history_row(ui: &mut Ui, p: &Palette, c: Rgb, selected: bool, cfg: &Config) -> RowAction {
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let painter = ui.painter();
    let hovered = resp.hovered();
    if selected {
        painter.rect_filled(row, 9.0, p.accent_soft);
    } else if hovered {
        painter.rect_filled(row, 9.0, p.control_hover);
    }
    let sw = Rect::from_center_size(
        pos2(row.left() + 8.0 + 15.0, row.center().y),
        vec2(30.0, 30.0),
    );
    painter.rect(
        sw,
        8.0,
        c32(c),
        Stroke::new(1.0, theme::mix(c32(c), Color32::BLACK, 0.15)),
        StrokeKind::Inside,
    );
    let tx = sw.right() + 10.0;
    painter.text(
        pos2(tx, row.center().y - 8.0),
        Align2::LEFT_CENTER,
        c.hex(),
        theme::font_semibold(13.0),
        p.text,
    );
    let sub = if cfg.show_color_name {
        formats::color_name(c).to_string()
    } else {
        format_color(c, formats::builtin_format("RGB").unwrap())
    };
    painter.text(
        pos2(tx, row.center().y + 9.0),
        Align2::LEFT_CENTER,
        sub,
        theme::font(11.5),
        p.text2,
    );
    let mut action = RowAction::None;
    if hovered || selected {
        let btn =
            Rect::from_center_size(pos2(row.right() - 18.0, row.center().y), vec2(26.0, 26.0));
        let b = ui.interact(btn, ui.scope_id().with(("remove", c.hex())), Sense::click());
        if b.hovered() {
            ui.painter().rect_filled(
                btn,
                7.0,
                if selected {
                    theme::mix(p.accent_soft, p.accent, 0.15)
                } else {
                    p.control_active
                },
            );
        }
        if hovered || b.hovered() {
            ui.painter().text(
                btn.center(),
                Align2::CENTER_CENTER,
                icon::TRASH,
                theme::icon_font(14.0),
                if b.hovered() { p.error } else { p.text2 },
            );
        }
        if b.on_hover_text("Remove from history").clicked() {
            return RowAction::Remove;
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Copy").clicked() {
            action = RowAction::Copy;
            ui.close();
        }
        if ui.button("Remove").clicked() {
            action = RowAction::Remove;
            ui.close();
        }
    });
    if resp.clicked() {
        return RowAction::Select;
    }
    action
}

/// A labeled slider whose track shows `color_at(t)` for t in 0..=1.
fn gradient_slider(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    color_at: impl Fn(f64) -> Rgb,
    shown: String,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let mut changed = false;
    ui.horizontal(|ui| {
        let (lr, _) = ui.allocate_exact_size(vec2(84.0, 30.0), Sense::hover());
        ui.painter().text(
            pos2(lr.left(), lr.center().y),
            Align2::LEFT_CENTER,
            label,
            theme::font(13.5),
            p.text,
        );
        let w = (ui.available_width() - 60.0).max(80.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click_and_drag());
        let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 18.0, 12.0));
        // Gradient mesh between round end caps.
        let n = 48;
        let r = track.height() / 2.0;
        let inner = track.shrink2(vec2(r, 0.0));
        let mut mesh = egui::Mesh::default();
        for i in 0..=n {
            let t = i as f64 / n as f64;
            let x = inner.left() + inner.width() * t as f32;
            let col = c32(color_at(t));
            mesh.colored_vertex(pos2(x, inner.top()), col);
            mesh.colored_vertex(pos2(x, inner.bottom()), col);
            if i > 0 {
                let b = (i * 2) as u32;
                mesh.add_triangle(b - 2, b - 1, b);
                mesh.add_triangle(b - 1, b, b + 1);
            }
        }
        let painter = ui.painter();
        painter.circle_filled(pos2(inner.left(), track.center().y), r, c32(color_at(0.0)));
        painter.circle_filled(pos2(inner.right(), track.center().y), r, c32(color_at(1.0)));
        painter.add(egui::Shape::mesh(mesh));
        painter.rect_stroke(
            track,
            r,
            Stroke::new(1.0, Color32::from_black_alpha(if p.dark { 60 } else { 22 })),
            StrokeKind::Outside,
        );
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on() {
                let t = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
                let nv = lo + (hi - lo) * t;
                if (nv - *value).abs() > f64::EPSILON {
                    *value = nv;
                    changed = true;
                }
            }
        }
        let t = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0);
        let knob = pos2(inner.left() + inner.width() * t as f32, track.center().y);
        let painter = ui.painter();
        painter.circle_filled(knob + vec2(0.0, 1.0), 10.5, Color32::from_black_alpha(40));
        painter.circle_filled(knob, 10.0, Color32::WHITE);
        painter.circle_filled(knob, 7.0, c32(color_at(t)));
        painter.circle_stroke(knob, 10.0, Stroke::new(1.0, Color32::from_black_alpha(30)));
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(shown)
                    .font(theme::font_semibold(12.5))
                    .color(p.text2),
            );
        });
    });
    changed
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}
