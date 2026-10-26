//! The Home view (widget cards), the Home / Shelf switcher, live activities
//! beside the collapsed notch, the note editor and the camera mirror.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use serde::{Deserialize, Serialize};

use super::super::camera::Camera;
use super::super::paint::{self, Canvas, Env, FontCache};
use super::super::{calendar as mac_cal, rt, sysinfo};
use super::*;
use crate::widgets::draw::{self, Act, CardDraw, Control, Ctx, Live};
use crate::widgets::layout::{self as wl, CardSpec};
use crate::widgets::media::Command;
use crate::widgets::timer::{Timer, TimerEvent};
use crate::widgets::{Access, DefaultView, View, WidgetData, WidgetKind};

/// Ears of a live activity, each side of the notch.
const LIVE_WING: f64 = 58.0;
const STATE_FILE: &str = "notch-widgets.json";
const NOTE_FILE: &str = "notch-note.txt";
const NOTE_MAX: usize = 4000;

pub fn mono() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    last_view: Option<View>,
}

/// App-wide widget state.
pub(in super::super) struct Widgets {
    pub view: View,
    pub last_view: View,
    pub timer: Timer,
    pub data: WidgetData,
    pub data_gen: u64,
    pub controls: Vec<Control>,
    pub controls_at: f64,
    pub artwork: Option<(u64, gfx::Image)>,
    pub fonts: FontCache,
    pub note: String,
    pub note_dirty: Option<f64>,
    pub editing: Option<usize>,
    pub camera: Camera,
    pub camera_access: Access,
    pub utc_offset: f64,
    pub offset_at: f64,
    pub forced_hover: Option<Act>,
    pub state_path: PathBuf,
    pub note_path: PathBuf,
    pub peek_timer: bool,
    pub demo: bool,
    pub battery_present: bool,
}

impl Widgets {
    pub fn new(data_dir: &std::path::Path) -> Widgets {
        let state_path = data_dir.join(STATE_FILE);
        let note_path = data_dir.join(NOTE_FILE);
        let p: Persisted = std::fs::read(&state_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let last_view = p.last_view.unwrap_or(View::Home);
        Widgets {
            view: last_view,
            last_view,
            timer: Timer::default(),
            data: WidgetData::default(),
            data_gen: 0,
            controls: vec![],
            controls_at: f64::NEG_INFINITY,
            artwork: None,
            fonts: FontCache::default(),
            note: std::fs::read_to_string(&note_path).unwrap_or_default(),
            note_dirty: None,
            editing: None,
            camera: Camera::new(),
            camera_access: Access::Unknown,
            utc_offset: rt::utc_offset(),
            offset_at: mono(),
            forced_hover: None,
            state_path,
            note_path,
            peek_timer: false,
            demo: false,
            battery_present: sysinfo::battery().is_some(),
        }
    }

    fn save_state(&self) {
        let p = Persisted { last_view: Some(self.last_view) };
        if let Ok(b) = serde_json::to_vec_pretty(&p) {
            let _ = std::fs::write(&self.state_path, b);
        }
    }
}

pub(in super::super) struct CardView {
    pub kind: WidgetKind,
    pub canvas: Canvas,
    pub draw: CardDraw,
}

pub(in super::super) struct HomePanel {
    pub group: Layer,
    /// Clips the cards (kept alive with the panel).
    pub _row: Layer,
    pub row_fade: Layer,
    pub layout: geo::HomeLayout,
    pub specs: Vec<CardSpec>,
    pub cards: Vec<CardView>,
    pub hover: Option<Act>,
}

pub(in super::super) struct SwitchView {
    pub bg: Layer,
    pub knob: Layer,
    pub icons: [Layer; 2],
    pub shown: Option<View>,
}

pub(in super::super) struct LiveView {
    pub canvas: Canvas,
    pub alpha: f64,
    pub live: Option<Live>,
    /// Widened collapsed shape while a live activity shows.
    pub rect: Option<Rect>,
}

pub(in super::super) struct NoteView {
    pub scroll: Retained<AnyObject>,
    pub text: Retained<AnyObject>,
}

impl App {
    // ----- setup ------------------------------------------------------------

    /// Kinds that get a card (static per panel build).
    pub(super) fn home_kinds(&self) -> Vec<WidgetKind> {
        let mut data = WidgetData::default();
        if self.w.battery_present || self.w.data.battery.is_some() {
            data.battery = Some(Default::default());
        }
        draw::visible_kinds(&self.cfg.enabled_widgets(), &data, self.n_controls())
    }

    pub(super) fn n_controls(&self) -> usize {
        self.shared.host.toggles.len() + self.shared.host.actions.len()
    }

    pub(super) fn home_specs(&self) -> Vec<CardSpec> {
        let n = self.n_controls();
        self.home_kinds().iter().map(|k| wl::spec(*k, n)).collect()
    }

    /// The Home view's panel rect for a collapsed rect, if there's a Home view.
    pub(super) fn home_rect_for(&self, collapsed: &Rect, screen: &Rect) -> Option<Rect> {
        let specs = self.home_specs();
        if specs.is_empty() {
            return None;
        }
        let m = self.metrics;
        Some(geo::home_rect(collapsed, screen, &m, wl::home_width(&specs, m.side_pad, m.panel_width)))
    }

    /// Layers of the Home view, the switcher and the live activity.
    pub(super) fn build_home(&mut self, p: &mut Panel) {
        let scale = p.scale;
        let bounds = Rect::new(0.0, 0.0, p.win.w, p.win.h);
        let mk = |parent: &AnyObject| {
            let l = gfx::layer();
            gfx::set_scale(&l, scale);
            gfx::add(parent, &l);
            l
        };
        // Live activity on the collapsed notch (in the container, above the shape).
        let live_root = p.peek_layer.clone();
        let mut live = Canvas::new(&live_root, scale);
        unsafe {
            let sup: Option<Retained<AnyObject>> = msg_send![&*live_root, superlayer];
            if let Some(sup) = sup {
                gfx::remove(&live.root);
                let _: () = msg_send![&*sup, insertSublayer: &*live.root, below: &*live_root];
            }
        }
        live.set_frame(bounds);
        gfx::set_opacity(&live.root, 0.0);
        p.live = Some(LiveView { canvas: live, alpha: 0.0, live: None, rect: None });

        let Some(home_rect) = p.home_rect else { return };
        let specs = self.home_specs();
        let kinds = self.home_kinds();
        let group = mk(&p.inner);
        gfx::set_frame(&group, bounds);
        let layout = geo::home_layout(&home_rect, &p.collapsed, &specs, 0.0, &self.metrics);
        let row = mk(&group);
        gfx::set_frame(&row, Self::local(p, layout.row));
        gfx::set_masks_to_bounds(&row, true);
        let row_fade = gfx::gradient_layer();
        gfx::set_frame(&row_fade, Rect::new(0.0, 0.0, layout.row.w, layout.row.h));
        gfx::set_gradient_h(&row_fade, 0.0, 0.0);
        gfx::set_mask(&row, &row_fade);
        let cards = kinds.iter().map(|k| CardView { kind: *k, canvas: Canvas::new(&row, scale), draw: CardDraw::default() }).collect();
        let mut hp = HomePanel { group, _row: row, row_fade, layout, specs, cards, hover: None };
        Self::place_cards(p, &mut hp);
        p.home = Some(hp);

        // Switcher.
        let bg = mk(&p.inner);
        let sw = geo::switcher(&p.layout.header, &self.metrics);
        gfx::set_frame(&bg, Self::local(p, sw.frame));
        gfx::set_corner(&bg, sw.frame.h / 2.0);
        gfx::set_bg(&bg, Some(&white(0.09)));
        let knob = mk(&bg);
        gfx::set_corner(&knob, (sw.frame.h - 4.0) / 2.0);
        gfx::set_bg(&knob, Some(&white(0.2)));
        gfx::set_shadow(&knob, 0.25, 2.0, -1.0);
        let icons = [mk(&bg), mk(&bg)];
        p.switch = Some(SwitchView { bg, knob, icons, shown: None });
    }

    fn place_cards(p: &Panel, hp: &mut HomePanel) {
        let row = hp.layout.row;
        for (c, r) in hp.cards.iter_mut().zip(&hp.layout.cards.cards) {
            c.canvas.set_frame(r.offset(-row.x, -row.y));
        }
        let fade = 22.0 / row.w.max(1.0);
        let l = &hp.layout.cards;
        let left = if l.scroll > 0.5 { fade } else { 0.0 };
        let right = if l.scroll < l.max_scroll - 0.5 { fade } else { 0.0 };
        transaction(None, || gfx::set_gradient_h(&hp.row_fade, left, right));
        let _ = p;
    }

    // ----- data -------------------------------------------------------------

    /// Pull provider data, decode artwork, read the host's toggles.
    pub(super) fn sync_widgets(&mut self) {
        let g = self.shared.data_gen.load(Ordering::Relaxed);
        if g != self.w.data_gen {
            self.w.data_gen = g;
            self.w.data = self.shared.data().clone();
            self.w.data.camera_access = self.w.camera_access;
            let key = self.w.data.now_playing.as_ref().map(|n| n.artwork_key()).unwrap_or(0);
            if self.w.artwork.as_ref().map(|(k, _)| *k) != Some(key) {
                let img = self.w.data.now_playing.as_ref().and_then(|n| n.artwork.as_ref()).and_then(|b| paint::decode_image(b));
                self.w.artwork = img.map(|i| (key, i));
            }
        }
        let t = mono();
        if t - self.w.offset_at > 60.0 {
            self.w.offset_at = t;
            self.w.utc_offset = rt::utc_offset();
        }
        if self.w.controls.len() != self.n_controls() || (self.shared.home_visible.load(Ordering::Relaxed) && t - self.w.controls_at > 1.0) {
            self.read_controls();
        }
        if let Some(a) = self.w.camera.poll() {
            self.w.camera_access = a;
            self.w.data.camera_access = a;
        }
        if let Some(at) = self.w.note_dirty {
            if t - at > 1.0 {
                self.save_note();
            }
        }
        // Timer.
        if self.w.timer.tick(t) == Some(TimerEvent::Finished) {
            rt::play_sound("Glass");
            log::info!("notch-shelf: timer finished");
            if !self.cfg.live_activities {
                self.w.peek_timer = true;
                let now = now();
                for p in &mut self.panels {
                    p.logic.peek(now, 3.5);
                }
            }
        }
    }

    pub(super) fn read_controls(&mut self) {
        self.w.controls_at = mono();
        let host = &self.shared.host;
        let mut v: Vec<Control> = host.toggles.iter().map(|t| Control { title: t.title.clone(), symbol: t.sf_symbol.clone(), on: Some((t.get)()) }).collect();
        v.extend(host.actions.iter().map(|a| Control { title: a.title.clone(), symbol: a.sf_symbol.clone(), on: None }));
        self.w.controls = v;
    }

    fn save_note(&mut self) {
        self.w.note_dirty = None;
        if let Some(i) = self.w.editing {
            if let Some(nv) = self.panels.get(i).and_then(|p| p.note_view.as_ref()) {
                let s: Retained<AnyObject> = unsafe { msg_send![&*nv.text, string] };
                self.w.note = rt::string_of(Some(&s)).chars().take(NOTE_MAX).collect();
            }
        }
        if let Err(e) = std::fs::write(&self.w.note_path, &self.w.note) {
            log::warn!("notch-shelf: saving note: {e}");
        }
    }

    // ----- views ------------------------------------------------------------

    pub(super) fn has_home(&self) -> bool {
        self.panels.iter().any(|p| p.home.is_some())
    }

    /// The view to open in.
    pub(super) fn opening_view(&self, by_drag: bool) -> View {
        if by_drag || !self.has_home() {
            return View::Shelf;
        }
        match self.cfg.default_view {
            DefaultView::Last => self.w.last_view,
            DefaultView::Home => View::Home,
            DefaultView::Shelf => View::Shelf,
        }
    }

    /// Switch every panel to `view`; `remember` makes it the last view.
    pub(super) fn apply_view(&mut self, view: View, remember: bool) {
        let view = if self.has_home() { view } else { View::Shelf };
        if remember && self.w.last_view != view {
            self.w.last_view = view;
            self.w.save_state();
        }
        if view == self.w.view && self.panels.iter().all(|p| p.expanded == if view == View::Home { p.home_rect.unwrap_or(p.shelf_rect) } else { p.shelf_rect }) {
            return;
        }
        self.w.view = view;
        if view != View::Home {
            self.end_note_edit();
            self.w.camera.stop();
        }
        let m = self.metrics;
        for p in &mut self.panels {
            p.expanded = match (view, p.home_rect) {
                (View::Home, Some(h)) => h,
                _ => p.shelf_rect,
            };
            if p.mode == Mode::Expanded {
                p.from = p.shape_now;
                p.to = geo::expanded_shape(&p.expanded, &m);
                let k = 360.0;
                p.spring = Spring::new(k, Spring::damping_for_ratio(k, 0.8), 0.0);
                p.spring.target = 1.0;
            }
        }
    }

    /// Resting shape while collapsed (widened when a live activity shows).
    pub(super) fn rest_shape(&self, p: &Panel) -> ShapeParams {
        let m = self.metrics;
        match p.live.as_ref().and_then(|l| l.rect) {
            Some(r) => ShapeParams { rect: r, shoulder: m.shoulder * 0.8, radius: (r.h / 2.0).min(m.collapsed_radius + 4.0) },
            None => geo::collapsed_shape(&p.collapsed, &m),
        }
    }

    fn ctx_parts(&self) -> (f64, f64) {
        (mono(), crate::widgets::unix_now())
    }

    // ----- per frame ----------------------------------------------------------

    /// Live activity state of panel `i` (call before stepping the spring).
    pub(super) fn update_live(&mut self, i: usize, dt: f64) {
        let (mono_now, unix_now) = self.ctx_parts();
        let live = if self.cfg.live_activities {
            let ctx = self.ctx(mono_now, unix_now, None);
            let mut l = draw::live_activity(&ctx);
            if matches!(l, Some(Live::Music { .. })) && !self.cfg.widget_enabled(WidgetKind::NowPlaying) {
                l = None;
            }
            l
        } else {
            None
        };
        let m = self.metrics;
        let p = &mut self.panels[i];
        let Some(lv) = p.live.as_mut() else { return };
        let rect = live.as_ref().map(|_| Rect::new(p.collapsed.x - LIVE_WING, p.collapsed.y, p.collapsed.w + 2.0 * LIVE_WING, p.collapsed.h));
        lv.rect = rect;
        lv.live = live;
        // Retarget the resting shape while collapsed.
        if p.mode == Mode::Collapsed {
            let rest = match rect {
                Some(r) => ShapeParams { rect: r, shoulder: m.shoulder * 0.8, radius: (r.h / 2.0).min(m.collapsed_radius + 4.0) },
                None => geo::collapsed_shape(&p.collapsed, &m),
            };
            if p.to != rest {
                p.from = p.shape_now;
                p.to = rest;
                let (k, z) = if rect.is_some() { (380.0, 0.68) } else { (420.0, 1.0) };
                p.spring = Spring::new(k, Spring::damping_for_ratio(k, z), 0.0);
                p.spring.target = 1.0;
            }
        }
        let target = if p.mode == Mode::Collapsed && lv.live.is_some() && p.spring.value > 0.55 { 1.0 } else { 0.0 };
        lv.alpha = approach(lv.alpha, target, dt, if target > lv.alpha { 0.07 } else { 0.03 });
        let alpha = lv.alpha;
        transaction(None, || gfx::set_opacity(&lv.canvas.root, alpha));
        if alpha <= 0.0 {
            return;
        }
        let (Some(live), Some(r)) = (lv.live.clone(), lv.rect) else { return };
        let off = Point::new(p.win.x, p.win.y);
        let left = Rect::new(r.x + m.shoulder, r.y, LIVE_WING - m.shoulder - 2.0, r.h).offset(-off.x, -off.y);
        let right = Rect::new(p.collapsed.right() + 2.0, r.y, LIVE_WING - m.shoulder - 2.0, r.h).offset(-off.x, -off.y);
        let prims = draw::live_prims(&live, left, right, self.accent_rgba(), mono_now);
        let p = &mut self.panels[i];
        let lv = p.live.as_mut().unwrap();
        let mut env = Env { scale: p.scale, symbols: &mut self.symbols, fonts: &mut self.w.fonts, artwork: self.w.artwork.as_ref().map(|(_, i)| i), artwork_key: self.w.artwork.as_ref().map(|(k, _)| *k).unwrap_or(0), camera: None };
        lv.canvas.paint(&prims, &mut env);
    }

    pub(super) fn accent_rgba(&self) -> crate::widgets::Rgba {
        crate::widgets::Rgba(self.accent.0, self.accent.1, self.accent.2, 1.0)
    }

    fn ctx<'a>(&'a self, mono_now: f64, unix_now: f64, hover: Option<&'a Act>) -> Ctx<'a> {
        Ctx {
            data: &self.w.data,
            timer: &self.w.timer,
            mono_now,
            unix_now,
            utc_offset: self.w.utc_offset,
            accent: self.accent_rgba(),
            controls: &self.w.controls,
            hover,
            note: &self.w.note,
            editing_note: self.w.editing.is_some(),
            camera_on: self.w.camera.running(),
            calendar_days: self.cfg.calendar.days.clamp(1, 7),
            calendar_filter: &self.cfg.calendar.calendars,
            weather_enabled: self.cfg.widget_enabled(WidgetKind::Weather),
            use_applescript: self.cfg.now_playing.use_applescript,
        }
    }

    /// Hover, crossfade and card drawing of the Home view.
    pub(super) fn update_home(&mut self, i: usize, cur: Point, dt: f64) {
        let home_target = if self.w.view == View::Home && self.panels[i].home.is_some() { 1.0 } else { 0.0 };
        {
            let p = &mut self.panels[i];
            p.view_mix = approach(p.view_mix, home_target, dt, 0.06);
            let mix = p.view_mix;
            transaction(None, || {
                gfx::set_opacity(&p.shelf_group, 1.0 - mix);
                gfx::set_hidden(&p.shelf_group, mix >= 1.0);
                if let Some(h) = &p.home {
                    gfx::set_opacity(&h.group, mix);
                    gfx::set_hidden(&h.group, mix <= 0.0);
                }
            });
        }
        self.update_switch(i);
        let p = &self.panels[i];
        let Some(hp) = &p.home else { return };
        if p.mode != Mode::Expanded && p.content_alpha <= 0.0 {
            return;
        }
        // Hover.
        let hover = if p.mode == Mode::Expanded && self.w.view == View::Home && self.press.is_none() {
            hp.layout.cards.cards.iter().zip(&hp.cards).find(|(r, _)| r.contains(cur) && hp.layout.row.contains(cur)).and_then(|(r, c)| c.draw.hit_at(Point::new(cur.x - r.x, cur.y - r.y)).cloned())
        } else {
            None
        };
        let hover = self.w.forced_hover.clone().or(hover);
        let (mono_now, unix_now) = self.ctx_parts();
        let draws: Vec<CardDraw> = {
            let ctx = self.ctx(mono_now, unix_now, hover.as_ref());
            hp.cards.iter().zip(&hp.layout.cards.cards).map(|(c, r)| draw::card(c.kind, r.w, r.h, &ctx)).collect()
        };
        let camera = self.w.camera.preview.clone();
        let p = &mut self.panels[i];
        let hp = p.home.as_mut().unwrap();
        hp.hover = hover;
        let mut env = Env {
            scale: p.scale,
            symbols: &mut self.symbols,
            fonts: &mut self.w.fonts,
            artwork: self.w.artwork.as_ref().map(|(_, i)| i),
            artwork_key: self.w.artwork.as_ref().map(|(k, _)| *k).unwrap_or(0),
            camera: camera.as_deref(),
        };
        for (c, d) in hp.cards.iter_mut().zip(draws) {
            c.canvas.paint(&d.prims, &mut env);
            c.draw = d;
        }
        // Keep the note editor over its card.
        if self.w.editing == Some(i) {
            self.place_note_view(i);
        }
    }

    /// The switcher of panel `p` for the panel rect `r` (current view's
    /// expanded rect for hit testing, the animated shape for drawing).
    fn switch_geom(&self, p: &Panel, r: Rect) -> geo::Switcher {
        geo::switcher(&Rect::new(r.x, r.y, r.w, geo::header_height(&p.collapsed, &self.metrics)), &self.metrics)
    }

    fn update_switch(&mut self, i: usize) {
        let view = self.w.view;
        let scale = self.panels[i].scale;
        let p = &self.panels[i];
        let Some(sv) = &p.switch else { return };
        // Follow the panel's left edge as it springs between the views.
        let r = if p.mode == Mode::Expanded { p.shape_now.rect } else { p.expanded };
        let f = Self::local(p, self.switch_geom(p, Rect::new(r.x, p.expanded.y, r.w, r.h)).frame);
        transaction(None, || gfx::set_frame(&sv.bg, f));
        if sv.shown == Some(view) {
            return;
        }
        let first = sv.shown.is_none();
        let sym = |a: &mut App, name: &str, on: bool| a.sym(name, 11.5, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, if on { 1.0 } else { 0.5 }), scale);
        let home_icon = sym(self, "house.fill", view == View::Home);
        let shelf_icon = sym(self, if self.display.iter().any(|d| d.id != CLIP_ID) { "tray.full.fill" } else { "tray.fill" }, view == View::Shelf);
        let sw = self.switch_geom(&self.panels[i], self.panels[i].expanded);
        let p = &mut self.panels[i];
        let sv = p.switch.as_mut().unwrap();
        sv.shown = Some(view);
        let o = Point::new(sw.frame.x, sw.frame.y);
        let seg = |r: Rect| r.offset(-o.x, -o.y);
        let knob = if view == View::Home { seg(sw.home) } else { seg(sw.shelf) };
        transaction(if first { None } else { Some(0.22) }, || gfx::set_frame(&sv.knob, knob));
        transaction(None, || {
            gfx::place_symbol(&sv.icons[0], home_icon.as_ref(), seg(sw.home));
            gfx::place_symbol(&sv.icons[1], shelf_icon.as_ref(), seg(sw.shelf));
        });
    }

    /// Force the switcher icons to redraw (item count changed).
    pub(super) fn refresh_switch(&mut self) {
        for p in &mut self.panels {
            if let Some(s) = p.switch.as_mut() {
                s.shown = None;
            }
        }
    }

    // ----- input --------------------------------------------------------------

    /// Mouse down in the expanded panel; true if the Home view handled it.
    pub(super) fn on_home_mouse_down(&mut self, i: usize, pt: Point) -> bool {
        let p = &self.panels[i];
        if let Some(v) = p.switch.as_ref().and_then(|_| self.switch_geom(p, p.expanded).hit(pt)) {
            self.apply_view(v, true);
            self.selected.clear();
            return true;
        }
        if self.w.view != View::Home || p.home.is_none() {
            return false;
        }
        let hp = p.home.as_ref().unwrap();
        let act = hp
            .layout
            .cards
            .cards
            .iter()
            .zip(&hp.cards)
            .find(|(r, _)| r.contains(pt) && hp.layout.row.contains(pt))
            .and_then(|(r, c)| c.draw.hit_at(Point::new(pt.x - r.x, pt.y - r.y)).cloned());
        if let Some(a) = act {
            self.perform(i, a);
        } else if self.w.editing.is_some() {
            self.end_note_edit();
        }
        true
    }

    pub(super) fn on_home_scroll(&mut self, i: usize, d: f64) -> bool {
        if self.w.view != View::Home {
            return false;
        }
        let m = self.metrics;
        let p = &mut self.panels[i];
        let Some(hp) = p.home.as_mut() else { return false };
        let max = hp.layout.cards.max_scroll;
        let s = (hp.layout.cards.scroll - d).clamp(0.0, max);
        if s != hp.layout.cards.scroll {
            hp.layout = geo::home_layout(&hp.layout.panel, &p.collapsed, &hp.specs, s, &m);
            let mut hp2 = p.home.take().unwrap();
            Self::place_cards(p, &mut hp2);
            p.home = Some(hp2);
        }
        true
    }

    fn collapse_all(&mut self) {
        for p in &mut self.panels {
            p.hold = false;
            p.logic.collapse(true);
        }
    }

    pub(super) fn perform(&mut self, i: usize, act: Act) {
        let t = mono();
        if !matches!(act, Act::EditNote) {
            self.end_note_edit();
        }
        match act {
            Act::Media(cmd) => {
                if cmd == Command::PlayPause {
                    for d in [&mut self.w.data, &mut *self.shared.data()] {
                        if let Some(n) = d.now_playing.as_mut() {
                            if n.has_track() {
                                if let Some(e) = crate::widgets::media::elapsed_at(n, crate::widgets::unix_now()) {
                                    n.elapsed = Some(e);
                                    n.sampled_at = crate::widgets::unix_now();
                                }
                                n.playing = !n.playing;
                            }
                        }
                    }
                }
                if !self.w.demo {
                    self.shared.reqs().media.push(cmd);
                    self.shared.signal.notify();
                }
            }
            Act::OpenEvent(id) => {
                let ev = self.w.data.events.iter().find(|e| e.id == id).cloned();
                match ev.and_then(|e| crate::widgets::calendar::open_url(&e)) {
                    Some(url) => rt::open_url(&url),
                    None => mac_cal::open_app(),
                }
            }
            Act::OpenCalendar => mac_cal::open_app(),
            Act::CalendarAllow => {
                self.shared.reqs().calendar_ask = true;
                self.shared.signal.notify();
            }
            Act::OpenPrivacy(pane) => rt::open_url(&format!("x-apple.systempreferences:com.apple.preference.security?Privacy_{pane}")),
            Act::TimerPreset(secs) => self.w.timer.start(secs as f64, t),
            Act::TimerStopwatch => self.w.timer.start_stopwatch(t),
            Act::TimerPause => self.w.timer.toggle_pause(t),
            Act::TimerReset => self.w.timer.reset(),
            Act::TimerAdd(secs) => self.w.timer.add(secs as f64, t),
            Act::Control(k) => {
                let nt = self.shared.host.toggles.len();
                if k < nt {
                    let tg = self.shared.host.toggles[k].clone();
                    let on = self.w.controls.get(k).and_then(|c| c.on).unwrap_or(false);
                    if let Some(c) = self.w.controls.get_mut(k) {
                        c.on = Some(!on);
                    }
                    // Re-read after the host had time to apply it.
                    self.w.controls_at = t + 0.5;
                    dispatch_main(move || (tg.set)(!on));
                } else if let Some(a) = self.shared.host.actions.get(k - nt).cloned() {
                    self.collapse_all();
                    dispatch_main(move || (a.run)());
                }
            }
            Act::EditNote => self.begin_note_edit(i),
            Act::Mirror => {
                if self.w.camera.running() || self.w.camera.want {
                    self.w.camera.stop();
                } else {
                    self.w.camera_access = self.w.camera.start();
                    self.w.data.camera_access = self.w.camera_access;
                }
                self.shared.data().camera_access = self.w.camera_access;
            }
            Act::WeatherRetry => {
                self.shared.reqs().refresh_weather = true;
                self.shared.signal.notify();
            }
        }
    }

    /// The panel collapsed / went away: stop the camera, end editing.
    pub(super) fn home_closed(&mut self) {
        if self.panels.iter().all(|p| p.mode != Mode::Expanded) {
            self.w.camera.stop();
            self.end_note_edit();
        }
    }

    // ----- note editor ----------------------------------------------------------

    fn note_rect(&self, i: usize) -> Option<Rect> {
        let p = self.panels.get(i)?;
        let hp = p.home.as_ref()?;
        let k = hp.cards.iter().position(|c| c.kind == WidgetKind::Note)?;
        let r = hp.layout.cards.cards[k];
        let tr = draw::note_text_rect(r.w, r.h).offset(r.x, r.y);
        Some(Self::local(p, tr))
    }

    fn place_note_view(&mut self, i: usize) {
        let Some(r) = self.note_rect(i) else { return };
        let p = &self.panels[i];
        if let Some(nv) = &p.note_view {
            let f = NSRect::new(NSPoint::new(r.x, p.win.h - (r.y + r.h)), NSSize::new(r.w, r.h));
            unsafe {
                let _: () = msg_send![&*nv.scroll, setFrame: f];
            }
        }
    }

    fn begin_note_edit(&mut self, i: usize) {
        if self.w.editing.is_some() || self.note_rect(i).is_none() {
            return;
        }
        let accent = self.accent;
        let p = &mut self.panels[i];
        if p.note_view.is_none() {
            p.note_view = Some(unsafe { make_note_view(&p.content_view, &p.view, accent) });
        }
        let nv = p.note_view.as_ref().unwrap();
        unsafe {
            let _: () = msg_send![&*nv.text, setString: &*ns(&self.w.note)];
            let _: () = msg_send![&*nv.scroll, setHidden: false];
        }
        p.window.set_key_ok(true);
        p.window.makeKeyWindow();
        unsafe {
            let _: bool = msg_send![&*p.window, makeFirstResponder: &*nv.text];
            // Caret at the end.
            let s: Retained<AnyObject> = msg_send![&*nv.text, string];
            let len: usize = msg_send![&*s, length];
            let _: () = msg_send![&*nv.text, setSelectedRange: objc2_foundation::NSRange::new(len, 0)];
        }
        self.w.editing = Some(i);
        self.place_note_view(i);
        log::info!("notch-shelf: editing note (panel key: {})", self.panels[i].window.isKeyWindow());
    }

    pub fn end_note_edit(&mut self) {
        let Some(i) = self.w.editing else { return };
        self.save_note();
        self.w.editing = None;
        let Some(p) = self.panels.get(i) else { return };
        if let Some(nv) = &p.note_view {
            unsafe {
                let _: () = msg_send![&*nv.scroll, setHidden: true];
            }
        }
        p.window.set_key_ok(false);
        if p.window.isKeyWindow() {
            unsafe {
                let _: bool = msg_send![&*p.window, makeFirstResponder: std::ptr::null::<AnyObject>()];
            }
            // Give key focus back to the app the user was in.
            p.window.orderOut(None);
            p.window.orderFrontRegardless();
        }
        log::info!("notch-shelf: note edit ended (panel key: {})", p.window.isKeyWindow());
    }

    pub fn on_note_changed(&mut self) {
        self.w.note_dirty = Some(mono());
    }

    /// While editing: stop when the window lost key status.
    pub(super) fn check_note_focus(&mut self) {
        if let Some(i) = self.w.editing {
            if self.panels.get(i).is_none_or(|p| !p.window.isKeyWindow()) {
                self.end_note_edit();
            }
        }
    }

    // ----- diagnostics ------------------------------------------------------------

    /// Widget debug commands (see `Handle::debug`); true if handled.
    pub(super) fn debug_widgets(&mut self, cmd: &str) -> bool {
        let t = mono();
        let arg = |c: &str| c.split_once(':').map(|(_, a)| a.to_string()).unwrap_or_default();
        match cmd {
            "demo" => {
                self.w.demo = true;
                self.shared.demo.store(true, Ordering::Relaxed);
                let art = demo_artwork();
                let d = crate::widgets::demo::data(crate::widgets::unix_now(), self.w.utc_offset, art);
                *self.shared.data() = d;
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
                self.sync_widgets();
                self.rebuild_requested = true;
            }
            "timerDone" => {
                self.w.timer.start(1.0, t - 2.0);
                self.sync_widgets();
            }
            "timerReset" => self.w.timer.reset(),
            "pause" => {
                if let Some(n) = self.shared.data().now_playing.as_mut() {
                    n.playing = false;
                }
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
            }
            "editNote" => {
                if let Some(i) = (0..self.panels.len()).find(|i| self.note_rect(*i).is_some()) {
                    self.begin_note_edit(i);
                }
            }
            "endEdit" => self.end_note_edit(),
            c if c.starts_with("timer:") => {
                // timer:TOTAL[:ELAPSED]
                let a = arg(c);
                let mut it = a.split(':').map(|v| v.parse::<f64>().unwrap_or(0.0));
                let total = it.next().unwrap_or(300.0);
                let el = it.next().unwrap_or(0.0);
                self.w.timer.start(total, t - el);
            }
            c if c.starts_with("stopwatch:") => {
                let el: f64 = arg(c).parse().unwrap_or(0.0);
                self.w.timer.start_stopwatch(t - el);
            }
            c if c.starts_with("hoverControl:") => self.w.forced_hover = arg(c).parse().ok().map(Act::Control),
            "noHover" => self.w.forced_hover = None,
            c if c.starts_with("note:") => {
                self.w.note = arg(c).replace("\\n", "\n");
                self.save_note();
            }
            c if c.starts_with("calendar:") => {
                let a = match arg(c).as_str() {
                    "denied" => Access::Denied,
                    "ask" => Access::NotDetermined,
                    _ => Access::Granted,
                };
                self.shared.data().calendar_access = a;
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
            }
            "weatherOff" => {
                self.shared.data().weather = crate::widgets::WeatherStatus::NoLocation;
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
            }
            _ => return false,
        }
        true
    }
}

/// A desktop picture thumbnail as fake album art for demo renders.
fn demo_artwork() -> Option<Vec<u8>> {
    let dir = std::path::Path::new("/System/Library/Desktop Pictures/.thumbnails");
    let mut files: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "heic" || e == "jpg" || e == "png")).collect();
    files.sort();
    let pick = files.iter().find(|p| p.to_string_lossy().contains("Sequoia")).or(files.first())?;
    std::fs::read(pick).ok()
}

unsafe fn make_note_view(container: &AnyObject, shelf_view: &AnyObject, accent: (f64, f64, f64)) -> NoteView {
    let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 60.0));
    let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSScrollView), alloc];
    let scroll: Retained<AnyObject> = msg_send![a, initWithFrame: zero];
    let _: () = msg_send![&*scroll, setDrawsBackground: false];
    let _: () = msg_send![&*scroll, setHasVerticalScroller: false];
    let _: () = msg_send![&*scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![&*scroll, setBorderType: 0usize];
    let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSTextView), alloc];
    let text: Retained<AnyObject> = msg_send![a, initWithFrame: zero];
    let _: () = msg_send![&*text, setDrawsBackground: false];
    let _: () = msg_send![&*text, setRichText: false];
    let _: () = msg_send![&*text, setAllowsUndo: true];
    let _: () = msg_send![&*text, setFocusRingType: 1usize];
    let _: () = msg_send![&*text, setVerticallyResizable: true];
    let _: () = msg_send![&*text, setHorizontallyResizable: false];
    let _: () = msg_send![&*text, setAutoresizingMask: 2usize]; // width sizable
    let _: () = msg_send![&*text, setTextContainerInset: NSSize::new(0.0, 0.0)];
    let tc: Retained<AnyObject> = msg_send![&*text, textContainer];
    let _: () = msg_send![&*tc, setLineFragmentPadding: 0.0f64];
    let _: () = msg_send![&*tc, setWidthTracksTextView: true];
    let font: Retained<AnyObject> = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
    let _: () = msg_send![&*text, setFont: &*font];
    let col: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: 1.0f64, green: 1.0f64, blue: 1.0f64, alpha: 0.88f64];
    let _: () = msg_send![&*text, setTextColor: &*col];
    let ac: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: accent.0, green: accent.1, blue: accent.2, alpha: 1.0f64];
    let _: () = msg_send![&*text, setInsertionPointColor: &*ac];
    let _: () = msg_send![&*text, setDelegate: shelf_view];
    let _: () = msg_send![&*scroll, setDocumentView: &*text];
    let _: () = msg_send![&*scroll, setHidden: true];
    let _: () = msg_send![container, addSubview: &*scroll];
    NoteView { scroll, text }
}
