//! The picker window: search, list, previews, pin / delete, paste.
//!
//! Runs in its own process (`powermac clipboard` → [`run_picker`]). It asks
//! the history service in the tray app for the items over [`crate::ipc`];
//! when no service is running it opens the history files itself.
//!
//! Keys: type to search · ↑/↓ select · Return pastes · Shift+Return pastes
//! as plain text · Cmd/Ctrl+1–9 (or plain 1–9 while the search box is
//! empty) picks · Cmd/Ctrl+P pins · Cmd/Ctrl+Backspace deletes · Esc closes.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, Key, Modifiers, Rect, RichText, Sense,
    Stroke, StrokeKind, TextureHandle, Ui,
};
use pm_ui::theme::{self, icon, pal, Palette};

use crate::config::{Config, PickerPosition};
use crate::content::Kind;
use crate::ipc::{self, Request};
use crate::platform::{self, FrontApp};
use crate::store::{self, ItemSummary, Store};

pub const WIDTH: f32 = 580.0;
pub const HEIGHT: f32 = 540.0;
const LIST_LIMIT: usize = 500;

/// Phosphor glyphs not in `pm_ui::theme::icon`.
pub(crate) mod glyph {
    pub const IMAGE: &str = "\u{E2CA}";
    pub const FILES: &str = "\u{E710}";
    pub const PUSH_PIN: &str = "\u{E3E2}";
    pub const TEXT_AA: &str = "\u{E6EE}";
    pub const PAUSE: &str = "\u{E39E}";
}

/// Where the items come from.
pub(crate) enum Backend {
    /// The service in the tray app.
    Remote(ipc::Client),
    /// The history files (no service running) or a test store.
    Local { store: Store, cfg: Config },
}

pub(crate) struct Listing {
    items: Vec<ItemSummary>,
    total: usize,
    paused: bool,
}

impl Backend {
    fn list(&mut self, query: &str) -> Result<Listing> {
        match self {
            Backend::Remote(c) => {
                let r = c.call_ok(&Request::List {
                    query: query.to_string(),
                    limit: LIST_LIMIT,
                })?;
                Ok(Listing {
                    items: r.items.unwrap_or_default(),
                    total: r.total,
                    paused: r.paused,
                })
            }
            Backend::Local { store, cfg } => Ok(Listing {
                items: store
                    .search(query)
                    .into_iter()
                    .take(LIST_LIMIT)
                    .map(|i| i.summary())
                    .collect(),
                total: store.len(),
                paused: cfg.paused,
            }),
        }
    }

    fn thumb(&mut self, id: u64) -> Option<Vec<u8>> {
        match self {
            Backend::Remote(c) => c
                .call_ok(&Request::Thumb { id })
                .ok()
                .and_then(|r| ipc::b64_decode(r.png.as_deref()?)),
            Backend::Local { store, .. } => store.thumbnail(id),
        }
    }

    fn pin(&mut self, id: u64, pinned: bool) -> Result<()> {
        match self {
            Backend::Remote(c) => c.call_ok(&Request::Pin { id, pinned }).map(|_| ()),
            Backend::Local { store, .. } => {
                store.set_pinned(id, pinned);
                store.save()
            }
        }
    }

    fn delete(&mut self, id: u64) -> Result<()> {
        match self {
            Backend::Remote(c) => c.call_ok(&Request::Delete { id }).map(|_| ()),
            Backend::Local { store, .. } => {
                store.delete(id);
                store.save()
            }
        }
    }

    /// Put the item on the clipboard (and paste it once `front` is back in
    /// front, when `paste`).
    fn paste(&mut self, id: u64, plain: bool, paste: bool, front: Option<&FrontApp>) -> Result<()> {
        let pid = front.map_or(0, |f| f.pid);
        match self {
            Backend::Remote(c) => {
                c.call_ok(&Request::Paste {
                    id,
                    plain,
                    paste,
                    pid,
                })?;
                if let Some(f) = front {
                    platform::activate(f.pid, f.window);
                }
                Ok(())
            }
            Backend::Local { store, .. } => {
                let content = store
                    .content(id)
                    .ok_or_else(|| anyhow!("item {id} is gone"))?;
                platform::write(&content, plain)?;
                store.touch(id, store::now_ms());
                store.save()?;
                if let Some(f) = front {
                    platform::activate(f.pid, f.window);
                }
                if paste {
                    crate::paste_when_frontmost(pid);
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Paste { id: u64, plain: bool },
}

pub(crate) struct PickerApp {
    pub(crate) backend: Backend,
    query: String,
    last_query: Option<String>,
    items: Vec<ItemSummary>,
    total: usize,
    paused: bool,
    selected: usize,
    scroll_to_selected: bool,
    thumbs: HashMap<u64, Option<TextureHandle>>,
    error: Option<String>,
    pub(crate) outcome: Option<Outcome>,
    was_focused: bool,
    /// Reference time for "2 min ago" (fixed in tests).
    pub(crate) now: Option<i64>,
    close: bool,
}

impl PickerApp {
    pub(crate) fn new(backend: Backend) -> Self {
        PickerApp {
            backend,
            query: String::new(),
            last_query: None,
            items: Vec::new(),
            total: 0,
            paused: false,
            selected: 0,
            scroll_to_selected: false,
            thumbs: HashMap::new(),
            error: None,
            outcome: None,
            was_focused: false,
            now: None,
            close: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, i: usize) {
        self.selected = i;
    }

    fn refresh(&mut self) {
        match self.backend.list(&self.query) {
            Ok(l) => {
                self.items = l.items;
                self.total = l.total;
                self.paused = l.paused;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.last_query = Some(self.query.clone());
    }

    fn texture(&mut self, ctx: &egui::Context, id: u64) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(&id) {
            return t.clone();
        }
        let tex = self.backend.thumb(id).and_then(|png| {
            let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .ok()?
                .to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(ctx.load_texture(format!("thumb-{id}"), ci, egui::TextureOptions::LINEAR))
        });
        self.thumbs.insert(id, tex.clone());
        tex
    }

    fn choose(&mut self, i: usize, plain: bool) {
        if let Some(it) = self.items.get(i) {
            self.outcome = Some(Outcome::Paste { id: it.id, plain });
            self.close = true;
        }
    }

    fn toggle_pin(&mut self, i: usize) {
        if let Some(it) = self.items.get(i).cloned() {
            if let Err(e) = self.backend.pin(it.id, !it.pinned) {
                self.error = Some(format!("{e:#}"));
            }
            self.refresh();
            // Keep the same item selected.
            if let Some(n) = self.items.iter().position(|x| x.id == it.id) {
                self.selected = n;
                self.scroll_to_selected = true;
            }
        }
    }

    fn delete(&mut self, i: usize) {
        if let Some(it) = self.items.get(i).cloned() {
            if let Err(e) = self.backend.delete(it.id) {
                self.error = Some(format!("{e:#}"));
            }
            self.thumbs.remove(&it.id);
            self.refresh();
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        let n = self.items.len();
        let cmd = Modifiers::COMMAND;
        let (down, up, enter, shift_enter, esc, pin, del, home, end) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(cmd, Key::P),
                i.consume_key(cmd, Key::Backspace) || i.consume_key(cmd, Key::Delete),
                i.consume_key(cmd, Key::ArrowUp),
                i.consume_key(cmd, Key::ArrowDown),
            )
        });
        if esc {
            self.close = true;
            return;
        }
        if n > 0 {
            if down {
                self.selected = (self.selected + 1).min(n - 1);
                self.scroll_to_selected = true;
            }
            if up {
                self.selected = self.selected.saturating_sub(1);
                self.scroll_to_selected = true;
            }
            if home {
                self.selected = 0;
                self.scroll_to_selected = true;
            }
            if end {
                self.selected = n - 1;
                self.scroll_to_selected = true;
            }
        }
        if enter || shift_enter {
            self.choose(self.selected, shift_enter);
            return;
        }
        if pin {
            self.toggle_pin(self.selected);
        }
        if del {
            self.delete(self.selected);
        }
        let digits = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        let empty = self.query.is_empty();
        for (d, k) in digits.into_iter().enumerate() {
            let (with_cmd, as_plain, bare) = ctx.input_mut(|i| {
                (
                    i.consume_key(cmd, k),
                    i.consume_key(cmd | Modifiers::SHIFT, k),
                    empty && i.consume_key(Modifiers::NONE, k),
                )
            });
            if with_cmd || as_plain || bare {
                if bare {
                    // Don't also type the digit into the search box.
                    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
                }
                self.choose(d, as_plain);
                return;
            }
        }
    }

    pub(crate) fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        let p = pal(ui);
        if self.last_query.as_deref() != Some(self.query.as_str()) {
            self.refresh();
        }
        let focused = ctx.input(|i| i.viewport().focused);
        if focused == Some(true) {
            self.was_focused = true;
        } else if focused == Some(false) && self.was_focused {
            self.close = true; // clicked elsewhere
        }
        self.keys(&ctx);
        if self.close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let frame = egui::Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, theme::mix(p.card_border, p.text3, 0.25)))
            .corner_radius(CornerRadius::same(14));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            self.header(ui, &p);
            hairline(ui, &p);
            let footer_h = 38.0;
            let list_h = (ui.available_height() - footer_h - 1.0).max(40.0);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), list_h),
                egui::Layout::top_down(Align::Min),
                |ui| self.list(ui, &p),
            );
            hairline(ui, &p);
            self.footer(ui, &p);
        });
    }

    fn header(&mut self, ui: &mut Ui, p: &Palette) {
        let h = 54.0;
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), h));
        let inner = rect.shrink2(vec2(16.0, 0.0));
        ui.painter().text(
            pos2(inner.left() + 2.0, inner.center().y),
            Align2::LEFT_CENTER,
            icon::MAGNIFYING_GLASS,
            theme::icon_font(18.0),
            p.text2,
        );
        // Right side: paused pill / count.
        let mut right = inner.right();
        if self.paused {
            let g =
                ui.painter()
                    .layout_no_wrap("Paused".into(), theme::font_semibold(11.5), p.warn);
            let w = g.size().x + 30.0;
            let pill =
                Rect::from_center_size(pos2(right - w / 2.0, inner.center().y), vec2(w, 22.0));
            ui.painter().rect_filled(
                pill,
                11.0,
                theme::mix(p.card, p.warn, if p.dark { 0.22 } else { 0.12 }),
            );
            ui.painter().text(
                pos2(pill.left() + 10.0, pill.center().y),
                Align2::LEFT_CENTER,
                glyph::PAUSE,
                theme::icon_font(11.0),
                p.warn,
            );
            ui.painter().galley(
                pos2(pill.left() + 22.0, pill.center().y - g.size().y / 2.0),
                g,
                p.warn,
            );
            right = pill.left() - 8.0;
        } else if self.total > 0 {
            let t = if self.query.trim().is_empty() {
                format!("{} items", self.total)
            } else {
                format!("{} of {}", self.items.len(), self.total)
            };
            let g = ui.painter().layout_no_wrap(t, theme::font(12.0), p.text3);
            let w = g.size().x;
            ui.painter().galley(
                pos2(right - w, inner.center().y - g.size().y / 2.0),
                g,
                p.text3,
            );
            right -= w + 10.0;
        }
        let edit_rect = Rect::from_min_max(
            pos2(inner.left() + 30.0, inner.center().y - 9.0),
            pos2(right, inner.center().y + 19.0),
        );
        let te = egui::TextEdit::singleline(&mut self.query)
            .hint_text(RichText::new("Search clipboard history").color(p.text3))
            .font(theme::font(16.0))
            .text_color(p.text)
            .frame(egui::Frame::NONE)
            .margin(vec2(0.0, 4.0))
            .desired_width(edit_rect.width());
        let r = ui.put(edit_rect, te);
        ui.allocate_rect(rect, Sense::hover());
        r.request_focus();
        if r.changed() {
            self.selected = 0;
            self.scroll_to_selected = true;
        }
    }

    fn list(&mut self, ui: &mut Ui, p: &Palette) {
        if let Some(e) = self.error.clone() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                pm_ui::widgets::error_label(ui, &e);
            });
        }
        if self.items.is_empty() {
            self.empty_state(ui, p);
            return;
        }
        let ctx = ui.ctx().clone();
        let now = self.now.unwrap_or_else(store::now_ms);
        let has_pinned = self.items.iter().any(|i| i.pinned);
        let show_sections = has_pinned && self.query.trim().is_empty();
        let mut clicked: Option<(usize, bool)> = None;
        let mut pin_click = None;
        let mut del_click = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                let items = self.items.clone();
                for (i, it) in items.iter().enumerate() {
                    if show_sections && (i == 0 || (items[i - 1].pinned && !it.pinned)) {
                        section_label(ui, p, if it.pinned { "Pinned" } else { "Recent" }, i > 0);
                    }
                    let tex = if it.kind == Kind::Image && it.has_thumbnail {
                        self.texture(&ctx, it.id)
                    } else {
                        None
                    };
                    let r = row(ui, p, it, i, i == self.selected, tex.as_ref(), now);
                    if r.clicked {
                        clicked = Some((i, ctx.input(|i| i.modifiers.shift)));
                    }
                    if r.pin {
                        pin_click = Some(i);
                    }
                    if r.delete {
                        del_click = Some(i);
                    }
                    if r.hovered && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        self.selected = i;
                    }
                    if i == self.selected && self.scroll_to_selected {
                        ui.scroll_to_rect(r.rect.expand(6.0), None);
                        self.scroll_to_selected = false;
                    }
                }
                ui.add_space(6.0);
            });
        if let Some(i) = pin_click {
            self.toggle_pin(i);
        } else if let Some(i) = del_click {
            self.delete(i);
        } else if let Some((i, plain)) = clicked {
            self.choose(i, plain);
        }
    }

    fn empty_state(&self, ui: &mut Ui, p: &Palette) {
        let rect = ui.available_rect_before_wrap();
        let c = rect.center() - vec2(0.0, 24.0);
        let tile = Rect::from_center_size(c - vec2(0.0, 26.0), vec2(48.0, 48.0));
        ui.painter().rect_filled(tile, 12.0, p.control);
        ui.painter().text(
            tile.center(),
            Align2::CENTER_CENTER,
            icon::CLIPBOARD_TEXT,
            theme::icon_font(26.0),
            p.text3,
        );
        let (title, sub) = if self.query.trim().is_empty() {
            (
                "Nothing copied yet".to_string(),
                if self.paused {
                    "History is paused.".to_string()
                } else {
                    "Text, images and files you copy will show up here.".to_string()
                },
            )
        } else {
            (
                "No matches".to_string(),
                format!("Nothing in your history contains “{}”.", self.query.trim()),
            )
        };
        ui.painter().text(
            c + vec2(0.0, 18.0),
            Align2::CENTER_CENTER,
            title,
            theme::font_semibold(15.0),
            p.text,
        );
        ui.painter().text(
            c + vec2(0.0, 40.0),
            Align2::CENTER_CENTER,
            sub,
            theme::font(12.5),
            p.text2,
        );
        ui.allocate_rect(rect, Sense::hover());
    }

    fn footer(&self, ui: &mut Ui, p: &Palette) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 37.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let mac = cfg!(target_os = "macos");
        let cmd = if mac { "⌘" } else { "Ctrl+" };
        let hints: Vec<(String, &str)> = vec![
            ("↩".into(), "Paste"),
            ("⇧↩".into(), "Plain text"),
            (format!("{cmd}P"), "Pin"),
            (format!("{cmd}⌫"), "Delete"),
            ("esc".into(), "Close"),
        ];
        let mut x = rect.left() + 16.0;
        let y = rect.center().y;
        for (k, label) in hints {
            let kg = painter.layout_no_wrap(k, theme::font_semibold(11.0), p.text2);
            let kw = kg.size().x + 10.0;
            let cap = Rect::from_min_size(pos2(x, y - 9.0), vec2(kw.max(20.0), 18.0));
            painter.rect(
                cap,
                5.0,
                p.control,
                Stroke::new(1.0, p.card_border),
                StrokeKind::Inside,
            );
            painter.galley(cap.center() - kg.size() / 2.0, kg, p.text2);
            x = cap.right() + 6.0;
            let lg = painter.layout_no_wrap(label.into(), theme::font(12.0), p.text2);
            let lw = lg.size().x;
            painter.galley(pos2(x, y - lg.size().y / 2.0), lg, p.text2);
            x += lw + 16.0;
        }
    }
}

fn hairline(ui: &mut Ui, p: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, p.separator));
}

fn section_label(ui: &mut Ui, p: &Palette, text: &str, gap: bool) {
    if gap {
        ui.add_space(6.0);
    }
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        pos2(r.left() + 20.0, r.center().y + 2.0),
        Align2::LEFT_CENTER,
        text.to_uppercase(),
        theme::font_semibold(10.5),
        p.text3,
    );
}

/// Accent per item type.
fn kind_style(kind: Kind) -> (&'static str, Color32, &'static str) {
    match kind {
        Kind::Text => (icon::TEXT_T, Color32::from_rgb(0x0A, 0x84, 0xFF), "Text"),
        Kind::RichText => (
            glyph::TEXT_AA,
            Color32::from_rgb(0xAF, 0x52, 0xDE),
            "Rich text",
        ),
        Kind::Image => (glyph::IMAGE, Color32::from_rgb(0x30, 0xB0, 0x5A), "Image"),
        Kind::Files => (glyph::FILES, Color32::from_rgb(0xFF, 0x95, 0x00), "Files"),
    }
}

struct RowResult {
    rect: Rect,
    clicked: bool,
    hovered: bool,
    pin: bool,
    delete: bool,
}

fn row(
    ui: &mut Ui,
    p: &Palette,
    it: &ItemSummary,
    index: usize,
    selected: bool,
    tex: Option<&TextureHandle>,
    now: i64,
) -> RowResult {
    let is_image = tex.is_some();
    let h = if is_image { 76.0 } else { 58.0 };
    let full = ui.available_width();
    let (outer, resp) = ui.allocate_exact_size(vec2(full, h), Sense::click());
    let rect = outer.shrink2(vec2(8.0, 2.0));
    let hovered = resp.hovered();
    let painter = ui.painter_at(outer);
    if selected {
        painter.rect_filled(rect, 9.0, p.accent_soft);
    } else if hovered {
        painter.rect_filled(rect, 9.0, theme::mix(p.card, p.control_hover, 0.7));
    }
    let (glyph_, tint, kind_name) = kind_style(it.kind);
    // Left: type tile or thumbnail.
    let left = rect.left() + 12.0;
    let content_left;
    if let Some(t) = tex {
        let ts = t.size_vec2();
        let max = vec2(104.0, h - 18.0);
        let scale = (max.x / ts.x).min(max.y / ts.y).min(1.0);
        let size = ts * scale;
        let slot = Rect::from_min_size(pos2(left, rect.center().y - max.y / 2.0), max);
        painter.rect_filled(slot, 7.0, p.control);
        let img_rect = Rect::from_center_size(slot.center(), size);
        egui::Image::new((t.id(), size))
            .corner_radius(6.0)
            .paint_at(ui, img_rect);
        painter.rect_stroke(
            slot,
            7.0,
            Stroke::new(1.0, p.card_border),
            StrokeKind::Inside,
        );
        content_left = slot.right() + 12.0;
    } else {
        let tile = Rect::from_min_size(pos2(left, rect.center().y - 15.0), vec2(30.0, 30.0));
        let fill = theme::mix(p.card, tint, if p.dark { 0.24 } else { 0.13 });
        painter.rect_filled(tile, 8.0, fill);
        painter.text(
            tile.center(),
            Align2::CENTER_CENTER,
            glyph_,
            theme::icon_font(16.0),
            if p.dark {
                theme::mix(tint, Color32::WHITE, 0.25)
            } else {
                tint
            },
        );
        content_left = tile.right() + 12.0;
    }

    // Right: shortcut / actions.
    let mut right = rect.right() - 12.0;
    let show_actions = hovered || selected;
    let mut pin_hit = false;
    let mut del_hit = false;
    if show_actions {
        for (g, is_pin) in [(icon::TRASH, false), (glyph::PUSH_PIN, true)] {
            let b = Rect::from_center_size(pos2(right - 13.0, rect.center().y), vec2(26.0, 26.0));
            let r = ui.interact(b, ui.scope_id().with((it.id, is_pin)), Sense::click());
            let active = is_pin && it.pinned;
            if r.hovered() {
                painter.rect_filled(b, 6.0, theme::mix(p.card, p.control_active, 0.8));
            }
            let c = if active {
                p.accent
            } else if r.hovered() {
                p.text
            } else {
                p.text2
            };
            painter.text(
                b.center(),
                Align2::CENTER_CENTER,
                g,
                theme::icon_font(15.0),
                c,
            );
            let r = r.on_hover_text(match (is_pin, it.pinned) {
                (true, true) => "Unpin",
                (true, false) => "Pin",
                _ => "Delete",
            });
            if r.clicked() {
                if is_pin {
                    pin_hit = true;
                } else {
                    del_hit = true;
                }
            }
            right = b.left() - 2.0;
        }
        right -= 4.0;
    } else if it.pinned {
        painter.text(
            pos2(right - 8.0, rect.center().y),
            Align2::CENTER_CENTER,
            glyph::PUSH_PIN,
            theme::icon_font(13.0),
            p.accent,
        );
        right -= 22.0;
    }
    if index < 9 {
        let k = if cfg!(target_os = "macos") {
            format!("⌘{}", index + 1)
        } else {
            format!("Ctrl+{}", index + 1)
        };
        let g = painter.layout_no_wrap(k, theme::font(11.5), p.text3);
        let w = g.size().x;
        painter.galley(
            pos2(right - w, rect.center().y - g.size().y / 2.0),
            g,
            p.text3,
        );
        right -= w + 12.0;
    }

    // Middle: preview + meta.
    let width = (right - content_left).max(40.0);
    let max_rows = if is_image { 1 } else { 2 };
    let preview = if it.kind == Kind::Text || it.kind == Kind::RichText {
        // Collapse runs of blank lines / indentation for the two-line preview.
        let mut s = String::new();
        for line in it.preview.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(line);
        }
        s
    } else if it.kind == Kind::Image {
        "Image".to_string()
    } else {
        it.preview.clone()
    };
    let mut job = egui::text::LayoutJob::single_section(
        preview,
        egui::TextFormat::simple(theme::font(13.5), p.text),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let meta = meta_line(it, kind_name, now);
    let mg = painter.layout_no_wrap(meta, theme::font(11.5), p.text2);
    let total_h = g.size().y + 3.0 + mg.size().y;
    let top = rect.center().y - total_h / 2.0;
    painter.galley(pos2(content_left, top), g.clone(), p.text);
    painter.galley(pos2(content_left, top + g.size().y + 3.0), mg, p.text2);

    RowResult {
        rect: outer,
        clicked: resp.clicked() && !pin_hit && !del_hit,
        hovered,
        pin: pin_hit,
        delete: del_hit,
    }
}

fn meta_line(it: &ItemSummary, kind_name: &str, now: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    match it.kind {
        Kind::Files => parts.push(if it.file_count == 1 {
            "1 file".into()
        } else {
            format!("{} files", it.file_count)
        }),
        Kind::Image => parts.push(match it.image_size {
            Some((w, h)) => format!("{w} × {h}"),
            None => kind_name.into(),
        }),
        Kind::Text | Kind::RichText => {
            if it.kind == Kind::RichText {
                parts.push("Rich text".into());
            }
            parts.push(format!(
                "{} character{}",
                group(it.chars),
                if it.chars == 1 { "" } else { "s" }
            ));
        }
    }
    if let Some(s) = &it.source_name {
        parts.insert(0, s.clone());
    }
    parts.push(ago(now - it.last_used));
    parts.join("  ·  ")
}

fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "just now", "4 min ago", "2 h ago", "yesterday", "5 days ago", ...
pub(crate) fn ago(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..=44 => "just now".into(),
        45..=3599 => format!("{} min ago", (s / 60).max(1)),
        3600..=86_399 => format!("{} h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ if s < 14 * 86_400 => format!("{} days ago", s / 86_400),
        _ if s < 60 * 86_400 => format!("{} weeks ago", s / (7 * 86_400)),
        _ => format!("{} months ago", s / (30 * 86_400)),
    }
}

/// Where to put the window: next to the pointer, kept on its screen.
fn position(cfg: &Config) -> Option<[f32; 2]> {
    let (pt, screen) = platform::pointer_and_screen()?;
    let [sx, sy, sw, sh] = screen;
    let margin = 12.0;
    let (x, y) = match cfg.picker_position {
        PickerPosition::Center => (sx + (sw - WIDTH) / 2.0, sy + (sh - HEIGHT) / 3.0),
        PickerPosition::Mouse => (pt[0] - 40.0, pt[1] + 14.0),
    };
    let x = x.clamp(sx + margin, (sx + sw - WIDTH - margin).max(sx + margin));
    let y = y.clamp(sy + 30.0, (sy + sh - HEIGHT - margin).max(sy + 30.0));
    Some([x, y])
}

impl eframe::App for PickerApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent around the rounded panel
    }
}

/// Open the picker and block until it closes; then paste the chosen item
/// into the app that was in front. Must be called on the main thread of
/// its own process (e.g. `powermac clipboard`).
///
/// `data_dir` is the history directory (see [`crate::default_data_dir`]);
/// `config` is the clipboard history config (paste behaviour, position, and
/// for a standalone picker whether the history is persisted at all).
pub fn run_picker(data_dir: PathBuf, config: Config) -> Result<()> {
    // Who to paste into: whatever was in front before our window appears.
    let front = platform::frontmost().filter(|f| f.pid != std::process::id() as i32);
    let backend = match ipc::Client::connect(&data_dir) {
        Some(c) => Backend::Remote(c),
        None => {
            log::info!(
                "clipboard picker: no history service running; reading {}",
                data_dir.display()
            );
            Backend::Local {
                store: Store::for_config(&data_dir, &config)?,
                cfg: config.clone(),
            }
        }
    };
    let mut vb = egui::ViewportBuilder::default()
        .with_title("Clipboard History")
        .with_app_id("com.powermac.clipboard")
        .with_inner_size([WIDTH, HEIGHT])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(true);
    if let Some(pos) = position(&config) {
        vb = vb.with_position(pos);
    }
    let options = eframe::NativeOptions {
        viewport: vb,
        centered: true,
        ..Default::default()
    };
    type Slot = std::rc::Rc<std::cell::RefCell<Option<(Option<Outcome>, Backend)>>>;
    let result: Slot = Default::default();
    let r2 = result.clone();
    struct Wrapper {
        app: Option<PickerApp>,
        out: Slot,
    }
    impl eframe::App for Wrapper {
        fn ui(&mut self, ui: &mut Ui, f: &mut eframe::Frame) {
            if let Some(a) = &mut self.app {
                a.ui(ui, f);
            }
        }
        fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
            self.app.as_ref().map_or([0.0; 4], |a| a.clear_color(v))
        }
        fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
            if let Some(a) = self.app.take() {
                *self.out.borrow_mut() = Some((a.outcome, a.backend));
            }
        }
    }
    eframe::run_native(
        "Clipboard History",
        options,
        Box::new(move |_cc| {
            become_accessory_and_activate();
            Ok(Box::new(Wrapper {
                app: Some(PickerApp::new(backend)),
                out: r2,
            }))
        }),
    )
    .map_err(|e| anyhow!("clipboard picker: {e}"))?;

    let taken = result.borrow_mut().take();
    if let Some((Some(Outcome::Paste { id, plain }), mut backend)) = taken {
        backend.paste(id, plain, config.paste_on_select, front.as_ref())?;
    } else if let Some(f) = &front {
        // Closed without choosing: give focus back.
        platform::activate(f.pid, f.window);
    }
    Ok(())
}

/// No Dock icon; take focus so typing goes to the search box.
fn become_accessory_and_activate() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(90_000), "1 min ago");
        assert_eq!(ago(3 * 3_600_000), "3 h ago");
        assert_eq!(ago(30 * 3_600_000), "yesterday");
        assert_eq!(ago(3 * 86_400_000), "3 days ago");
        assert_eq!(ago(20 * 86_400_000), "2 weeks ago");
        assert_eq!(group(1234567), "1,234,567");
        assert_eq!(group(12), "12");
    }
}
