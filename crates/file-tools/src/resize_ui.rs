//! The Image Resizer window: the size presets as radio cards (the custom
//! size expands into fields), options on the left, Resize with progress and
//! a per-file result list.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};
use pm_ui::theme::{self, icon as picon, pal, Palette};
use pm_ui::widgets::{self, block, card, row, segmented, toggle_row};

use crate::config::Config;
use crate::resize::{
    self, compute_dimensions, fmt_num, ImageInfo, OutputFormat, ResizeFit, ResizeOptions,
    ResizeSize, ResizeUnit,
};
use crate::ui_common::{icon, pretty_path, tint};

const OPTIONS_W: f32 = 344.0;
const RESULT_ROW_H: f32 = 46.0;

/// A finished file: the output and its size, or the error.
pub(crate) type FileResult = Result<(PathBuf, (u32, u32)), String>;

struct Job {
    rx: mpsc::Receiver<(usize, FileResult)>,
    cancel: Arc<AtomicBool>,
    done: usize,
}

/// The window's state.
pub struct ResizeApp {
    files: Vec<PathBuf>,
    first_info: Option<ImageInfo>,
    cfg: Config,
    cfg_path: Option<PathBuf>,
    selected: usize,
    custom: ResizeSize,
    opts: ResizeOptions,
    formats: Vec<OutputFormat>,
    job: Option<Job>,
    results: Vec<Option<FileResult>>,
    show_results: bool,
    /// Close / reveal for real (off in tests).
    live: bool,
}

impl ResizeApp {
    /// A window for `paths` (folders are expanded to the images in them),
    /// with settings from `cfg_path`.
    pub fn new(paths: Vec<PathBuf>, cfg_path: Option<PathBuf>) -> Self {
        let cfg = cfg_path
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_default();
        let files: Vec<PathBuf> = resize::collect_images(&paths)
            .into_iter()
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect();
        let first_info = files.iter().find_map(|f| resize::image_info(f).ok());
        let r = &cfg.resize;
        let selected = r.selected.min(r.presets.len());
        let formats = resize::available_formats();
        let mut opts = r.options.clone();
        if !formats.contains(&opts.format) {
            opts.format = OutputFormat::Keep;
        }
        Self {
            results: vec![None; files.len()],
            files,
            first_info,
            selected,
            custom: r.custom.clone(),
            opts,
            formats,
            cfg,
            cfg_path,
            job: None,
            show_results: false,
            live: true,
        }
    }

    fn size_at(&self, i: usize) -> ResizeSize {
        self.cfg
            .resize
            .presets
            .get(i)
            .cloned()
            .unwrap_or_else(|| self.custom.clone())
    }

    fn current_options(&self) -> ResizeOptions {
        ResizeOptions {
            size: self.size_at(self.selected),
            ..self.opts.clone()
        }
    }

    fn save_cfg(&mut self) {
        self.cfg.resize.selected = self.selected;
        self.cfg.resize.custom = self.custom.clone();
        self.cfg.resize.options = self.current_options();
        if let Some(p) = &self.cfg_path {
            if let Err(e) = self.cfg.save_to(p) {
                log::warn!("{}: {e}", p.display());
            }
        }
    }

    fn can_resize(&self) -> bool {
        let s = self.size_at(self.selected);
        let valid = !(s.width == 0.0 && s.height == 0.0);
        self.job.is_none() && !self.files.is_empty() && valid
    }

    fn start(&mut self, ctx: &egui::Context) {
        if !self.can_resize() {
            return;
        }
        self.save_cfg();
        let opts = self.current_options();
        let files = self.files.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let c2 = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (i, f) in files.iter().enumerate() {
                if c2.load(Ordering::Relaxed) {
                    break;
                }
                let r = resize::resize_one(f, &opts)
                    .map(|out| {
                        let d = resize::image_info(&out)
                            .map(|i| (i.width, i.height))
                            .unwrap_or((0, 0));
                        (out, d)
                    })
                    .map_err(|e| format!("{e:#}"));
                if tx.send((i, r)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });
        self.results = vec![None; self.files.len()];
        self.show_results = true;
        self.job = Some(Job {
            rx,
            cancel,
            done: 0,
        });
    }

    fn poll(&mut self) {
        let Some(job) = &mut self.job else { return };
        loop {
            match job.rx.try_recv() {
                Ok((i, r)) => {
                    self.results[i] = Some(r);
                    job.done += 1;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    return;
                }
            }
        }
    }

    fn counts(&self) -> (usize, usize) {
        let ok = self
            .results
            .iter()
            .filter(|r| matches!(r, Some(Ok(_))))
            .count();
        let err = self
            .results
            .iter()
            .filter(|r| matches!(r, Some(Err(_))))
            .count();
        (ok, err)
    }

    /// One frame of the window.
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        self.poll();
        let p = pal(ui);
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(j) = &self.job {
                j.cancel.store(true, Ordering::Relaxed);
            }
            self.save_cfg();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.job.is_none() {
            self.close(&ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.show_results
            && ctx.memory(|m| m.focused().is_none())
        {
            self.start(&ctx);
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
            .show(ui, |ui| {
                self.header(ui, &p);
                ui.add_space(14.0);
                if self.show_results {
                    self.results_list(ui, &p);
                } else {
                    self.sizes(ui, &p);
                }
            });
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.save_cfg();
        if self.live {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // -----------------------------------------------------------------
    // Header

    fn header(&self, ui: &mut Ui, p: &Palette) {
        let chip_w = if self.first_info.is_some() {
            150.0
        } else {
            0.0
        };
        ui.horizontal(|ui| {
            widgets::icon_tile(ui, icon::FRAME_CORNERS, tint::TEAL, 30.0);
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width() - chip_w);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("Image Resizer")
                        .font(theme::font_semibold(17.0))
                        .color(p.text),
                );
                let n = self.files.len();
                let mut s = format!("{n} picture{}", if n == 1 { "" } else { "s" });
                let dirs: Vec<&Path> = self.files.iter().filter_map(|f| f.parent()).collect();
                if let Some(d) = dirs.first() {
                    if dirs.iter().all(|x| x == d) {
                        s += &format!(" · {}", pretty_path(d));
                    } else {
                        s += " · several folders";
                    }
                }
                let r =
                    ui.add(egui::Label::new(RichText::new(s).size(12.0).color(p.text2)).truncate());
                if n > 0 {
                    r.on_hover_ui(|ui| {
                        ui.set_max_width(420.0);
                        for f in self.files.iter().take(14) {
                            ui.label(RichText::new(pretty_path(f)).size(12.0));
                        }
                        if n > 14 {
                            ui.label(RichText::new(format!("… and {} more", n - 14)).size(12.0));
                        }
                    });
                }
            });
            if let Some(i) = &self.first_info {
                let h = ui.min_rect().height();
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), h),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        chip(ui, p, icon::IMAGE, &format!("{} × {}", i.width, i.height))
                            .on_hover_text("Size of the first picture");
                    },
                );
            }
        });
    }

    // -----------------------------------------------------------------
    // Sizes

    fn size_icon(s: &ResizeSize, custom: bool) -> (&'static str, Color32) {
        if custom {
            return (picon::SLIDERS_HORIZONTAL, tint::GRAY);
        }
        let n = s.name.to_ascii_lowercase();
        if n.contains("phone") || n.contains("mobile") {
            (icon::DEVICE_MOBILE, tint::GREEN)
        } else if n.contains("small") {
            (icon::TELEVISION_SIMPLE, tint::ORANGE)
        } else if n.contains("medium") {
            (picon::LAPTOP, tint::BLUE)
        } else if n.contains("large") {
            (picon::MONITOR, tint::INDIGO)
        } else {
            (icon::IMAGE, tint::PURPLE)
        }
    }

    fn describe(s: &ResizeSize) -> String {
        let fit = match s.fit {
            ResizeFit::Fit => "Fit",
            ResizeFit::Fill => "Fill",
            ResizeFit::Stretch => "Stretch",
        };
        format!("{} · {fit}", s.describe())
    }

    /// "→ 853 × 480" for the first picture.
    fn preview(&self, s: &ResizeSize) -> Option<String> {
        let i = self.first_info.as_ref()?;
        let d = compute_dimensions(
            i.width,
            i.height,
            i.dpi_x,
            i.dpi_y,
            s,
            self.opts.shrink_only,
            self.opts.ignore_orientation,
        )
        .ok()?;
        let (w, h) = d.output();
        Some(if d.unchanged {
            format!("{w} × {h} (unchanged)")
        } else {
            format!("{w} × {h}")
        })
    }

    fn sizes(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Size")
                    .font(theme::font_semibold(15.0))
                    .color(p.text),
            );
        });
        ui.add_space(6.0);
        let n = self.cfg.resize.presets.len();
        egui::ScrollArea::vertical()
            .id_salt("sizes")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for i in 0..=n {
                    self.size_card(ui, i, i == n, p);
                }
                if self.files.is_empty() {
                    ui.add_space(6.0);
                    widgets::warn_label(ui, "No pictures were selected.");
                }
                ui.spacing_mut().item_spacing.y = 4.0;
                self.output(ui);
                ui.add_space(8.0);
            });
    }

    fn size_card(&mut self, ui: &mut Ui, i: usize, custom: bool, p: &Palette) {
        let size = self.size_at(i);
        let sel = self.selected == i;
        let id = ui.scope_id().with(("size", i));
        let hovered = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
        let fill = if sel {
            p.accent_soft
        } else if hovered && ui.is_enabled() {
            theme::mix(p.card, p.control, 0.6)
        } else {
            p.card
        };
        let stroke = if sel {
            Stroke::new(1.5, p.accent.gamma_multiply(0.75))
        } else {
            Stroke::new(1.0, p.card_border)
        };
        let frame = egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(14, 10));
        let mut head_rect = Rect::NOTHING;
        let r = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            let head = ui.horizontal(|ui| {
                ui.set_min_height(36.0);
                let (rr, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                paint_radio(ui.painter(), rr, sel, p);
                ui.add_space(4.0);
                let (g, t) = Self::size_icon(&size, custom);
                widgets::icon_tile(ui, g, t, 30.0);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(
                        RichText::new(if custom { "Custom" } else { size.name.as_str() })
                            .font(theme::font_semibold(13.5))
                            .color(p.text),
                    );
                    ui.label(
                        RichText::new(Self::describe(&size))
                            .size(12.0)
                            .color(p.text2),
                    );
                });
                if let Some(pv) = self.preview(&size) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(pv).size(12.0).color(if sel {
                            p.accent
                        } else {
                            p.text2
                        }));
                        ui.label(
                            theme::icon_text(icon::ARROW_RIGHT)
                                .size(12.0)
                                .color(p.text3),
                        );
                    });
                }
            });
            head_rect = head.response.rect;
            if custom && sel {
                ui.add_space(8.0);
                self.custom_editor(ui, p);
            }
        });
        let outer = r.response.rect;
        let click_rect = if custom && sel {
            head_rect.expand(6.0)
        } else {
            outer
        };
        let resp = ui.interact(click_rect, id.with("click"), Sense::click());
        let now_hovered = resp.hovered();
        if now_hovered != hovered {
            ui.data_mut(|d| d.insert_temp(id, now_hovered));
            ui.ctx().request_repaint();
        }
        if resp.clicked() && ui.is_enabled() {
            self.selected = i;
        }
        let _ = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }

    fn custom_editor(&mut self, ui: &mut Ui, p: &Palette) {
        let c = &mut self.custom;
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Fit").size(12.5).color(p.text2));
                    ui.add_space(8.0);
                    segmented(
                        ui,
                        &mut c.fit,
                        &[
                            (ResizeFit::Fill, "Fill"),
                            (ResizeFit::Fit, "Fit"),
                            (ResizeFit::Stretch, "Stretch"),
                        ],
                    );
                });
                ui.horizontal(|ui| {
                    let pct = c.unit == ResizeUnit::Percent && c.fit != ResizeFit::Stretch;
                    let decimals = if matches!(c.unit, ResizeUnit::Pixel) { 0 } else { 2 };
                    let num = |ui: &mut Ui, v: &mut f64, enabled: bool| {
                        ui.add_enabled(
                            enabled,
                            egui::DragValue::new(v)
                                .range(0.0..=100_000.0)
                                .speed(if decimals == 0 { 1.0 } else { 0.1 })
                                .max_decimals(decimals)
                                .custom_formatter(|v, _| {
                                    if v == 0.0 {
                                        "auto".into()
                                    } else {
                                        fmt_num(v)
                                    }
                                })
                                .custom_parser(|s| {
                                    let s = s.trim();
                                    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
                                        Some(0.0)
                                    } else {
                                        s.parse().ok()
                                    }
                                }),
                        )
                    };
                    ui.spacing_mut().interact_size = vec2(76.0, 28.0);
                    ui.label(RichText::new("Width").size(12.5).color(p.text2));
                    num(ui, &mut c.width, true);
                    ui.label(RichText::new("×").size(14.0).color(p.text3));
                    ui.label(RichText::new("Height").size(12.5).color(p.text2));
                    num(ui, &mut c.height, !pct).on_disabled_hover_text(
                        "Percent with Fit / Fill scales both sides by the width's percentage",
                    );
                    egui::ComboBox::from_id_salt("unit")
                        .selected_text(c.unit.label())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for u in ResizeUnit::ALL {
                                ui.selectable_value(&mut c.unit, u, u.label());
                            }
                        });
                });
                let hint = if c.width == 0.0 && c.height == 0.0 {
                    None
                } else {
                    Some("0 or “auto” keeps the aspect ratio from the other side. Centimeters and inches use each picture's resolution.")
                };
                match hint {
                    None => widgets::error_label(ui, "Enter a width or a height."),
                    Some(h) => {
                        ui.add(egui::Label::new(RichText::new(h).size(11.5).color(p.text2)).wrap());
                    }
                }
            });
        });
    }

    // -----------------------------------------------------------------
    // Options

    fn options(&mut self, ui: &mut Ui) {
        let o = &mut self.opts;
        card(ui, "Options", |ui| {
            toggle_row(
                ui,
                "Make pictures smaller but not larger",
                None,
                &mut o.shrink_only,
            );
            toggle_row(
                ui,
                "Ignore the orientation of pictures",
                Some("Turn the size for portrait pictures"),
                &mut o.ignore_orientation,
            );
            toggle_row(
                ui,
                "Overwrite the originals",
                Some(if cfg!(target_os = "macos") {
                    "The originals are moved to the Trash"
                } else {
                    "Instead of saving copies"
                }),
                &mut o.replace,
            );
            toggle_row(
                ui,
                "Keep the date modified",
                None,
                &mut o.keep_date_modified,
            );
        });
        card(ui, "Metadata", |ui| {
            toggle_row(
                ui,
                "Keep metadata",
                Some("Camera, date taken, copyright, …"),
                &mut o.keep_metadata,
            );
            ui.add_enabled_ui(o.keep_metadata, |ui| {
                toggle_row(
                    ui,
                    "Remove location",
                    Some("Drop the GPS position"),
                    &mut o.remove_gps,
                );
            });
        });
    }

    fn output(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let example = self.example_name();
        let o = &mut self.opts;
        let formats = self.formats.clone();
        card(ui, "Output", |ui| {
            row(ui, "Format", None, |ui| {
                let current = o.format.label();
                egui::ComboBox::from_id_salt("format")
                    .selected_text(current)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for f in &formats {
                            ui.selectable_value(&mut o.format, *f, f.label());
                        }
                    });
            });
            let lossy = o.format == OutputFormat::Keep || o.format.is_lossy();
            ui.add_enabled_ui(lossy, |ui| {
                row(ui, "Quality", Some("JPEG, HEIC and WebP"), |ui| {
                    ui.spacing_mut().slider_width = 92.0;
                    let mut q = u32::from(o.jpeg_quality);
                    ui.add(egui::Slider::new(&mut q, 1..=100).show_value(false));
                    ui.add_sized(
                        vec2(46.0, 24.0),
                        egui::DragValue::new(&mut q).range(1..=100),
                    );
                    o.jpeg_quality = q.clamp(1, 100) as u8;
                });
            });
            ui.add_enabled_ui(!o.replace, |ui| {
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(110.0, 30.0), Sense::hover());
                        ui.painter().text(
                            pos2(rect.left(), rect.center().y),
                            Align2::LEFT_CENTER,
                            "File name",
                            theme::font(14.0),
                            if o.replace { p.text3 } else { p.text },
                        );
                        let r = ui.add_sized(
                            vec2(ui.available_width(), 30.0),
                            egui::TextEdit::singleline(&mut o.file_name_format)
                                .hint_text(RichText::new("%1 (%2)").color(p.text3))
                                .margin(vec2(8.0, 6.0)),
                        );
                        crate::rename_ui::focus_ring(ui, &r, &p);
                    });
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new("%1 name · %2 size · %3 × %4 selected · %5 × %6 actual")
                                .size(11.5)
                                .color(p.text2),
                        )
                        .wrap(),
                    );
                });
            });
            if let Some(e) = &example {
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Example").color(p.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(e).size(12.5).color(p.text2))
                                    .truncate(),
                            );
                        });
                    });
                });
            }
        });
    }

    fn example_name(&self) -> Option<String> {
        let f = self.files.first()?;
        let opts = self.current_options();
        let fmt =
            resize::resolve_format(opts.format, self.first_info.as_ref().and_then(|i| i.format));
        let ext = resize::output_extension(f, fmt);
        if opts.replace {
            let stem = f.file_stem()?.to_string_lossy();
            return Some(format!("{stem}.{ext} (replaced)"));
        }
        let out = self
            .first_info
            .as_ref()
            .and_then(|i| {
                compute_dimensions(
                    i.width,
                    i.height,
                    i.dpi_x,
                    i.dpi_y,
                    &opts.size,
                    opts.shrink_only,
                    opts.ignore_orientation,
                )
                .ok()
            })
            .map(|d| d.output())
            .unwrap_or((0, 0));
        let stem = f.file_stem()?.to_string_lossy();
        Some(format!(
            "{}.{ext}",
            resize::format_file_name(&opts.file_name_format, &stem, &opts.size, out)
        ))
    }

    // -----------------------------------------------------------------
    // Results

    fn results_list(&mut self, ui: &mut Ui, p: &Palette) {
        let (ok, err) = self.counts();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Results")
                    .font(theme::font_semibold(15.0))
                    .color(p.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if err > 0 {
                    ui.label(
                        RichText::new(format!("{err} failed"))
                            .size(12.0)
                            .color(p.error),
                    );
                }
                ui.label(
                    RichText::new(format!("{ok} done"))
                        .size(12.0)
                        .color(p.text2),
                );
            });
        });
        ui.add_space(6.0);
        let frame = widgets::card_frame(p).inner_margin(egui::Margin::same(0));
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let n = self.files.len();
            let current = self.job.as_ref().map(|j| j.done);
            egui::ScrollArea::vertical()
                .id_salt("results")
                .auto_shrink([false, false])
                .show_rows(ui, RESULT_ROW_H, n, |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for i in range {
                        self.result_row(ui, i, current == Some(i), p);
                    }
                });
        });
    }

    fn result_row(&self, ui: &mut Ui, i: usize, active: bool, p: &Palette) {
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(vec2(w, RESULT_ROW_H), Sense::hover());
        let painter = ui.painter();
        if i > 0 {
            painter.hline(
                (r.left() + 56.0)..=r.right(),
                r.top() + 0.5,
                Stroke::new(1.0, p.separator),
            );
        }
        if resp.hovered() {
            painter.rect_filled(r.shrink(2.0), 6.0, p.control.gamma_multiply(0.6));
        }
        let f = &self.files[i];
        let tile =
            Rect::from_min_size(pos2(r.left() + 14.0, r.center().y - 14.0), vec2(28.0, 28.0));
        widgets::paint_icon_tile(painter, tile, icon::IMAGE, tint::TEAL);
        let x = tile.right() + 12.0;
        let name = f
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let maxw = (r.right() - x - 48.0).max(40.0);
        let g = clip(ui, &name, theme::font_semibold(13.0), p.text, maxw);
        painter.galley(pos2(x, r.center().y - 9.0 - g.size().y / 2.0), g, p.text);
        let (sub, color, glyph, gc) = match &self.results[i] {
            None if active => (
                "Resizing…".to_string(),
                p.text2,
                icon::SPINNER_GAP,
                p.accent,
            ),
            None => (
                if self.job.is_some() {
                    "Waiting"
                } else {
                    "Skipped"
                }
                .to_string(),
                p.text3,
                picon::CLOCK,
                p.text3,
            ),
            Some(Ok((out, (w, h)))) => (
                format!(
                    "{}  ·  {w} × {h}",
                    out.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                p.text2,
                picon::CHECK_CIRCLE,
                p.ok,
            ),
            Some(Err(e)) => (e.clone(), p.error, picon::WARNING_CIRCLE, p.error),
        };
        let sg = clip(ui, &sub, theme::font(11.5), color, maxw);
        painter.galley(pos2(x, r.center().y + 9.0 - sg.size().y / 2.0), sg, color);
        painter.text(
            pos2(r.right() - 22.0, r.center().y),
            Align2::CENTER_CENTER,
            glyph,
            theme::icon_font(17.0),
            gc,
        );
        if let Some(Ok((out, _))) = &self.results[i] {
            resp.on_hover_text(pretty_path(out));
        } else if let Some(Err(e)) = &self.results[i] {
            resp.on_hover_text(e);
        }
    }

    // -----------------------------------------------------------------
    // Footer

    fn footer(&mut self, ui: &mut Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let n = self.files.len();
            if let Some(job) = &self.job {
                let frac = if n == 0 {
                    0.0
                } else {
                    job.done as f32 / n as f32
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(240.0)
                        .desired_height(8.0)
                        .fill(p.accent)
                        .corner_radius(4.0),
                );
                ui.label(
                    RichText::new(format!("Resizing {} of {n}…", (job.done + 1).min(n)))
                        .size(12.5)
                        .color(p.text2),
                );
            } else if self.show_results {
                let (ok, err) = self.counts();
                let msg = format!("Resized {ok} picture{}", if ok == 1 { "" } else { "s" });
                if err == 0 {
                    widgets::ok_label(ui, &msg);
                } else {
                    widgets::error_label(ui, &format!("{msg}; {err} failed"));
                }
            } else {
                let s = self.size_at(self.selected);
                let name = if self.selected >= self.cfg.resize.presets.len() {
                    "a custom size".to_string()
                } else {
                    s.name.clone()
                };
                ui.label(
                    RichText::new(format!(
                        "{n} picture{} to {name} ({})",
                        if n == 1 { "" } else { "s" },
                        s.describe()
                    ))
                    .size(12.5)
                    .color(p.text2),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(job) = &self.job {
                    if ui
                        .add(egui::Button::new("Stop").min_size(vec2(72.0, 28.0)))
                        .clicked()
                    {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                } else if self.show_results {
                    if widgets::primary_button(ui, "  Done  ").clicked() {
                        self.close(&ctx);
                    }
                    let first = self.results.iter().find_map(|r| {
                        r.as_ref()
                            .and_then(|r| r.as_ref().ok())
                            .map(|(p, _)| p.clone())
                    });
                    if let Some(f) = first {
                        if widgets::icon_text_button(
                            ui,
                            picon::FOLDER_OPEN,
                            if cfg!(windows) {
                                "Show in Explorer"
                            } else {
                                "Show in Finder"
                            },
                        )
                        .clicked()
                            && self.live
                        {
                            crate::newplus::reveal(&f);
                        }
                    }
                    if widgets::icon_text_button(ui, picon::CARET_LEFT, "Back").clicked() {
                        self.show_results = false;
                    }
                } else {
                    let r = ui.add_enabled_ui(self.can_resize(), |ui| {
                        widgets::primary_icon_button(ui, icon::ARROWS_IN_SIMPLE, "Resize")
                    });
                    if r.inner.on_hover_text("Resize (↩)").clicked() {
                        self.start(&ctx);
                    }
                    if ui
                        .add(egui::Button::new("Cancel").min_size(vec2(72.0, 28.0)))
                        .clicked()
                    {
                        self.close(&ctx);
                    }
                }
            });
        });
    }
}

impl eframe::App for ResizeApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

fn paint_radio(painter: &egui::Painter, r: Rect, on: bool, p: &Palette) {
    let c = r.center();
    let rad = r.width() / 2.0;
    if on {
        painter.circle_filled(c, rad, p.accent);
        painter.circle_filled(c, rad * 0.4, p.on_accent);
    } else {
        painter.circle(c, rad - 0.5, p.input, Stroke::new(1.0, p.control_border));
    }
}

fn chip(ui: &mut Ui, p: &Palette, glyph: &str, text: &str) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        glyph,
        0.0,
        egui::TextFormat::simple(theme::icon_font(12.0), p.text2),
    );
    job.append(
        text,
        5.0,
        egui::TextFormat::simple(theme::font(12.0), p.text2),
    );
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let size = g.size() + vec2(18.0, 10.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        r,
        size.y / 2.0,
        p.card,
        Stroke::new(1.0, p.card_border),
        StrokeKind::Inside,
    );
    ui.painter().galley(r.min + vec2(9.0, 5.0), g, p.text2);
    resp
}

fn clip(ui: &Ui, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(20.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts_mut(|f| f.layout_job(job))
}

#[cfg(test)]
impl ResizeApp {
    pub(crate) fn test_mode(&mut self) {
        self.live = false;
    }
    pub(crate) fn select(&mut self, i: usize) {
        self.selected = i;
    }
    pub(crate) fn set_custom(&mut self, f: impl FnOnce(&mut ResizeSize)) {
        f(&mut self.custom);
    }
    pub(crate) fn set_opts(&mut self, f: impl FnOnce(&mut ResizeOptions)) {
        f(&mut self.opts);
    }
    /// Pretend a run is in progress: `done` finished (the rest pending).
    pub(crate) fn set_running(&mut self, done: usize, results: Vec<Option<FileResult>>) {
        let (_tx, rx) = mpsc::channel();
        std::mem::forget(_tx);
        self.results = results;
        self.show_results = true;
        self.job = Some(Job {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            done,
        });
    }
    pub(crate) fn set_finished(&mut self, results: Vec<Option<FileResult>>) {
        self.results = results;
        self.show_results = true;
        self.job = None;
    }
    /// Run the resize on this thread's terms and wait for it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn run_now(&mut self, ctx: &egui::Context) -> Vec<Option<FileResult>> {
        self.start(ctx);
        for _ in 0..2000 {
            self.poll();
            if self.job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.results.clone()
    }
    pub(crate) fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// Open the Image Resizer for `paths` (images, or folders of images) and
/// block until it is closed. Settings are in `file-tools.json`. Must run on
/// the main thread.
pub fn run_resize_window(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    run_resize_window_with(paths, Config::default_path())
}

/// [`run_resize_window`] with an explicit settings file.
pub fn run_resize_window_with(
    paths: Vec<PathBuf>,
    cfg_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Image Resizer")
            .with_app_id("com.powermac.imageresizer")
            .with_inner_size([960.0, 760.0])
            .with_min_inner_size([820.0, 560.0])
            .with_icon(crate::ui_common::app_icon_with(
                [0x30, 0xB0, 0xC7],
                crate::ui_common::Mark::Frames,
            ))
            .with_active(true),
        ..Default::default()
    };
    eframe::run_native(
        "Image Resizer",
        options,
        Box::new(move |_cc| {
            crate::ui_common::become_foreground_app();
            Ok(Box::new(ResizeApp::new(paths, cfg_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Image Resizer window: {e}"))
}
