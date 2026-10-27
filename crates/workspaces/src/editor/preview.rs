//! Layout previews: the workspace's monitors with each window drawn as a
//! small window (title bar, app icon), minimized windows as a dock strip.
//! Drawn from the saved rectangles and app icons only — no screenshots, so
//! no Screen Recording permission.

use std::collections::HashMap;

use eframe::egui::{self, pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Stroke, StrokeKind, TextureHandle};
use pm_ui::theme::{self, Palette};

use crate::geometry::Rect as WRect;
use crate::model::{App, WindowState, Workspace};

/// App icons as textures, loaded on first use (per app key).
#[derive(Default)]
pub struct Icons {
    map: HashMap<String, Option<TextureHandle>>,
    /// Don't touch the OS (tests that need deterministic output).
    pub disabled: bool,
}

pub const ICON_PX: u32 = 64;

impl Icons {
    /// Fallback tiles only.
    pub fn disabled() -> Self {
        Self { map: HashMap::new(), disabled: true }
    }

    pub fn get(&mut self, ctx: &egui::Context, app: &App) -> Option<TextureHandle> {
        let key = crate::matching::app_key(app);
        if let Some(t) = self.map.get(&key) {
            return t.clone();
        }
        let tex = if self.disabled {
            None
        } else {
            crate::platform::app_icon_rgba(&app.path, &app.bundle_id, ICON_PX).map(|buf| {
                let img = egui::ColorImage::from_rgba_premultiplied([ICON_PX as usize, ICON_PX as usize], &buf);
                ctx.load_texture(format!("ws-icon-{key}"), img, egui::TextureOptions::LINEAR)
            })
        };
        self.map.insert(key, tex.clone());
        tex
    }
}

/// A stable color for an app (fallback icon, title bar tint).
pub fn app_color(app: &App) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(0x0A, 0x84, 0xFF),
        Color32::from_rgb(0x5E, 0x5C, 0xE6),
        Color32::from_rgb(0xBF, 0x5A, 0xF2),
        Color32::from_rgb(0xFF, 0x37, 0x5F),
        Color32::from_rgb(0xFF, 0x9F, 0x0A),
        Color32::from_rgb(0x30, 0xD1, 0x58),
        Color32::from_rgb(0x40, 0xC8, 0xE0),
        Color32::from_rgb(0xAC, 0x8E, 0x68),
    ];
    let k = crate::matching::app_key(app);
    let h = k.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    COLORS[(h as usize) % COLORS.len()]
}

/// The app's icon, or a rounded tile with its initial.
pub fn paint_app_icon(painter: &Painter, rect: Rect, app: &App, tex: Option<&TextureHandle>, alpha: f32) {
    match tex {
        Some(t) => {
            painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(alpha));
        }
        None => {
            let tint = app_color(app).gamma_multiply(alpha);
            let s = rect.width();
            let r = rect.shrink(s * 0.08);
            painter.rect_filled(r, (r.width() * 0.24).round(), tint);
            let letter: String = app.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
            painter.text(r.center(), Align2::CENTER_CENTER, letter, theme::font_semibold(r.height() * 0.55), Color32::WHITE.gamma_multiply(alpha));
        }
    }
}

fn union(rs: impl Iterator<Item = WRect>) -> Option<WRect> {
    let mut out: Option<(i32, i32, i32, i32)> = None;
    for r in rs {
        if r.is_empty() {
            continue;
        }
        out = Some(match out {
            None => (r.x, r.y, r.right(), r.bottom()),
            Some((l, t, rr, b)) => (l.min(r.x), t.min(r.y), rr.max(r.right()), b.max(r.bottom())),
        });
    }
    out.map(|(l, t, r, b)| WRect::new(l, t, r - l, b - t))
}

/// Maps global coordinates into the preview rect.
#[derive(Clone, Copy)]
pub struct Mapper {
    origin: (f32, f32),
    scale: f32,
    offset: Pos2,
}

impl Mapper {
    pub fn rect(&self, r: &WRect) -> Rect {
        Rect::from_min_size(
            self.offset + vec2((r.x as f32 - self.origin.0) * self.scale, (r.y as f32 - self.origin.1) * self.scale),
            vec2(r.width as f32 * self.scale, r.height as f32 * self.scale),
        )
    }
}

fn desktop_fill(p: &Palette) -> (Color32, Color32) {
    if p.dark {
        (Color32::from_rgb(0x2A, 0x4A, 0x7C), Color32::from_rgb(0x4A, 0x36, 0x7A))
    } else {
        (Color32::from_rgb(0x9F, 0xC3, 0xF5), Color32::from_rgb(0xC6, 0xB2, 0xF0))
    }
}

/// Height / width of the area the preview shows (monitors, else windows).
pub fn aspect(ws: &Workspace) -> f32 {
    let b = union(ws.monitors.iter().map(|m| m.frame)).or_else(|| union(ws.apps.iter().map(|a| a.frame)));
    b.map(|b| b.height as f32 / b.width.max(1) as f32).unwrap_or(0.6)
}

/// A vertical-gradient rounded rect (desktop wallpaper).
fn gradient(painter: &Painter, rect: Rect, r: f32, top: Color32, bottom: Color32) {
    let mut pts = Vec::new();
    let r = r.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let n = 5;
    let corners = [
        (pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (c, start) in corners {
        for k in 0..=n {
            let a = (start + 90.0 * k as f32 / n as f32).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * r);
        }
    }
    let mut mesh = egui::Mesh::default();
    let col = |p: Pos2| theme::mix(top, bottom, ((p.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0));
    mesh.colored_vertex(rect.center(), col(rect.center()));
    for p in &pts {
        mesh.colored_vertex(*p, col(*p));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Where each app is drawn (for hit testing): (app index, rect), front
/// first.
pub type HitList = Vec<(usize, Rect)>;

/// Frame of an app as drawn (zoomed / full screen fill their monitor).
fn drawn_frame(ws: &Workspace, a: &App) -> WRect {
    let m = ws.monitor(&a.monitor_id);
    match (a.state, m) {
        (WindowState::FullScreen, Some(m)) => m.frame,
        (WindowState::Zoomed, Some(m)) if !m.work_area.is_empty() => m.work_area,
        _ => a.frame,
    }
}

/// Paint the preview of `ws` into `rect`. `included[i] == false` dims an
/// app; `selected` is outlined. Returns the hit list.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    ui: &egui::Ui,
    painter: &Painter,
    rect: Rect,
    ws: &Workspace,
    icons: &mut Icons,
    included: Option<&[bool]>,
    selected: Option<usize>,
    hovered: Option<usize>,
) -> HitList {
    let p = theme::pal(ui);
    let ctx = ui.ctx().clone();
    let mut hits: HitList = Vec::new();
    let visible = |i: usize| included.is_none_or(|v| v.get(i).copied().unwrap_or(true));
    let bounds = union(ws.monitors.iter().map(|m| m.frame)).or_else(|| union(ws.apps.iter().map(|a| a.frame)));
    let Some(b) = bounds else {
        painter.text(rect.center(), Align2::CENTER_CENTER, "No windows", theme::font(12.0), p.text3);
        return hits;
    };
    let pad = 6.0;
    let avail = rect.shrink(pad);
    let scale = (avail.width() / b.width.max(1) as f32).min(avail.height() / b.height.max(1) as f32);
    let size = vec2(b.width as f32 * scale, b.height as f32 * scale);
    let offset = avail.center() - size / 2.0;
    let map = Mapper { origin: (b.x as f32, b.y as f32), scale, offset };
    let (top, bottom) = desktop_fill(&p);
    let bezel = (scale * 40.0).clamp(1.5, 4.0);
    let mon_rects: Vec<Rect> = ws.monitors.iter().map(|m| map.rect(&m.frame).shrink(1.5).shrink(bezel)).collect();
    for (m, mr) in ws.monitors.iter().zip(&mon_rects) {
        let cr = (mr.height() * 0.035).clamp(2.0, 6.0);
        let frame_col = if p.dark { Color32::from_rgb(0x0E, 0x0E, 0x10) } else { Color32::from_rgb(0x3A, 0x3A, 0x3E) };
        painter.rect_filled(mr.expand(bezel), cr + bezel, frame_col);
        gradient(painter, *mr, cr, top, bottom);
        // Menu bar / taskbar strip.
        if !m.work_area.is_empty() && m.work_area.y > m.frame.y {
            let h = ((m.work_area.y - m.frame.y) as f32 * scale).max(2.0);
            let bar = Rect::from_min_size(mr.min, vec2(mr.width(), h));
            painter.rect_filled(bar, egui::CornerRadius { nw: cr as u8, ne: cr as u8, sw: 0, se: 0 }, Color32::from_white_alpha(if p.dark { 18 } else { 110 }));
        }
        painter.rect_stroke(*mr, cr, Stroke::new(1.0, if p.dark { Color32::from_white_alpha(26) } else { Color32::from_black_alpha(30) }), StrokeKind::Inside);
    }
    let clip = painter.clip_rect().intersect(rect);
    let painter = painter.with_clip_rect(clip);

    // Back to front: apps[0] was the frontmost window.
    let mut minimized: Vec<(usize, usize)> = Vec::new(); // (app, monitor index)
    for i in (0..ws.apps.len()).rev() {
        let a = &ws.apps[i];
        if a.state == WindowState::Minimized {
            let mi = ws.monitors.iter().position(|m| m.id == a.monitor_id).unwrap_or(0);
            minimized.push((i, mi));
            continue;
        }
        let f = drawn_frame(ws, a);
        let mut r = map.rect(&f);
        if let Some(mr) = ws.monitors.iter().position(|m| m.id == a.monitor_id).and_then(|k| mon_rects.get(k)) {
            r = r.intersect(*mr);
        }
        if r.width() < 2.0 || r.height() < 2.0 {
            continue;
        }
        let on = visible(i);
        let alpha = if on { 1.0 } else { 0.28 };
        let sel = selected == Some(i);
        let hov = hovered == Some(i);
        let cr = (r.height().min(r.width()) * 0.06).clamp(1.5, 5.0);
        let body = if p.dark { Color32::from_rgb(0x2C, 0x2C, 0x30) } else { Color32::from_rgb(0xFB, 0xFB, 0xFD) };
        // shadow
        painter.rect_filled(r.translate(vec2(0.0, 1.0)).expand(0.5), cr, Color32::from_black_alpha((if p.dark { 90.0 } else { 34.0 } * alpha) as u8));
        painter.rect_filled(r, cr, body.gamma_multiply(if on { 1.0 } else { 0.55 }));
        let bar_h = (r.height() * 0.12).clamp(2.0, 9.0);
        let tint = app_color(a);
        let bar = Rect::from_min_size(r.min, vec2(r.width(), bar_h));
        painter.rect_filled(bar, egui::CornerRadius { nw: cr as u8, ne: cr as u8, sw: 0, se: 0 }, theme::mix(body, tint, if p.dark { 0.35 } else { 0.22 }).gamma_multiply(alpha));
        if bar_h >= 5.0 && r.width() > 30.0 {
            for (k, c) in [Color32::from_rgb(0xFF, 0x5F, 0x57), Color32::from_rgb(0xFE, 0xBC, 0x2E), Color32::from_rgb(0x28, 0xC8, 0x40)].iter().enumerate() {
                painter.circle_filled(pos2(r.left() + bar_h * 0.7 + k as f32 * bar_h * 0.75, bar.center().y), (bar_h * 0.2).max(1.0), c.gamma_multiply(alpha));
            }
        }
        let content = Rect::from_min_max(pos2(r.left(), bar.bottom()), r.max);
        let isz = (content.width().min(content.height()) * 0.42).clamp(8.0, 44.0);
        if content.height() > 8.0 {
            let tex = icons.get(&ctx, a);
            paint_app_icon(&painter, Rect::from_center_size(content.center(), vec2(isz, isz)), a, tex.as_ref(), alpha);
        }
        let stroke = if sel {
            Stroke::new(2.0, p.accent)
        } else if hov {
            Stroke::new(1.5, p.accent.gamma_multiply(0.6))
        } else {
            Stroke::new(1.0, if p.dark { Color32::from_white_alpha(28) } else { Color32::from_black_alpha(38) }.gamma_multiply(alpha))
        };
        painter.rect_stroke(r, cr, stroke, if sel { StrokeKind::Outside } else { StrokeKind::Inside });
        hits.insert(0, (i, r));
    }
    // Minimized: a dock strip at the bottom of their monitor.
    minimized.reverse();
    for (k, mr) in mon_rects.iter().enumerate() {
        let items: Vec<usize> = minimized.iter().filter(|(_, m)| *m == k).map(|(i, _)| *i).collect();
        if items.is_empty() {
            continue;
        }
        let isz = (mr.height() * 0.1).clamp(9.0, 22.0);
        let gap = isz * 0.25;
        let w = items.len() as f32 * (isz + gap) + gap;
        let dock = Rect::from_center_size(pos2(mr.center().x, mr.bottom() - isz * 0.5 - 4.0), vec2(w.min(mr.width() - 4.0), isz + gap * 1.4));
        painter.rect_filled(dock, (dock.height() * 0.3).min(6.0), Color32::from_white_alpha(if p.dark { 30 } else { 120 }));
        for (j, &i) in items.iter().enumerate() {
            let r = Rect::from_min_size(pos2(dock.left() + gap + j as f32 * (isz + gap), dock.center().y - isz / 2.0), vec2(isz, isz));
            if r.right() > dock.right() {
                break;
            }
            let tex = icons.get(&ctx, &ws.apps[i]);
            let alpha = if visible(i) { 0.95 } else { 0.3 };
            paint_app_icon(&painter, r, &ws.apps[i], tex.as_ref(), alpha);
            if selected == Some(i) || hovered == Some(i) {
                painter.rect_stroke(r.expand(1.5), 4.0, Stroke::new(if selected == Some(i) { 2.0 } else { 1.5 }, p.accent), StrokeKind::Outside);
            }
            hits.insert(0, (i, r));
        }
    }
    hits
}

/// The app under `pos` (front first).
pub fn hit(hits: &HitList, pos: Pos2) -> Option<usize> {
    hits.iter().find(|(_, r)| r.contains(pos)).map(|(i, _)| *i)
}
