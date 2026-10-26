//! Layout gallery pieces: the templates and the layout preview cards.

use eframe::egui::{self, Color32, Rect, Response, RichText, Sense, Stroke, Ui};
use fancyzones::layout::{compute_relative_zones, CustomLayout, LayoutType};
use fancyzones::AppliedLayout;

use crate::widgets::ACCENT;

/// The FancyZones templates in gallery order with their default zone count.
pub const TEMPLATES: [(LayoutType, &str, i32); 6] = [
    (LayoutType::Blank, "No layout", 0),
    (LayoutType::Focus, "Focus", 3),
    (LayoutType::Columns, "Columns", 3),
    (LayoutType::Rows, "Rows", 3),
    (LayoutType::Grid, "Grid", 4),
    (LayoutType::PriorityGrid, "Priority grid", 3),
];

pub const MAX_ZONES: i32 = 128;

pub fn template_name(kind: LayoutType) -> &'static str {
    TEMPLATES.iter().find(|t| t.0 == kind).map(|t| t.1).unwrap_or("Custom")
}

/// Whether a template has a zone count / uses spacing (FancyZones' editor
/// hides those for Blank, and spacing for Focus).
pub fn has_zone_count(kind: LayoutType) -> bool {
    !matches!(kind, LayoutType::Blank | LayoutType::Custom)
}
pub fn has_spacing(kind: LayoutType) -> bool {
    !matches!(kind, LayoutType::Blank | LayoutType::Focus)
}

/// The template parameters the gallery applies.
pub fn default_template(kind: LayoutType) -> AppliedLayout {
    let n = TEMPLATES.iter().find(|t| t.0 == kind).map(|t| t.2).unwrap_or(3);
    AppliedLayout::template(kind, n)
}

pub const CARD_W: f32 = 190.0;
pub const CARD_H: f32 = 158.0;

/// What happened to a card this frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct CardResponse {
    pub clicked: bool,
    pub edit: bool,
}

/// A layout preview card: name, zones on a `work` (w, h) screen, a
/// highlight when `active`, the hotkey badge and an Edit button.
pub fn card(
    ui: &mut Ui,
    name: &str,
    applied: &AppliedLayout,
    custom: &[CustomLayout],
    work: (i32, i32),
    active: bool,
    hotkey: Option<u8>,
) -> CardResponse {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), Sense::click());
    let resp: Response = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let v = ui.visuals().clone();
    let p = ui.painter_at(rect.expand(2.0));
    let bg = if resp.hovered() { v.widgets.hovered.weak_bg_fill } else { v.faint_bg_color };
    let stroke = if active { Stroke::new(2.5, ACCENT) } else { v.widgets.noninteractive.bg_stroke };
    p.rect(rect, 8.0, bg, stroke, egui::StrokeKind::Inside);
    p.text(rect.left_top() + egui::vec2(12.0, 10.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(14.0), v.strong_text_color());
    if let Some(k) = hotkey {
        let b = Rect::from_min_size(rect.right_top() + egui::vec2(-30.0, 8.0), egui::vec2(20.0, 20.0));
        p.rect_filled(b, 4.0, v.widgets.inactive.bg_fill);
        p.text(b.center(), egui::Align2::CENTER_CENTER, k.to_string(), egui::FontId::monospace(13.0), v.text_color());
    }
    // Preview area keeps the monitor's aspect ratio.
    let area = Rect::from_min_max(rect.left_top() + egui::vec2(12.0, 34.0), rect.right_bottom() - egui::vec2(12.0, 34.0));
    let (w, h) = (work.0.max(1) as f32, work.1.max(1) as f32);
    let scale = (area.width() / w).min(area.height() / h);
    let screen = Rect::from_center_size(area.center(), egui::vec2(w * scale, h * scale));
    p.rect(screen, 3.0, v.extreme_bg_color, Stroke::new(1.0, v.weak_text_color()), egui::StrokeKind::Inside);
    match compute_relative_zones(applied, work.0.max(1), work.1.max(1), custom) {
        Ok(zones) if zones.is_empty() => {
            p.text(screen.center(), egui::Align2::CENTER_CENTER, "No zones", egui::FontId::proportional(12.0), v.weak_text_color());
        }
        Ok(zones) => {
            let fill = Color32::from_rgba_unmultiplied(0x2B, 0x88, 0xD8, if active { 130 } else { 80 });
            for z in zones {
                let r = Rect::from_min_max(
                    screen.min + egui::vec2(z.left as f32 * scale, z.top as f32 * scale),
                    screen.min + egui::vec2(z.right as f32 * scale, z.bottom as f32 * scale),
                )
                .shrink(0.5);
                p.rect(r, 2.0, fill, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            }
        }
        Err(e) => {
            p.text(screen.center(), egui::Align2::CENTER_CENTER, format!("Invalid: {e}"), egui::FontId::proportional(11.0), crate::widgets::ERROR);
        }
    }
    // Footer: zone count + Edit.
    let footer_y = rect.bottom() - 18.0;
    let count = match compute_relative_zones(applied, 1920, 1080, custom) {
        Ok(z) => crate::editor::zones_label(z.len()),
        Err(_) => String::new(),
    };
    p.text(egui::pos2(rect.left() + 12.0, footer_y), egui::Align2::LEFT_CENTER, count, egui::FontId::proportional(12.0), v.weak_text_color());
    let edit_rect = Rect::from_center_size(egui::pos2(rect.right() - 34.0, footer_y), egui::vec2(48.0, 22.0));
    let mut out = CardResponse::default();
    let edit = ui.put(edit_rect, egui::Button::new(RichText::new("Edit").size(12.0)));
    if edit.clicked() {
        out.edit = true;
    } else if resp.clicked() {
        out.clicked = true;
    }
    out
}

/// The dashed "Create new layout" card.
pub fn new_card(ui: &mut Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let v = ui.visuals();
    let p = ui.painter_at(rect.expand(2.0));
    let c = if resp.hovered() { ACCENT } else { v.weak_text_color() };
    p.rect(rect, 8.0, Color32::TRANSPARENT, Stroke::new(1.5, c), egui::StrokeKind::Inside);
    p.text(rect.center() - egui::vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(34.0), c);
    p.text(rect.center() + egui::vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, "Create new layout", egui::FontId::proportional(14.0), c);
    resp.clicked()
}
