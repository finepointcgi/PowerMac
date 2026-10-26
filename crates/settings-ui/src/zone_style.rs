//! FancyZones zone colors: presets, the colors the overlay will really use,
//! and a painter that imitates the Shift-drag overlay on a small desktop.

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, Mesh, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
};
use fancyzones::Settings;

use crate::theme::{self, pal};
use crate::widgets::hex_to_color32;

/// A one-click color scheme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub zone: &'static str,
    pub border: &'static str,
    pub highlight: &'static str,
    pub number: &'static str,
    pub opacity: u8,
}

pub const PRESETS: [Preset; 7] = [
    Preset {
        name: "PowerToys Blue",
        zone: "#AACDFF",
        border: "#FFFFFF",
        highlight: "#008CFF",
        number: "#000000",
        opacity: 50,
    },
    Preset {
        name: "Purple",
        zone: "#D6C4FF",
        border: "#FFFFFF",
        highlight: "#8B5CF6",
        number: "#24124A",
        opacity: 50,
    },
    Preset {
        name: "Green",
        zone: "#B9EFCB",
        border: "#FFFFFF",
        highlight: "#22A55A",
        number: "#0B2E17",
        opacity: 50,
    },
    Preset {
        name: "Orange",
        zone: "#FFD7AE",
        border: "#FFFFFF",
        highlight: "#FF8A00",
        number: "#3A1E00",
        opacity: 55,
    },
    Preset {
        name: "Pink",
        zone: "#FFC6E2",
        border: "#FFFFFF",
        highlight: "#F0388E",
        number: "#3D0420",
        opacity: 50,
    },
    Preset {
        name: "Graphite",
        zone: "#D1D1D6",
        border: "#F2F2F7",
        highlight: "#636366",
        number: "#1C1C1E",
        opacity: 55,
    },
    Preset {
        name: "High contrast",
        zone: "#FFFFFF",
        border: "#000000",
        highlight: "#FFD60A",
        number: "#000000",
        opacity: 85,
    },
];

impl Preset {
    pub fn apply(&self, s: &mut Settings) {
        s.use_system_accent = false;
        s.zone_color = self.zone.into();
        s.zone_border_color = self.border.into();
        s.zone_highlight_color = self.highlight.into();
        s.zone_number_color = self.number.into();
        s.zone_highlight_opacity = self.opacity;
    }

    pub fn matches(&self, s: &Settings) -> bool {
        !s.use_system_accent
            && s.zone_color.eq_ignore_ascii_case(self.zone)
            && s.zone_border_color.eq_ignore_ascii_case(self.border)
            && s.zone_highlight_color.eq_ignore_ascii_case(self.highlight)
            && s.zone_number_color.eq_ignore_ascii_case(self.number)
            && s.zone_highlight_opacity == self.opacity
    }

    pub fn colors(&self) -> ZoneColors {
        let c = |h: &str| hex_to_color32(h, Color32::GRAY);
        ZoneColors {
            zone: c(self.zone),
            border: c(self.border),
            highlight: c(self.highlight),
            number: c(self.number),
            opacity: self.opacity as f32 / 100.0,
        }
    }
}

/// Use the system accent: store the derived colors too, so the file shows
/// what the overlay uses (and other tools see sensible values).
pub fn apply_system_accent(s: &mut Settings, accent: Option<(u8, u8, u8)>) {
    s.use_system_accent = true;
    if let Some(a) = accent {
        let z = fancyzones::engine::accent_zone_color(a);
        s.zone_highlight_color = format!("#{:02X}{:02X}{:02X}", a.0, a.1, a.2);
        s.zone_color = format!("#{:02X}{:02X}{:02X}", z.0, z.1, z.2);
    }
}

/// The colors the overlay draws with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneColors {
    pub zone: Color32,
    pub border: Color32,
    pub highlight: Color32,
    pub number: Color32,
    /// 0..=1
    pub opacity: f32,
}

/// Same rules as `Engine::overlay_base`.
pub fn effective(s: &Settings, accent: Option<(u8, u8, u8)>) -> ZoneColors {
    let mut zone = hex_to_color32(&s.zone_color, Color32::from_rgb(0xAA, 0xCD, 0xFF));
    let mut highlight =
        hex_to_color32(&s.zone_highlight_color, Color32::from_rgb(0x00, 0x8C, 0xFF));
    if s.use_system_accent {
        if let Some(a) = accent {
            let z = fancyzones::engine::accent_zone_color(a);
            zone = Color32::from_rgb(z.0, z.1, z.2);
            highlight = Color32::from_rgb(a.0, a.1, a.2);
        }
    }
    ZoneColors {
        zone,
        border: hex_to_color32(&s.zone_border_color, Color32::WHITE),
        highlight,
        number: hex_to_color32(&s.zone_number_color, Color32::BLACK),
        opacity: s.zone_highlight_opacity.min(100) as f32 / 100.0,
    }
}

/// A soft desktop wallpaper (neutral enough to judge colors on).
pub fn paint_wallpaper(p: &Painter, rect: Rect) {
    let tl = Color32::from_rgb(0x74, 0x86, 0xB0);
    let tr = Color32::from_rgb(0x9A, 0x86, 0xBC);
    let bl = Color32::from_rgb(0x2E, 0x3A, 0x5E);
    let br = Color32::from_rgb(0x4C, 0x3D, 0x6E);
    let mut m = Mesh::default();
    m.colored_vertex(rect.left_top(), tl);
    m.colored_vertex(rect.right_top(), tr);
    m.colored_vertex(rect.right_bottom(), br);
    m.colored_vertex(rect.left_bottom(), bl);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

/// Zones (normalized 0..1 rects) drawn like the overlay: fill = zone or
/// highlight color at `opacity`, 2 pt borders (highlight color for the
/// highlighted zone), bold numbers. `scale` = preview points per screen point.
pub fn paint_zones(
    p: &Painter,
    area: Rect,
    zones: &[Rect],
    c: &ZoneColors,
    highlight: Option<usize>,
    numbers: bool,
    scale: f32,
) {
    let a = (c.opacity * 255.0).round() as u8;
    let with_a = |col: Color32| Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a);
    let bw = (2.0 * scale).clamp(1.0, 2.0);
    let radius = (8.0 * scale).clamp(2.0, 8.0);
    for (i, z) in zones.iter().enumerate() {
        let r = Rect::from_min_max(
            area.lerp_inside(z.min.to_vec2()),
            area.lerp_inside(z.max.to_vec2()),
        );
        let hl = highlight == Some(i);
        p.rect(
            r,
            radius,
            with_a(if hl { c.highlight } else { c.zone }),
            Stroke::new(bw, if hl { c.highlight } else { c.border }),
            StrokeKind::Inside,
        );
        if numbers {
            let size = (r.height().min(r.width()) / 4.0).clamp(9.0, 34.0);
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                theme::font_semibold(size),
                c.number,
            );
        }
    }
}

/// A small window being dragged (title bar with traffic lights) + cursor.
fn paint_dragged_window(p: &Painter, r: Rect) {
    let shadow = Color32::from_black_alpha(60);
    p.rect_filled(r.translate(vec2(0.0, 3.0)).expand(1.0), 6.0, shadow);
    p.rect(
        r,
        6.0,
        Color32::from_rgba_unmultiplied(0xF7, 0xF7, 0xFA, 235),
        Stroke::new(1.0, Color32::from_black_alpha(40)),
        StrokeKind::Inside,
    );
    let bar = Rect::from_min_size(r.min, vec2(r.width(), 13.0));
    p.rect_filled(
        bar,
        egui::CornerRadius {
            nw: 6,
            ne: 6,
            sw: 0,
            se: 0,
        },
        Color32::from_rgb(0xE6, 0xE6, 0xEB),
    );
    for (k, col) in [
        (0.0, (0xFF, 0x5F, 0x57)),
        (1.0, (0xFE, 0xBC, 0x2E)),
        (2.0, (0x28, 0xC8, 0x40)),
    ] {
        p.circle_filled(
            pos2(r.left() + 8.0 + k * 8.0, bar.center().y),
            2.4,
            Color32::from_rgb(col.0, col.1, col.2),
        );
    }
    for k in 0..3 {
        let y = bar.bottom() + 8.0 + k as f32 * 6.0;
        let w = r.width() * [0.7, 0.55, 0.62][k] - 12.0;
        if y + 3.0 < r.bottom() {
            p.rect_filled(
                Rect::from_min_size(pos2(r.left() + 7.0, y), vec2(w.max(4.0), 3.0)),
                1.5,
                Color32::from_gray(210),
            );
        }
    }
}

fn paint_cursor(p: &Painter, at: Pos2) {
    let pts = [
        at,
        at + vec2(0.0, 15.0),
        at + vec2(4.0, 11.5),
        at + vec2(7.5, 17.5),
        at + vec2(9.5, 16.5),
        at + vec2(6.5, 10.5),
        at + vec2(11.0, 10.5),
    ];
    p.add(egui::Shape::convex_polygon(
        pts.to_vec(),
        Color32::BLACK,
        Stroke::new(1.2, Color32::WHITE),
    ));
}

/// The live preview: a mini desktop of `work` (w, h) with `zones`
/// (normalized), the zone under the pointer (or `default_hl`) highlighted, a
/// window being dragged into it. Returns the highlighted zone.
pub fn desktop_preview(
    ui: &mut Ui,
    size: egui::Vec2,
    work: (i32, i32),
    zones: &[Rect],
    c: &ZoneColors,
    numbers: bool,
    default_hl: usize,
) -> Option<usize> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let pal = pal(ui);
    let p = ui.painter_at(rect.expand(4.0));
    // Device bezel.
    p.rect_filled(
        rect.translate(vec2(0.0, 2.0)),
        12.0,
        Color32::from_black_alpha(if pal.dark { 90 } else { 28 }),
    );
    p.rect_filled(rect, 12.0, Color32::from_rgb(0x1A, 0x1A, 0x1D));
    let screen = rect.shrink(6.0);
    let sp = ui.painter_at(screen);
    paint_wallpaper(&sp, screen);
    // Menu bar (macOS) or taskbar (Windows) takes part of the screen.
    let mac = cfg!(target_os = "macos");
    let bar_h = 9.0;
    let work_rect = if mac {
        let bar = Rect::from_min_size(screen.min, vec2(screen.width(), bar_h));
        sp.rect_filled(bar, 0.0, Color32::from_white_alpha(70));
        sp.circle_filled(
            pos2(bar.left() + 9.0, bar.center().y),
            2.5,
            Color32::from_black_alpha(140),
        );
        for k in 0..4 {
            sp.rect_filled(
                Rect::from_min_size(
                    pos2(bar.left() + 18.0 + k as f32 * 18.0, bar.center().y - 1.5),
                    vec2(12.0, 3.0),
                ),
                1.5,
                Color32::from_black_alpha(90),
            );
        }
        Rect::from_min_max(pos2(screen.left(), bar.bottom()), screen.max)
    } else {
        let bar = Rect::from_min_max(
            pos2(screen.left(), screen.bottom() - bar_h - 2.0),
            screen.max,
        );
        sp.rect_filled(
            bar,
            0.0,
            Color32::from_rgba_unmultiplied(0x20, 0x20, 0x24, 200),
        );
        for k in 0..5 {
            sp.rect_filled(
                Rect::from_center_size(
                    pos2(bar.center().x + (k as f32 - 2.0) * 12.0, bar.center().y),
                    vec2(7.0, 7.0),
                ),
                1.5,
                Color32::from_white_alpha(150),
            );
        }
        Rect::from_min_max(screen.min, pos2(screen.right(), bar.top()))
    };
    let hovered = resp.hover_pos().and_then(|pos| {
        zones.iter().rposition(|z| {
            Rect::from_min_max(
                work_rect.lerp_inside(z.min.to_vec2()),
                work_rect.lerp_inside(z.max.to_vec2()),
            )
            .contains(pos)
        })
    });
    let hl = if zones.is_empty() {
        None
    } else {
        Some(hovered.unwrap_or(default_hl.min(zones.len() - 1)))
    };
    let scale = work_rect.width() / work.0.max(1) as f32;
    paint_zones(
        &sp,
        work_rect,
        zones,
        c,
        hl,
        numbers,
        (scale * 2.5).min(1.0),
    );
    if zones.is_empty() {
        sp.text(
            work_rect.center(),
            Align2::CENTER_CENTER,
            "No zones in this layout",
            theme::font(13.0),
            Color32::from_white_alpha(220),
        );
    }
    // A window being dragged into the highlighted zone.
    if let Some(i) = hl {
        let z = zones[i];
        let zr = Rect::from_min_max(
            work_rect.lerp_inside(z.min.to_vec2()),
            work_rect.lerp_inside(z.max.to_vec2()),
        );
        let w = (work_rect.width() * 0.26).min(zr.width() * 0.9).max(50.0);
        let h = w * 0.62;
        let c = match hovered {
            Some(_) => resp.hover_pos().unwrap_or(zr.center()),
            None => pos2(zr.center().x, zr.bottom() - zr.height() * 0.3),
        };
        let win = Rect::from_min_size(c - vec2(w * 0.45, 6.0), vec2(w, h));
        paint_dragged_window(&sp, win);
        paint_cursor(&sp, c + vec2(-2.0, 0.0));
    }
    if hovered.is_some() {
        ui.ctx().request_repaint();
    }
    hl
}

/// A preset chip `w` wide: a tiny 3-zone screen in the preset colors and
/// the preset's name.
pub fn preset_chip(
    ui: &mut Ui,
    w: f32,
    name: &str,
    colors: &ZoneColors,
    selected: bool,
) -> egui::Response {
    let pal = pal(ui);
    let size = vec2(w, 48.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = ui.painter_at(rect.expand(3.0));
    let bg = if selected {
        pal.accent_soft
    } else if resp.hovered() {
        pal.control_hover
    } else {
        pal.control
    };
    p.rect_filled(rect, 10.0, bg);
    let screen = Rect::from_min_size(rect.min + vec2(7.0, 7.0), vec2(56.0, 34.0));
    let sp = ui.painter_at(screen);
    paint_wallpaper(&sp, screen);
    let zones = [
        Rect::from_min_max(pos2(0.0, 0.0), pos2(0.5, 1.0)),
        Rect::from_min_max(pos2(0.5, 0.0), pos2(1.0, 0.5)),
        Rect::from_min_max(pos2(0.5, 0.5), pos2(1.0, 1.0)),
    ];
    paint_zones(&sp, screen.shrink(2.0), &zones, colors, Some(0), false, 0.3);
    p.rect_stroke(
        screen,
        4.0,
        Stroke::new(1.0, Color32::from_black_alpha(40)),
        StrokeKind::Inside,
    );
    let tc = if selected { pal.accent } else { pal.text };
    let font = if selected {
        theme::font_semibold(12.5)
    } else {
        theme::font(12.5)
    };
    let clip = Rect::from_min_max(
        pos2(screen.right() + 10.0, rect.top()),
        pos2(rect.right() - 6.0, rect.bottom()),
    );
    p.with_clip_rect(clip).text(
        pos2(clip.left(), rect.center().y),
        Align2::LEFT_CENTER,
        name,
        font,
        tc,
    );
    if selected {
        p.rect_stroke(rect, 10.0, Stroke::new(1.5, pal.accent), StrokeKind::Inside);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_round_trip() {
        let mut s = Settings::default();
        assert!(
            PRESETS[0].matches(&s),
            "the first preset is FancyZones' default"
        );
        for p in PRESETS {
            p.apply(&mut s);
            assert!(p.matches(&s));
            assert!(fancyzones::config::parse_hex_color(&s.zone_color).is_some());
            assert_eq!(effective(&s, None), p.colors());
        }
        apply_system_accent(&mut s, Some((0x0A, 0x84, 0xFF)));
        assert!(s.use_system_accent && PRESETS.iter().all(|p| !p.matches(&s)));
        assert_eq!(s.zone_highlight_color, "#0A84FF");
        let e = effective(&s, Some((0xFF, 0x00, 0x00)));
        assert_eq!(e.highlight, Color32::from_rgb(0xFF, 0, 0));
        assert_eq!(e.zone, Color32::from_rgb(0xFF, 0x99, 0x99));
    }
}
