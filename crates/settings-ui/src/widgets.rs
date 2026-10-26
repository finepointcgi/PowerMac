//! Small reusable widgets: sections, the on/off switch, color pickers.

use eframe::egui::{self, Color32, Ui};

pub const ERROR: Color32 = Color32::from_rgb(0xE0, 0x4F, 0x4F);
pub const WARN: Color32 = Color32::from_rgb(0xD8, 0x9B, 0x2B);
pub const OK: Color32 = Color32::from_rgb(0x3F, 0xA8, 0x5B);
pub const ACCENT: Color32 = Color32::from_rgb(0x2B, 0x88, 0xD8);

/// A titled card.
pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).strong().size(15.0));
        ui.add_space(4.0);
        add(ui);
    });
}

/// Two-column label/widget grid.
pub fn grid(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([20.0, 8.0]).show(ui, add);
}

pub fn error_label(ui: &mut Ui, text: &str) {
    ui.colored_label(ERROR, text);
}

/// An iOS/WinUI-style on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, ""));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let v = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(v.expansion);
        let r = 0.5 * rect.height();
        let fill = if *on { ACCENT } else { v.bg_fill };
        ui.painter().rect(rect, r, fill, v.bg_stroke, egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
        ui.painter().circle(egui::pos2(x, rect.center().y), 0.75 * r, Color32::WHITE, v.fg_stroke);
    }
    response
}

/// The switch at the top of a page / card.
pub fn enable_row(ui: &mut Ui, label: &str, on: &mut bool) {
    ui.horizontal(|ui| {
        toggle(ui, on);
        ui.label(egui::RichText::new(label).size(15.0));
    });
}

/// A dropdown over a fixed list of values.
pub fn combo<T: PartialEq + Copy>(ui: &mut Ui, id: &str, value: &mut T, items: &[(T, &str)]) {
    let current = items.iter().find(|(v, _)| v == value).map(|(_, n)| *n).unwrap_or("?");
    egui::ComboBox::from_id_salt(id).selected_text(current).width(240.0).show_ui(ui, |ui| {
        for (v, n) in items {
            ui.selectable_value(value, *v, *n);
        }
    });
}

pub fn percent_slider(ui: &mut Ui, v: &mut f64) {
    let mut pct = (*v * 100.0).round().clamp(0.0, 100.0) as u32;
    if ui.add(egui::Slider::new(&mut pct, 0..=100).suffix(" %")).changed() {
        *v = f64::from(pct) / 100.0;
    }
}

/// Color picker for a Mouse Utilities color (with or without alpha).
pub fn color_edit(ui: &mut Ui, c: &mut mouse_utils::Color, alpha: bool) {
    if alpha {
        let mut rgba = [c.r, c.g, c.b, c.a];
        if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
            *c = mouse_utils::Color::argb(rgba[3], rgba[0], rgba[1], rgba[2]);
        }
    } else {
        let mut rgb = [c.r, c.g, c.b];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *c = mouse_utils::Color::argb(c.a, rgb[0], rgb[1], rgb[2]);
        }
    }
    ui.monospace(c.to_string());
}

pub fn hex_to_color32(s: &str, fallback: Color32) -> Color32 {
    fancyzones::config::parse_hex_color(s).map(|(r, g, b)| Color32::from_rgb(r, g, b)).unwrap_or(fallback)
}

/// Color picker for a FancyZones `#RRGGBB` string.
pub fn hex_color_edit(ui: &mut Ui, s: &mut String) {
    ui.horizontal(|ui| {
        let c = hex_to_color32(s, Color32::BLACK);
        let mut rgb = [c.r(), c.g(), c.b()];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *s = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
        }
        ui.monospace(s.as_str());
    });
}
