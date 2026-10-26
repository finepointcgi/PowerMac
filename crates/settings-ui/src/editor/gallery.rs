//! Layout gallery pieces: the templates and the layout preview cards.

use eframe::egui::{self, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui};
use fancyzones::layout::{compute_relative_zones, CustomLayout, LayoutType};
use fancyzones::AppliedLayout;

use crate::theme::{self, icon, pal};

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

pub const CARD_MIN_W: f32 = 196.0;
pub const CARD_MAX_W: f32 = 250.0;

/// What happened to a card this frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct CardResponse {
    pub clicked: bool,
    pub edit: bool,
}

fn card_h(w: f32) -> f32 {
    (w * 0.56).round() + 58.0
}

/// Zones of `applied` drawn on a monitor-shaped area inside `rect`.
pub fn paint_preview(
    ui: &Ui,
    rect: Rect,
    applied: &AppliedLayout,
    custom: &[CustomLayout],
    work: (i32, i32),
    active: bool,
) {
    let p = pal(ui);
    let painter = ui.painter_at(rect.expand(1.0));
    let (w, h) = (work.0.max(1) as f32, work.1.max(1) as f32);
    let scale = (rect.width() / w).min(rect.height() / h);
    let screen = Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
    let screen_fill = if p.dark {
        Color32::from_rgb(0x1E, 0x1E, 0x21)
    } else {
        Color32::from_rgb(0xF2, 0xF3, 0xF6)
    };
    painter.rect(
        screen,
        6.0,
        screen_fill,
        Stroke::new(1.0, p.card_border),
        StrokeKind::Inside,
    );
    let accent = p.accent;
    match compute_relative_zones(applied, work.0.max(1), work.1.max(1), custom) {
        Ok(zones) if zones.is_empty() => {
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "No zones",
                theme::font(12.0),
                p.text3,
            );
        }
        Ok(zones) => {
            let inner = screen.shrink(3.0);
            let s = (inner.width() / w).min(inner.height() / h);
            let fill = accent.gamma_multiply(if active {
                0.26
            } else if p.dark {
                0.13
            } else {
                0.10
            });
            let stroke = Stroke::new(1.0, accent.gamma_multiply(if active { 0.9 } else { 0.5 }));
            for z in zones {
                let r = Rect::from_min_max(
                    inner.min + egui::vec2(z.left as f32 * s, z.top as f32 * s),
                    inner.min + egui::vec2(z.right as f32 * s, z.bottom as f32 * s),
                )
                .shrink(1.5);
                painter.rect(r, 3.0, fill, stroke, StrokeKind::Inside);
            }
        }
        Err(e) => {
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                format!("Invalid: {e}"),
                theme::font(11.0),
                p.error,
            );
        }
    }
}

/// A standalone preview (dialogs).
pub fn preview(
    ui: &mut Ui,
    size: egui::Vec2,
    applied: &AppliedLayout,
    custom: &[CustomLayout],
    work: (i32, i32),
    active: bool,
) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_preview(ui, rect, applied, custom, work, active);
}

/// A layout card `w` wide: preview, name, zone count, the hotkey badge and
/// an edit button; an accent ring when `active`.
#[allow(clippy::too_many_arguments)]
pub fn card(
    ui: &mut Ui,
    w: f32,
    name: &str,
    applied: &AppliedLayout,
    custom: &[CustomLayout],
    work: (i32, i32),
    active: bool,
    hotkey: Option<u8>,
) -> CardResponse {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, card_h(w)), Sense::click());
    let resp: Response = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter_at(rect.expand(4.0));
    let hovered = resp.hovered();
    // Elevation.
    painter.rect_filled(
        rect.translate(egui::vec2(0.0, if hovered { 3.0 } else { 1.0 })),
        12.0,
        Color32::from_black_alpha(if p.dark {
            50
        } else if hovered {
            22
        } else {
            10
        }),
    );
    let fill = if hovered && !active {
        theme::mix(p.card, p.control, 0.5)
    } else {
        p.card
    };
    painter.rect(
        rect,
        12.0,
        fill,
        Stroke::new(1.0, p.card_border),
        StrokeKind::Inside,
    );
    if active {
        painter.rect_stroke(rect, 12.0, Stroke::new(2.0, p.accent), StrokeKind::Inside);
    }
    let area = Rect::from_min_max(
        rect.left_top() + egui::vec2(12.0, 12.0),
        egui::pos2(rect.right() - 12.0, rect.bottom() - 50.0),
    );
    paint_preview(ui, area, applied, custom, work, active);
    // Footer: name, count, hotkey, edit.
    let fy = rect.bottom() - 38.0;
    let count = match compute_relative_zones(applied, 1920, 1080, custom) {
        Ok(z) => crate::editor::zones_label(z.len()),
        Err(_) => String::new(),
    };
    let text_clip = Rect::from_min_max(
        egui::pos2(rect.left() + 14.0, fy - 4.0),
        egui::pos2(rect.right() - 44.0, rect.bottom()),
    );
    let tp = painter.with_clip_rect(text_clip);
    let mut x = rect.left() + 14.0;
    if let Some(k) = hotkey {
        let b = Rect::from_min_size(egui::pos2(x, fy - 1.0), egui::vec2(20.0, 18.0));
        tp.rect(
            b,
            5.0,
            p.control,
            Stroke::new(1.0, p.control_border),
            StrokeKind::Inside,
        );
        tp.text(
            b.center(),
            egui::Align2::CENTER_CENTER,
            k.to_string(),
            theme::font_semibold(11.0),
            p.text,
        );
        x += 27.0;
    }
    tp.text(
        egui::pos2(x, fy + 8.0),
        egui::Align2::LEFT_CENTER,
        name,
        theme::font_semibold(13.0),
        p.text,
    );
    tp.text(
        egui::pos2(rect.left() + 14.0, fy + 26.0),
        egui::Align2::LEFT_CENTER,
        count,
        theme::font(11.5),
        p.text2,
    );
    if active {
        let c = egui::pos2(rect.right() - 22.0, rect.top() + 22.0);
        painter.circle_filled(c, 10.0, p.accent);
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            icon::CHECK,
            theme::icon_font(12.0),
            Color32::WHITE,
        );
    }
    // Edit button (own interaction; doesn't move the cursor).
    let edit_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 26.0, fy + 17.0),
        egui::vec2(30.0, 30.0),
    );
    let edit = ui
        .interact(edit_rect, resp.id.with("edit"), Sense::click())
        .on_hover_text("Edit")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if edit.hovered() {
        painter.rect_filled(edit_rect, 8.0, p.control_hover);
    }
    painter.text(
        edit_rect.center(),
        egui::Align2::CENTER_CENTER,
        icon::PENCIL_SIMPLE,
        theme::icon_font(15.0),
        if edit.hovered() || hovered {
            p.text
        } else {
            p.text2
        },
    );
    let mut out = CardResponse::default();
    if edit.clicked() {
        out.edit = true;
    } else if resp.clicked() {
        out.clicked = true;
    }
    out
}

/// The dashed "Create new layout" card.
pub fn new_card(ui: &mut Ui, w: f32) -> bool {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, card_h(w)), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter_at(rect.expand(2.0));
    let hovered = resp.hovered();
    let c = if hovered { p.accent } else { p.text2 };
    if hovered {
        painter.rect_filled(rect, 12.0, p.accent_soft);
    }
    // Dashed outline.
    let r = rect.shrink(1.0);
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    let shapes = egui::Shape::dashed_line(
        &pts,
        Stroke::new(1.3, if hovered { p.accent } else { p.control_border }),
        6.0,
        4.0,
    );
    painter.with_clip_rect(rect).extend(shapes);
    let circle = rect.center() - egui::vec2(0.0, 14.0);
    painter.circle_filled(circle, 20.0, if hovered { p.accent } else { p.control });
    painter.text(
        circle,
        egui::Align2::CENTER_CENTER,
        icon::PLUS,
        theme::icon_font(20.0),
        if hovered { Color32::WHITE } else { p.text2 },
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 26.0),
        egui::Align2::CENTER_CENTER,
        "New layout",
        theme::font_semibold(13.0),
        c,
    );
    resp.clicked()
}
