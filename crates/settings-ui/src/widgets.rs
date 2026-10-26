//! Reusable widgets in the PowerMac style: cards, setting rows (label +
//! description on the left, control on the right), the animated switch,
//! segmented controls, keycap chips, color fields and messages.

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Id, Layout, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder,
};

use crate::theme::{self, icon, pal, Palette};

// ---------------------------------------------------------------------------
// Search highlight / scroll anchors (set by the app for one frame at a time)

fn highlight_id() -> Id {
    Id::unique("pm-row-highlight")
}
fn anchor_id() -> Id {
    Id::unique("pm-card-anchor")
}
fn rows_id() -> Id {
    Id::unique("pm-card-rows")
}

/// Rows whose label contains `term` (lowercase) get a soft accent background.
pub fn set_highlight(ctx: &egui::Context, term: Option<String>) {
    ctx.data_mut(|d| match term {
        Some(t) => {
            d.insert_temp(highlight_id(), t);
        }
        None => d.remove::<String>(highlight_id()),
    });
}

/// Scroll the card titled `title` into view on the next frame it's drawn.
pub fn set_anchor(ctx: &egui::Context, title: Option<String>) {
    ctx.data_mut(|d| match title {
        Some(t) => {
            d.insert_temp(anchor_id(), t);
        }
        None => d.remove::<String>(anchor_id()),
    });
}

fn collect_id() -> Id {
    Id::unique("pm-search-collect")
}
fn section_id() -> Id {
    Id::unique("pm-search-section")
}

/// One searchable setting: (section, label, description).
pub type IndexEntry = (String, String, String);

/// Start recording the rows drawn (for the search index).
pub fn begin_collect(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(collect_id(), Vec::<IndexEntry>::new()));
}

pub fn end_collect(ctx: &egui::Context) -> Vec<IndexEntry> {
    ctx.data_mut(|d| d.remove_temp::<Vec<IndexEntry>>(collect_id()))
        .unwrap_or_default()
}

fn collect(ui: &Ui, label: &str, desc: Option<&str>) {
    ui.data_mut(|d| {
        let section = d.get_temp::<String>(section_id()).unwrap_or_default();
        if d.get_temp::<Vec<IndexEntry>>(collect_id()).is_some() {
            let v = d.get_temp_mut_or_default::<Vec<IndexEntry>>(collect_id());
            v.push((
                section,
                label.to_string(),
                desc.unwrap_or_default().to_string(),
            ));
        }
    });
}

/// Whether a search highlight is active.
pub fn has_highlight(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<String>(highlight_id())).is_some()
}

/// The next row or block draws a separator above it.
pub fn continue_rows(ui: &mut Ui) {
    ui.data_mut(|d| d.insert_temp(rows_id(), 1usize));
}

/// Restart the row separators (for a column of rows inside a card).
pub fn reset_rows(ui: &mut Ui) {
    ui.data_mut(|d| d.insert_temp(rows_id(), 0usize));
}

fn highlighted(ui: &Ui, label: &str) -> bool {
    match ui.data(|d| d.get_temp::<String>(highlight_id())) {
        Some(t) if !t.is_empty() => label.to_lowercase().contains(&t),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Cards and rows

/// A section: a title above a rounded card holding rows.
pub fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) -> Response {
    card_with(ui, title, None, add)
}

/// [`card`] with a secondary line under the title.
pub fn card_with(
    ui: &mut Ui,
    title: &str,
    subtitle: Option<&str>,
    add: impl FnOnce(&mut Ui),
) -> Response {
    let p = pal(ui);
    ui.add_space(18.0);
    let head = ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        if !title.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(title)
                        .font(theme::font_semibold(15.0))
                        .color(p.text),
                );
            });
        }
        if let Some(s) = subtitle {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(s).size(12.0).color(p.text2)).wrap());
            });
        }
    });
    ui.add_space(6.0);
    let anchored = ui.is_visible()
        && ui
            .data(|d| d.get_temp::<String>(anchor_id()))
            .is_some_and(|a| a == title);
    ui.data_mut(|d| d.insert_temp(section_id(), title.to_string()));
    ui.data_mut(|d| d.insert_temp(rows_id(), 0usize));
    let r = card_frame(&p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    if anchored {
        let rect = head.response.rect.union(r.response.rect);
        ui.scroll_to_rect(rect.expand(12.0), Some(Align::TOP));
        ui.painter().rect_stroke(
            r.response.rect,
            12.0,
            Stroke::new(2.0, p.accent),
            StrokeKind::Outside,
        );
        set_anchor(ui.ctx(), None);
    }
    r.response
}

pub fn card_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_border))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 1],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(if p.dark { 40 } else { 8 }),
        })
}

/// A thin line between rows of a card (skipped before the first row).
fn row_separator(ui: &mut Ui) {
    let n = ui.data(|d| d.get_temp::<usize>(rows_id())).unwrap_or(1);
    ui.data_mut(|d| d.insert_temp(rows_id(), n + 1));
    if n > 0 {
        divider(ui);
    }
}

/// A full-width hairline.
pub fn divider(ui: &mut Ui) {
    let p = pal(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, p.separator));
}

/// Free-form content inside a card, separated from the rows around it.
pub fn block(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    row_separator(ui);
    ui.add_space(8.0);
    add(ui);
    ui.add_space(8.0);
}

#[derive(Clone, Copy, Default)]
struct RowSize {
    control: egui::Vec2,
    text: f32,
}

/// A setting row: `label` and an optional description on the left, the
/// control right-aligned. Returns the control's result.
pub fn row<R>(
    ui: &mut Ui,
    label: &str,
    desc: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    row_impl(ui, label, desc, None, control).0
}

/// [`row`] that also returns the label's response (clickable).
pub fn icon_row_plain<R>(
    ui: &mut Ui,
    label: &str,
    desc: &str,
    control: impl FnOnce(&mut Ui) -> R,
) -> (R, Response) {
    row_impl(ui, label, Some(desc), None, control)
}

/// [`row`] with a module icon tile before the label; returns the control's
/// result and the label's response (clickable).
pub fn icon_row<R>(
    ui: &mut Ui,
    glyph: &str,
    tint: Color32,
    label: &str,
    desc: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> (R, Response) {
    row_impl(ui, label, desc, Some((glyph, tint)), control)
}

fn row_impl<R>(
    ui: &mut Ui,
    label: &str,
    desc: Option<&str>,
    icon: Option<(&str, Color32)>,
    control: impl FnOnce(&mut Ui) -> R,
) -> (R, Response) {
    row_separator(ui);
    collect(ui, label, desc);
    let p = pal(ui);
    // Cards don't get scope ids of their own: key rows by section + label.
    let section = ui
        .data(|d| d.get_temp::<String>(section_id()))
        .unwrap_or_default();
    let key = ui.scope_id().with(("row", section, label));
    let mut size = ui.data(|d| d.get_temp::<RowSize>(key)).unwrap_or(RowSize {
        control: vec2(40.0, 24.0),
        text: 18.0,
    });
    const PAD: f32 = 9.0;
    const GAP: f32 = 20.0;
    let w = ui.available_width();
    let h = size.control.y.max(size.text).max(22.0) + 2.0 * PAD;
    let rect = Rect::from_min_size(ui.available_rect_before_wrap().min, vec2(w, h));
    let resp = ui.allocate_rect(rect, Sense::hover());
    if highlighted(ui, label) {
        ui.painter()
            .rect_filled(rect.expand2(vec2(10.0, 0.0)), 8.0, p.accent_soft);
    }
    // The control starts where last frame's control did (right-aligned) but
    // may use more room; a changed size re-lays the row out.
    let cap = (w - 90.0).max(w * 0.6);
    let cw = size.control.x.min(cap);
    let crect = Rect::from_min_size(
        pos2(rect.right() - cw, rect.center().y - size.control.y / 2.0),
        vec2(cap.max(cw), size.control.y),
    );
    let mut cui = ui.new_child(
        UiBuilder::new()
            .id_salt(key.with("c"))
            .max_rect(crect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let out = control(&mut cui);
    let used = cui.min_rect();
    let icon_w = if icon.is_some() { 38.0 } else { 0.0 };
    if let Some((g, tint)) = icon {
        let ir =
            Rect::from_center_size(pos2(rect.left() + 13.0, rect.center().y), vec2(26.0, 26.0));
        paint_icon_tile(ui.painter(), ir, g, tint);
    }
    let text_w = (w - used.width() - GAP - icon_w).max(80.0);
    let trect = Rect::from_min_size(
        pos2(rect.left() + icon_w, rect.center().y - size.text / 2.0),
        vec2(text_w, size.text.max(1.0)),
    );
    let mut tui = ui.new_child(
        UiBuilder::new()
            .id_salt(key.with("t"))
            .max_rect(trect)
            .layout(Layout::top_down(Align::Min)),
    );
    tui.spacing_mut().item_spacing.y = 2.0;
    let enabled = tui.is_enabled();
    let lc = if enabled { p.text } else { p.text3 };
    tui.add(egui::Label::new(RichText::new(label).color(lc)).wrap());
    if let Some(d) = desc {
        tui.add(
            egui::Label::new(RichText::new(d).size(12.0).color(if enabled {
                p.text2
            } else {
                p.text3
            }))
            .wrap(),
        );
    }
    let th = tui.min_rect().height();
    let new = RowSize {
        control: used.size(),
        text: th,
    };
    if (new.control - size.control).length() > 0.5 || (new.text - size.text).abs() > 0.5 {
        size = new;
        ui.data_mut(|d| d.insert_temp(key, size));
        ui.ctx().request_discard("row layout");
    }
    let text_resp = ui.interact(tui.min_rect(), key.with("label"), Sense::click());
    let _ = resp;
    (out, text_resp)
}

/// A row with a switch. Clicking the label toggles too.
pub fn toggle_row(ui: &mut Ui, label: &str, desc: Option<&str>, on: &mut bool) -> Response {
    let mut changed = false;
    let (mut r, text) = row_impl(ui, label, desc, None, |ui| toggle_labeled(ui, on, label));
    let text = text.on_hover_cursor(egui::CursorIcon::PointingHand);
    if text.clicked() && ui.is_enabled() {
        *on = !*on;
        changed = true;
    }
    if changed {
        r.mark_changed();
    }
    r
}

// ---------------------------------------------------------------------------
// Controls

/// A macOS-style on/off switch (`label` names it for accessibility).
pub fn toggle_labeled(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = pal(ui);
    let size = vec2(40.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let label = label.to_string();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, label.clone())
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.16);
        let track = theme::mix(p.off, p.accent, t);
        let track = if ui.is_enabled() {
            track
        } else {
            theme::mix(track, p.card, 0.5)
        };
        let r = rect.height() / 2.0;
        ui.painter().rect_filled(rect, r, track);
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
        let c = pos2(x, rect.center().y);
        ui.painter()
            .circle_filled(c + vec2(0.0, 0.8), r - 2.0, Color32::from_black_alpha(40));
        ui.painter().circle_filled(c, r - 2.0, Color32::WHITE);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                r + 2.0,
                Stroke::new(2.0, p.accent.gamma_multiply(0.5)),
                StrokeKind::Outside,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A segmented control for a small enum.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)]) -> Response {
    let p = pal(ui);
    let font = theme::font(12.5);
    let pad = 12.0;
    let h = 26.0;
    let widths: Vec<f32> = items
        .iter()
        .map(|(_, n)| {
            ui.fonts_mut(|f| f.layout_no_wrap(n.to_string(), font.clone(), p.text))
                .size()
                .x
                + 2.0 * pad
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect(
        rect,
        8.0,
        p.control,
        Stroke::new(1.0, p.control_border.gamma_multiply(0.6)),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 2.0;
    for (i, ((v, name), w)) in items.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(*w, h - 4.0));
        let id = resp.id.with(i);
        let ir = ui.interact(r, id, Sense::click());
        let sel = *v == *value;
        if sel {
            let fill = if p.dark {
                Color32::from_rgb(0x63, 0x63, 0x68)
            } else {
                Color32::WHITE
            };
            painter.rect_filled(
                r.translate(vec2(0.0, 0.5)),
                6.0,
                Color32::from_black_alpha(if p.dark { 60 } else { 22 }),
            );
            painter.rect_filled(r, 6.0, fill);
        } else if ir.hovered() && ui.is_enabled() {
            painter.rect_filled(r, 6.0, p.control_hover);
        }
        let tc = if !ui.is_enabled() {
            p.text3
        } else if sel {
            p.text
        } else {
            p.text2
        };
        let f = if sel {
            theme::font_semibold(12.5)
        } else {
            font.clone()
        };
        painter.text(r.center(), Align2::CENTER_CENTER, *name, f, tc);
        if ir.clicked() && !sel {
            *value = *v;
            resp.mark_changed();
        }
        x += w;
    }
    resp
}

/// A dropdown over a fixed list of values.
pub fn combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    items: &[(T, &str)],
) -> Response {
    let current = items
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, n)| *n)
        .unwrap_or("?");
    let r = egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(220.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(220.0);
            for (v, n) in items {
                ui.selectable_value(value, *v, *n);
            }
        });
    r.response
}

/// A slider plus a fixed-width value field, so sliders line up in a card.
pub fn slider<N: egui::emath::Numeric>(
    ui: &mut Ui,
    v: &mut N,
    range: std::ops::RangeInclusive<N>,
    suffix: &str,
) -> Response {
    ui.spacing_mut().slider_width = 150.0;
    let mut r = ui.add(egui::Slider::new(v, range.clone()).show_value(false));
    let d = ui.add_sized(
        vec2(72.0, 24.0),
        egui::DragValue::new(v).range(range).suffix(suffix),
    );
    if d.changed() {
        r.mark_changed();
    }
    r
}

pub fn percent_slider(ui: &mut Ui, v: &mut f64) -> Response {
    let mut pct = (*v * 100.0).round().clamp(0.0, 100.0) as u32;
    let r = slider(ui, &mut pct, 0..=100, " %");
    if r.changed() {
        *v = f64::from(pct) / 100.0;
    }
    r
}

/// A button with just an icon (and a tooltip).
pub fn icon_button(ui: &mut Ui, glyph: &str, tip: &str) -> Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(theme::icon_text(glyph).size(15.0).color(p.text))
            .min_size(vec2(28.0, 28.0)),
    )
    .on_hover_text(tip)
}

/// A button with an icon and a label.
pub fn icon_text_button(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    let p = pal(ui);
    let mut job = egui::text::LayoutJob::default();
    let tc = if ui.is_enabled() { p.text } else { p.text3 };
    job.append(
        glyph,
        0.0,
        egui::TextFormat::simple(theme::icon_font(14.0), tc),
    );
    job.append(text, 6.0, egui::TextFormat::simple(theme::font(13.0), tc));
    ui.add(egui::Button::new(job).min_size(vec2(28.0, 28.0)))
}

/// The accent-filled button for the main action.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(
            RichText::new(text)
                .font(theme::font_semibold(13.0))
                .color(p.on_accent),
        )
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .min_size(vec2(28.0, 28.0)),
    )
}

pub fn primary_icon_button(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    let p = pal(ui);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        glyph,
        0.0,
        egui::TextFormat::simple(theme::icon_font(14.0), p.on_accent),
    );
    job.append(
        text,
        6.0,
        egui::TextFormat::simple(theme::font_semibold(13.0), p.on_accent),
    );
    ui.add(
        egui::Button::new(job)
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .min_size(vec2(28.0, 28.0)),
    )
}

/// A rounded tinted tile with a white icon (module icons).
pub fn icon_tile(ui: &mut Ui, glyph: &str, tint: Color32, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_icon_tile(ui.painter(), rect, glyph, tint);
    resp
}

pub fn paint_icon_tile(painter: &egui::Painter, rect: Rect, glyph: &str, tint: Color32) {
    let size = rect.width();
    let cr = (size * 0.24).round();
    painter.rect_filled(rect, cr, tint);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, theme::mix(tint, Color32::BLACK, 0.12)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        theme::icon_font(size * 0.58),
        Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// Text

pub fn error_label(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    message(ui, icon::WARNING_CIRCLE, p.error, text);
}

pub fn warn_label(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    message(ui, icon::WARNING, p.warn, text);
}

pub fn ok_label(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    message(ui, icon::CHECK_CIRCLE, p.ok, text);
}

fn message(ui: &mut Ui, glyph: &str, color: Color32, text: &str) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    job.append(
        glyph,
        0.0,
        egui::TextFormat::simple(theme::icon_font(13.0), color),
    );
    job.append(
        text,
        5.0,
        egui::TextFormat::simple(theme::font(12.5), color),
    );
    ui.add(egui::Label::new(job).wrap());
}

/// Secondary explanatory text.
pub fn hint(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text2)).wrap());
}

// ---------------------------------------------------------------------------
// Keycaps

/// Pretty name of one key of a shortcut.
fn cap_text(k: &str) -> String {
    let mac = cfg!(target_os = "macos");
    match k.trim() {
        "Win" | "Cmd" | "Meta" if mac => "⌘ Cmd".into(),
        "Alt" | "Option" if mac => "⌥ Option".into(),
        "Shift" => "⇧ Shift".into(),
        "Ctrl" | "Control" if mac => "⌃ Ctrl".into(),
        other => other.to_string(),
    }
}

/// Draw a shortcut like `Ctrl+Shift+V` as keycap chips. Returns the area.
pub fn keycaps(ui: &mut Ui, text: &str) -> Response {
    let p = pal(ui);
    let parts: Vec<&str> = if text.trim().is_empty() {
        vec![]
    } else {
        split_keys(text)
    };
    let font = theme::font_semibold(12.0);
    let galleys: Vec<_> = parts
        .iter()
        .map(|k| ui.fonts_mut(|f| f.layout_no_wrap(cap_text(k), font.clone(), p.text)))
        .collect();
    let gap = 4.0;
    let h = 24.0;
    let w: f32 = if galleys.is_empty() {
        ui.fonts_mut(|f| f.layout_no_wrap("Not set".into(), theme::font(12.5), p.text3))
            .size()
            .x
    } else {
        galleys
            .iter()
            .map(|g| g.size().x.max(10.0) + 14.0)
            .sum::<f32>()
            + gap * (galleys.len() as f32 - 1.0)
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if galleys.is_empty() {
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            "Not set",
            theme::font(12.5),
            p.text3,
        );
        return resp;
    }
    let mut x = rect.left();
    let cap_fill = if p.dark {
        Color32::from_rgb(0x48, 0x48, 0x4C)
    } else {
        Color32::WHITE
    };
    let edge = if p.dark {
        Color32::from_rgb(0x2C, 0x2C, 0x2F)
    } else {
        Color32::from_rgb(0xC8, 0xC8, 0xCE)
    };
    for g in galleys {
        let cw = g.size().x.max(10.0) + 14.0;
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(cw, h - 1.0));
        ui.painter()
            .rect_filled(r.translate(vec2(0.0, 1.5)), 5.0, edge);
        ui.painter()
            .rect(r, 5.0, cap_fill, Stroke::new(1.0, edge), StrokeKind::Inside);
        let tc = if ui.is_enabled() { p.text } else { p.text3 };
        ui.painter()
            .galley_with_override_text_color(r.center() - g.size() / 2.0, g, tc);
        x += cw + gap;
    }
    resp
}

/// `Ctrl+Shift+=` → [Ctrl, Shift, =]; a trailing `+` key is kept.
fn split_keys(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' && i > start {
            out.push(s[start..i].trim());
            start = i + 1;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(s[start..].trim());
    }
    out.retain(|k| !k.is_empty());
    out
}

// ---------------------------------------------------------------------------
// Colors

pub fn hex_to_color32(s: &str, fallback: Color32) -> Color32 {
    fancyzones::config::parse_hex_color(s).map(|(r, g, b)| Color32::from_rgb(r, g, b)).unwrap_or(fallback)
}

pub fn color32_to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// A rounded color swatch that opens a picker; returns true when changed.
pub fn swatch_picker(ui: &mut Ui, c: &mut Color32, alpha: bool) -> Response {
    let p = pal(ui);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(44.0, 26.0), Sense::click());
    let resp_c = resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    // checkerboard for alpha
    if alpha && c.a() < 255 {
        let n = 6.0;
        let painter = ui.painter_at(rect.shrink(1.0));
        let mut y = rect.top();
        let mut row = 0;
        while y < rect.bottom() {
            let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { n };
            while x < rect.right() {
                painter.rect_filled(
                    Rect::from_min_size(pos2(x, y), vec2(n, n)),
                    0.0,
                    Color32::from_gray(200),
                );
                x += 2.0 * n;
            }
            y += n;
            row += 1;
        }
    }
    ui.painter().rect(
        rect,
        7.0,
        *c,
        Stroke::new(1.0, p.control_border),
        StrokeKind::Inside,
    );
    ui.painter().rect_stroke(
        rect.shrink(1.5),
        6.0,
        Stroke::new(1.0, Color32::from_white_alpha(70)),
        StrokeKind::Inside,
    );
    let mut changed = false;
    egui::Popup::from_toggle_button_response(&resp_c)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .frame(
            egui::Frame::popup(ui.style())
                .corner_radius(12.0)
                .inner_margin(12.0),
        )
        .show(|ui| {
            changed = color_picker(ui, c, alpha);
        });
    if changed {
        resp.mark_changed();
    }
    resp
}

/// A compact HSV picker: saturation/value square, hue bar, optional alpha
/// bar and a row of common colors.
pub fn color_picker(ui: &mut Ui, c: &mut Color32, alpha: bool) -> bool {
    use egui::ecolor::Hsva;
    let p = pal(ui);
    let id = ui.scope_id().with("hsva");
    // Keep the hue while saturation/value are 0 (it can't be derived then).
    let mut hsva = ui
        .data(|d| d.get_temp::<Hsva>(id))
        .filter(|h| Color32::from(*h) == *c)
        .unwrap_or_else(|| Hsva::from(*c));
    let mut changed = false;
    let w = 220.0;
    // SV square.
    let (sv_rect, sv) = ui.allocate_exact_size(vec2(w, 150.0), Sense::click_and_drag());
    let n = 24;
    let mut mesh = egui::Mesh::default();
    for yi in 0..=n {
        for xi in 0..=n {
            let (sx, vy) = (xi as f32 / n as f32, yi as f32 / n as f32);
            let col: Color32 = Hsva::new(hsva.h, sx, 1.0 - vy, 1.0).into();
            mesh.colored_vertex(
                pos2(
                    sv_rect.left() + sx * w,
                    sv_rect.top() + vy * sv_rect.height(),
                ),
                col,
            );
        }
    }
    for yi in 0..n {
        for xi in 0..n {
            let i = (yi * (n + 1) + xi) as u32;
            mesh.add_triangle(i, i + 1, i + n as u32 + 2);
            mesh.add_triangle(i, i + n as u32 + 2, i + n as u32 + 1);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(
        sv_rect,
        6.0,
        Stroke::new(1.0, p.card_border),
        StrokeKind::Inside,
    );
    if let Some(pp) = sv.interact_pointer_pos() {
        hsva.s = ((pp.x - sv_rect.left()) / w).clamp(0.0, 1.0);
        hsva.v = 1.0 - ((pp.y - sv_rect.top()) / sv_rect.height()).clamp(0.0, 1.0);
        changed = true;
    }
    let knob = pos2(
        sv_rect.left() + hsva.s * w,
        sv_rect.top() + (1.0 - hsva.v) * sv_rect.height(),
    );
    ui.painter()
        .circle_stroke(knob, 6.0, Stroke::new(2.5, Color32::WHITE));
    ui.painter()
        .circle_stroke(knob, 7.5, Stroke::new(1.0, Color32::from_black_alpha(90)));
    ui.add_space(4.0);
    // Hue bar.
    let (hue_rect, hue) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click_and_drag());
    let mut mesh = egui::Mesh::default();
    let steps = 36;
    for k in 0..=steps {
        let t = k as f32 / steps as f32;
        let col: Color32 = Hsva::new(t, 1.0, 1.0, 1.0).into();
        mesh.colored_vertex(pos2(hue_rect.left() + t * w, hue_rect.top()), col);
        mesh.colored_vertex(pos2(hue_rect.left() + t * w, hue_rect.bottom()), col);
        if k < steps {
            let i = 2 * k as u32;
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i + 1, i + 2, i + 3);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    if let Some(pp) = hue.interact_pointer_pos() {
        hsva.h = ((pp.x - hue_rect.left()) / w).clamp(0.0, 0.9999);
        changed = true;
    }
    let hx = hue_rect.left() + hsva.h * w;
    ui.painter().rect_stroke(
        Rect::from_center_size(pos2(hx, hue_rect.center().y), vec2(6.0, 18.0)),
        3.0,
        Stroke::new(2.0, Color32::WHITE),
        StrokeKind::Middle,
    );
    if alpha {
        ui.add_space(4.0);
        let mut a = (hsva.a * 100.0).round() as u32;
        ui.spacing_mut().slider_width = w - 60.0;
        if ui
            .add(egui::Slider::new(&mut a, 0..=100).suffix(" %").text(""))
            .changed()
        {
            hsva.a = a as f32 / 100.0;
            changed = true;
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for hex in [
            "#FFFFFF", "#000000", "#8E8E93", "#007AFF", "#5E5CE6", "#AF52DE", "#FF2D55", "#FF9500",
            "#FFCC00", "#34C759",
        ] {
            let col = hex_to_color32(hex, Color32::GRAY);
            let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            ui.painter().circle(
                r.center(),
                7.5,
                col,
                Stroke::new(1.0, Color32::from_black_alpha(50)),
            );
            if resp.on_hover_text(hex).clicked() {
                let a = hsva.a;
                hsva = Hsva::from(col);
                hsva.a = a;
                changed = true;
            }
        }
    });
    if changed {
        let mut out: Color32 = hsva.into();
        if !alpha {
            out = Color32::from_rgb(out.r(), out.g(), out.b());
        }
        *c = out;
    }
    ui.data_mut(|d| d.insert_temp(id, hsva));
    changed
}

/// Swatch + `#RRGGBB` field for a hex color string.
pub fn hex_color_edit(ui: &mut Ui, s: &mut String) -> Response {
    let mut c = hex_to_color32(s, Color32::BLACK);
    let mut r = swatch_picker(ui, &mut c, false);
    if r.changed() {
        *s = color32_to_hex(c);
    }
    let id = r.id.with("hex");
    let mut buf = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| s.clone());
    let te = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .id(id.with("te"))
            .desired_width(78.0)
            .font(egui::TextStyle::Monospace)
            .char_limit(7),
    );
    if te.changed() {
        let t = buf.trim();
        let t = if t.starts_with('#') {
            t.to_string()
        } else {
            format!("#{t}")
        };
        if fancyzones::config::parse_hex_color(&t).is_some() && t.len() == 7 {
            *s = t.to_uppercase();
            r.mark_changed();
        }
    }
    if te.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    r
}

/// Color picker for a Mouse Utilities color (with or without alpha).
pub fn color_edit(ui: &mut Ui, c: &mut mouse_utils::Color, alpha: bool) {
    let mut col = Color32::from_rgba_unmultiplied(c.r, c.g, c.b, if alpha { c.a } else { 255 });
    if swatch_picker(ui, &mut col, alpha).changed() {
        let [r, g, b, a] = col.to_srgba_unmultiplied();
        *c = mouse_utils::Color::argb(if alpha { a } else { c.a }, r, g, b);
    }
    let p = pal(ui);
    ui.label(RichText::new(c.to_string()).monospace().color(p.text2));
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_split() {
        assert_eq!(
            super::split_keys("Ctrl+Shift+V"),
            vec!["Ctrl", "Shift", "V"]
        );
        assert_eq!(super::split_keys("Ctrl++"), vec!["Ctrl", "+"]);
        assert_eq!(super::split_keys("Esc"), vec!["Esc"]);
    }
}
