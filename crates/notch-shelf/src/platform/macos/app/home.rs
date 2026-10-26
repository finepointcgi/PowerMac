//! The Home view (widget cards on named pages), the Home / Shelf switcher,
//! the page tabs, live activities beside the collapsed notch, the note
//! editor and the camera mirror.

use std::collections::HashMap;
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
use crate::widgets::catalog::{ClickAction, KindSettings};
use crate::widgets::content::{ItemAction, Status};
use crate::widgets::draw::{self, Act, Card, CardDraw, CardUi, Control, Ctx, ImageKey, ImageSource, Live};
use crate::widgets::instance::{legacy_id, WidgetInstance};
use crate::widgets::layout::{self as wl, CardSpec};
use crate::widgets::media::Command;
use crate::widgets::timer::TimerEvent;
use crate::widgets::{Access, DefaultView, InstanceData, View, WidgetData, WidgetKind};
use crate::SoundCmd;

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
    last_page: Option<usize>,
}

/// One enabled widget instance and its UI state.
pub(in super::super) struct CardModel {
    pub inst: WidgetInstance,
    pub settings: KindSettings,
    pub ui: CardUi,
    /// Where a Note instance keeps its text.
    pub note_path: Option<PathBuf>,
}

impl CardModel {
    fn card(&self) -> Card<'_> {
        Card { id: &self.inst.id, kind: self.inst.kind, size: self.inst.effective_size(), settings: &self.settings, ui: &self.ui }
    }
}

/// The volume slider being dragged.
#[derive(Clone, Copy)]
pub(in super::super) struct SliderDrag {
    pub model: usize,
    /// Track ends in global coordinates.
    pub x0: f64,
    pub x1: f64,
    pub last_sent: f64,
}

/// A horizontal swipe in progress (page switching).
#[derive(Default)]
pub(in super::super) struct Gesture {
    pub acc: f64,
    pub switched: bool,
    pub last: f64,
}

/// App-wide widget state.
pub(in super::super) struct Widgets {
    pub view: View,
    pub last_view: View,
    /// Home page on screen (index into `Config::pages`).
    pub page: usize,
    pub models: Vec<CardModel>,
    models_src: Vec<WidgetInstance>,
    pub data: WidgetData,
    pub data_gen: u64,
    pub controls: Vec<Control>,
    pub controls_at: f64,
    pub artwork: Option<(u64, gfx::Image)>,
    pub images: HashMap<ImageKey, Option<gfx::Image>>,
    pub images_gen: u64,
    pub fonts: FontCache,
    pub note_dirty: Option<f64>,
    /// (panel, model) of the note being edited.
    pub editing: Option<(usize, usize)>,
    pub camera: Camera,
    pub camera_access: Access,
    pub utc_offset: f64,
    pub offset_at: f64,
    pub hour24: bool,
    pub forced_hover: Option<Act>,
    pub state_path: PathBuf,
    pub data_dir: PathBuf,
    pub peek_timer: bool,
    pub demo: bool,
    pub battery_present: bool,
    pub slider: Option<SliderDrag>,
    pub gesture: Gesture,
    /// The Home page tabs' click targets need the page list.
    pub n_pages: usize,
}

impl Widgets {
    pub fn new(data_dir: &std::path::Path) -> Widgets {
        let state_path = data_dir.join(STATE_FILE);
        let p: Persisted = std::fs::read(&state_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let last_view = p.last_view.unwrap_or(View::Home);
        Widgets {
            view: last_view,
            last_view,
            page: p.last_page.unwrap_or(0),
            models: vec![],
            models_src: vec![],
            data: WidgetData::default(),
            data_gen: 0,
            controls: vec![],
            controls_at: f64::NEG_INFINITY,
            artwork: None,
            images: HashMap::new(),
            images_gen: 1,
            fonts: FontCache::default(),
            note_dirty: None,
            editing: None,
            camera: Camera::new(),
            camera_access: Access::Unknown,
            utc_offset: rt::utc_offset(),
            offset_at: mono(),
            hour24: rt::uses_24h(),
            forced_hover: None,
            state_path,
            data_dir: data_dir.to_path_buf(),
            peek_timer: false,
            demo: false,
            battery_present: sysinfo::battery().is_some(),
            slider: None,
            gesture: Gesture::default(),
            n_pages: 1,
        }
    }

    fn save_state(&self) {
        let p = Persisted { last_view: Some(self.last_view), last_page: Some(self.page) };
        if let Ok(b) = serde_json::to_vec_pretty(&p) {
            let _ = std::fs::write(&self.state_path, b);
        }
    }

    fn note_path_for(&self, id: &str) -> PathBuf {
        // The note migrated from the single-note era keeps its file.
        if id == legacy_id(WidgetKind::Note) {
            self.data_dir.join(NOTE_FILE)
        } else {
            let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            self.data_dir.join(format!("notch-note-{safe}.txt"))
        }
    }
}

pub(in super::super) struct CardView {
    pub model: usize,
    pub canvas: Canvas,
    pub draw: CardDraw,
}

/// One Home page: its clipped row and cards.
pub(in super::super) struct PageView {
    /// Index into `Config::pages`.
    pub page: usize,
    pub row: Layer,
    pub fade: Layer,
    pub layout: wl::CardsLayout,
    pub specs: Vec<CardSpec>,
    pub cards: Vec<CardView>,
    pub alpha: f64,
}

pub(in super::super) struct HomePanel {
    pub group: Layer,
    /// Clips the sliding pages to the card row.
    pub _viewport: Layer,
    /// The card row (global coordinates).
    pub row: Rect,
    pub pages: Vec<PageView>,
    /// Index into `pages` of the page shown.
    pub cur: usize,
    /// Animated page position (fractional while sliding).
    pub pos: Spring,
    /// (card index on the current page, act) under the pointer.
    pub hover: Option<(usize, Act)>,
    pub tabs: Canvas,
    /// Tab / dot frames (global coordinates), one per page.
    pub tab_frames: Vec<Rect>,
    pub dots: bool,
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
    // ----- models -------------------------------------------------------------

    /// Rebuild the card models after a config change (UI state of
    /// unchanged instances is kept).
    pub(super) fn sync_models(&mut self) {
        let src: Vec<WidgetInstance> = self.cfg.widgets.iter().filter(|w| w.enabled && w.kind != WidgetKind::Unknown).cloned().collect();
        self.w.n_pages = self.cfg.page_list().len();
        if src == self.w.models_src {
            return;
        }
        self.end_note_edit();
        let mut old: HashMap<String, CardModel> = self.w.models.drain(..).map(|m| (m.inst.id.clone(), m)).collect();
        let mut models = vec![];
        for inst in &src {
            let settings = KindSettings::parse(inst.kind, &inst.settings);
            let m = match old.remove(&inst.id) {
                Some(mut m) if m.inst.kind == inst.kind => {
                    m.inst = inst.clone();
                    m.settings = settings;
                    m
                }
                _ => {
                    let note_path = (inst.kind == WidgetKind::Note).then(|| self.w.note_path_for(&inst.id));
                    let note = note_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
                    CardModel { inst: inst.clone(), settings, ui: CardUi { note, ..Default::default() }, note_path }
                }
            };
            models.push(m);
        }
        self.w.models = models;
        self.w.models_src = src;
        self.rebuild_requested = true;
    }

    pub(super) fn n_controls(&self) -> usize {
        self.shared.host.toggles.len() + self.shared.host.actions.len()
    }

    /// Pages with something to show: (page index, model indexes).
    pub(super) fn home_pages(&self) -> Vec<(usize, Vec<usize>)> {
        let n = self.cfg.page_list().len();
        let has_batt = self.w.battery_present || self.w.data.battery.is_some();
        let nc = self.n_controls();
        (0..n)
            .map(|p| {
                let ms = self.w.models.iter().enumerate().filter(|(_, m)| m.inst.page.min(n - 1) == p && draw::kind_visible(m.inst.kind, has_batt, nc)).map(|(i, _)| i).collect::<Vec<_>>();
                (p, ms)
            })
            .filter(|(_, ms)| !ms.is_empty())
            .collect()
    }

    fn specs_of(&self, models: &[usize]) -> Vec<CardSpec> {
        models.iter().map(|&i| wl::fixed(self.w.models[i].inst.effective_size().width())).collect()
    }

    /// The Home view's panel rect for a collapsed rect, if there's a Home view.
    pub(super) fn home_rect_for(&self, collapsed: &Rect, screen: &Rect) -> Option<Rect> {
        let pages = self.home_pages();
        if pages.is_empty() {
            return None;
        }
        let specs: Vec<Vec<CardSpec>> = pages.iter().map(|(_, ms)| self.specs_of(ms)).collect();
        let m = self.metrics;
        Some(geo::home_rect(collapsed, screen, &m, wl::pages_width(&specs, m.side_pad, m.panel_width)))
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
        let group = mk(&p.inner);
        gfx::set_frame(&group, bounds);
        let pages_src = self.home_pages();
        let base = geo::home_layout(&home_rect, &p.collapsed, &[], 0.0, &self.metrics);
        let row = base.row;
        let viewport = mk(&group);
        // A little room above / below so card edges and shadows aren't cut.
        gfx::set_frame(&viewport, Self::local(p, row.inflate(0.0, 2.0)));
        gfx::set_masks_to_bounds(&viewport, true);
        let mut pages = vec![];
        for (page, models) in &pages_src {
            let specs = self.specs_of(models);
            let layout = wl::layout_cards(row, &specs, 0.0);
            let r = mk(&viewport);
            gfx::set_frame(&r, Rect::new(0.0, 2.0, row.w, row.h));
            gfx::set_masks_to_bounds(&r, true);
            let fade = gfx::gradient_layer();
            gfx::set_frame(&fade, Rect::new(0.0, 0.0, row.w, row.h));
            gfx::set_gradient_h(&fade, 0.0, 0.0);
            gfx::set_mask(&r, &fade);
            let cards = models.iter().map(|&mi| CardView { model: mi, canvas: Canvas::new(&r, scale), draw: CardDraw::default() }).collect();
            pages.push(PageView { page: *page, row: r, fade, layout, specs, cards, alpha: 1.0 });
        }
        let cur = pages.iter().position(|pv| pv.page == self.w.page).unwrap_or(0);
        if let Some(pv) = pages.get(cur) {
            self.w.page = pv.page;
        }
        let k = 260.0;
        let mut pos = Spring::new(k, Spring::damping_for_ratio(k, 0.86), cur as f64);
        pos.target = cur as f64;
        let tabs = Canvas::new(&group, scale);
        let mut hp = HomePanel { group, _viewport: viewport, row, pages, cur, pos, hover: None, tabs, tab_frames: vec![], dots: false };
        for k in 0..hp.pages.len() {
            Self::place_cards(&mut hp, k);
        }
        Self::place_pages(&mut hp);
        self.layout_tabs(p, &mut hp);
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

    /// Card frames of page `k` inside its row, and the row's edge fades.
    fn place_cards(hp: &mut HomePanel, k: usize) {
        let row = hp.row;
        let pv = &mut hp.pages[k];
        for (c, r) in pv.cards.iter_mut().zip(&pv.layout.cards) {
            c.canvas.set_frame(r.offset(-row.x, -row.y));
        }
        let fade = 22.0 / row.w.max(1.0);
        let l = &pv.layout;
        let left = if l.scroll > 0.5 { fade } else { 0.0 };
        let right = if l.scroll < l.max_scroll - 0.5 { fade } else { 0.0 };
        transaction(None, || gfx::set_gradient_h(&pv.fade, left, right));
    }

    /// Slide the page rows to the animated page position.
    fn place_pages(hp: &mut HomePanel) {
        let pos = hp.pos.value;
        let w = hp.row.w;
        let h = hp.row.h;
        let mut alphas = vec![];
        transaction(None, || {
            for (k, pv) in hp.pages.iter().enumerate() {
                let (x, a) = wl::page_offset(k, pos, w);
                gfx::set_frame(&pv.row, Rect::new(x, 2.0, w, h));
                gfx::set_opacity(&pv.row, a);
                gfx::set_hidden(&pv.row, a <= 0.001);
                alphas.push(a);
            }
        });
        for (pv, a) in hp.pages.iter_mut().zip(alphas) {
            pv.alpha = a;
        }
    }

    /// Tab (or dot) frames for the pages, right of the notch.
    fn layout_tabs(&self, p: &Panel, hp: &mut HomePanel) {
        hp.tab_frames.clear();
        hp.dots = false;
        if hp.pages.len() < 2 {
            return;
        }
        let Some(home) = p.home_rect else { return };
        let hh = geo::header_height(&p.collapsed, &self.metrics);
        let x0 = p.collapsed.right() + 14.0;
        let area = Rect::new(x0, home.y, (home.right() - self.metrics.side_pad - x0).max(0.0), hh);
        let names = self.cfg.page_list();
        let widths: Vec<f64> = hp.pages.iter().map(|pv| gfx::text_width(&names[pv.page].name, 11.0, gfx::W_SEMIBOLD)).collect();
        match wl::page_tabs(area, &widths) {
            Some(t) => hp.tab_frames = t,
            None => {
                hp.dots = true;
                hp.tab_frames = wl::page_dots(area, hp.pages.len());
            }
        }
        let bounds = hp.tab_frames.iter().skip(1).fold(hp.tab_frames[0], |a, b| a.union(b));
        hp.tabs.set_frame(Self::local(p, bounds.inflate(2.0, 2.0)));
    }

    /// Draw the tabs / dots with the knob at the animated page position.
    fn draw_tabs(&mut self, i: usize) {
        let names = self.cfg.page_list();
        let p = &mut self.panels[i];
        let Some(hp) = p.home.as_mut() else { return };
        if hp.tab_frames.is_empty() {
            return;
        }
        let bounds = hp.tab_frames.iter().skip(1).fold(hp.tab_frames[0], |a, b| a.union(b)).inflate(2.0, 2.0);
        let o = Point::new(bounds.x, bounds.y);
        let pos = hp.pos.value.clamp(0.0, (hp.pages.len() - 1) as f64);
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(hp.pages.len() - 1);
        let t = pos - lo as f64;
        let lerp = |a: Rect, b: Rect| Rect::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.w + (b.w - a.w) * t, a.h + (b.h - a.h) * t);
        let hover = hp.hover.is_none();
        let _ = hover;
        let mut prims = vec![];
        let w = crate::widgets::Rgba::white;
        if hp.dots {
            for (k, f) in hp.tab_frames.iter().enumerate() {
                let near = (1.0 - (k as f64 - pos).abs()).clamp(0.0, 1.0);
                let d = 6.0 + 8.0 * near;
                let r = Rect::new(f.center_x() - d / 2.0, f.center_y() - 3.0, d, 6.0).offset(-o.x, -o.y);
                prims.push(draw::Prim::Rect { r, radius: 3.0, fill: w(0.3 + 0.62 * near), stroke: None });
            }
        } else {
            let bg = bounds.offset(-o.x, -o.y).inflate(-2.0, -2.0);
            prims.push(draw::Prim::Rect { r: bg, radius: bg.h / 2.0, fill: w(0.09), stroke: None });
            let knob = lerp(hp.tab_frames[lo], hp.tab_frames[hi]).offset(-o.x, -o.y).inflate(-2.0, -2.0);
            prims.push(draw::Prim::Rect { r: knob, radius: knob.h / 2.0, fill: w(0.2), stroke: None });
            for (k, f) in hp.tab_frames.iter().enumerate() {
                let near = (1.0 - (k as f64 - pos).abs()).clamp(0.0, 1.0);
                let name = names.get(hp.pages[k].page).map(|p| p.name.clone()).unwrap_or_default();
                prims.push(draw::Prim::Text { r: f.offset(-o.x, -o.y), text: name, font: draw::font(11.0, draw::Weight::Semibold), color: w(0.5 + 0.45 * near), align: draw::Align::Center, wrap: false });
            }
        }
        let mut env = Env { scale: p.scale, symbols: &mut self.symbols, fonts: &mut self.w.fonts, artwork: None, artwork_key: 0, camera: None, images: None, images_gen: 0 };
        hp.tabs.paint(&prims, &mut env);
    }

    // ----- data -------------------------------------------------------------

    /// Pull provider data, decode artwork, read the host's toggles.
    pub(super) fn sync_widgets(&mut self) {
        let g = self.shared.data_gen.load(Ordering::Relaxed);
        if g != self.w.data_gen {
            self.w.data_gen = g;
            let battery_before = self.w.data.battery.is_some();
            self.w.data = self.shared.data().clone();
            self.w.data.camera_access = self.w.camera_access;
            let key = self.w.data.now_playing.as_ref().map(|n| n.artwork_key()).unwrap_or(0);
            if self.w.artwork.as_ref().map(|(k, _)| *k) != Some(key) {
                let img = self.w.data.now_playing.as_ref().and_then(|n| n.artwork.as_ref()).and_then(|b| paint::decode_image(b));
                self.w.artwork = img.map(|i| (key, i));
            }
            if !battery_before && self.w.data.battery.is_some() && !self.w.battery_present {
                self.w.battery_present = true;
                self.rebuild_requested = true;
            }
        }
        let t = mono();
        if t - self.w.offset_at > 60.0 {
            self.w.offset_at = t;
            self.w.utc_offset = rt::utc_offset();
            self.w.hour24 = rt::uses_24h();
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
        // Timers.
        let mut finished = false;
        for m in &mut self.w.models {
            if m.inst.kind == WidgetKind::Timer && m.ui.timer.tick(t) == Some(TimerEvent::Finished) {
                finished = true;
            }
            m.ui.done.retain(|(_, at)| t - at < 30.0);
        }
        if finished {
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
        let mut v: Vec<Control> = host.toggles.iter().map(|t| Control { id: t.id.clone(), title: t.title.clone(), symbol: t.sf_symbol.clone(), on: Some((t.get)()) }).collect();
        v.extend(host.actions.iter().map(|a| Control { id: a.id.clone(), title: a.title.clone(), symbol: a.sf_symbol.clone(), on: None }));
        self.w.controls = v;
    }

    fn save_note(&mut self) {
        self.w.note_dirty = None;
        let Some((i, mi)) = self.w.editing else { return };
        if let Some(nv) = self.panels.get(i).and_then(|p| p.note_view.as_ref()) {
            let s: Retained<AnyObject> = unsafe { msg_send![&*nv.text, string] };
            if let Some(m) = self.w.models.get_mut(mi) {
                m.ui.note = rt::string_of(Some(&s)).chars().take(NOTE_MAX).collect();
            }
        }
        if let Some(m) = self.w.models.get(mi) {
            if let Some(path) = &m.note_path {
                if let Err(e) = std::fs::write(path, &m.ui.note) {
                    log::warn!("notch-shelf: saving note: {e}");
                }
            }
        }
    }

    /// Decode / look up images cards asked for.
    fn load_images(&mut self, reqs: &[(ImageKey, ImageSource)], scale: f64) {
        for (k, src) in reqs {
            if self.w.images.contains_key(k) {
                continue;
            }
            let img = match src {
                ImageSource::Data { bytes, template } => paint::nsimage_from_data(bytes).and_then(|ns| {
                    let size: NSSize = unsafe { msg_send![&*ns, size] };
                    let (w, h) = (size.width.max(1.0), size.height.max(1.0));
                    let f = (32.0 / w.max(h)).min(2.0);
                    gfx::rasterize(&ns, w * f, h * f, scale, template.then_some((1.0, 1.0, 1.0, 0.92)))
                }),
                ImageSource::Icon(s) => {
                    let path = match s.strip_prefix("app:") {
                        Some(app) => rt::resolve_app(app),
                        None => Some(s.clone()),
                    };
                    path.and_then(|p| {
                        // Through symlinks (Safari in /Applications is one),
                        // so the icon has no alias badge.
                        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p));
                        let icon = pboard::file_icon(&real);
                        gfx::rasterize(&icon, 64.0, 64.0, scale, None)
                    })
                }
            };
            self.w.images.insert(*k, img);
            self.w.images_gen += 1;
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
                p.switch_grace = Some(p.shape_now.rect);
                p.from = p.shape_now;
                p.to = geo::expanded_shape(&p.expanded, &m);
                let k = 360.0;
                p.spring = Spring::new(k, Spring::damping_for_ratio(k, 0.8), 0.0);
                p.spring.target = 1.0;
            }
        }
    }

    /// Show Home page `page` (index into the config's pages) on every
    /// panel; the pages slide.
    pub(super) fn set_page(&mut self, page: usize) {
        let mut found = false;
        for p in &mut self.panels {
            if let Some(hp) = p.home.as_mut() {
                if let Some(k) = hp.pages.iter().position(|pv| pv.page == page) {
                    hp.cur = k;
                    hp.pos.target = k as f64;
                    hp.hover = None;
                    found = true;
                }
            }
        }
        if (found || self.panels.is_empty()) && self.w.page != page {
            self.end_note_edit();
            self.w.page = page;
            self.w.save_state();
        }
        self.shared.presence.lock().unwrap_or_else(|e| e.into_inner()).page = self.w.page;
        self.shared.signal.notify();
    }

    fn step_page(&mut self, i: usize, dir: f64) {
        let Some(hp) = self.panels.get(i).and_then(|p| p.home.as_ref()) else { return };
        let k = wl::swipe_target(hp.cur, hp.pages.len(), dir);
        if k != hp.cur {
            let page = hp.pages[k].page;
            self.set_page(page);
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
            let cards: Vec<Card> = self.w.models.iter().map(|m| m.card()).collect();
            draw::live_activity(&ctx, &cards)
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
        let mut env = Env {
            scale: p.scale,
            symbols: &mut self.symbols,
            fonts: &mut self.w.fonts,
            artwork: self.w.artwork.as_ref().map(|(_, i)| i),
            artwork_key: self.w.artwork.as_ref().map(|(k, _)| *k).unwrap_or(0),
            camera: None,
            images: None,
            images_gen: 0,
        };
        lv.canvas.paint(&prims, &mut env);
    }

    pub(super) fn accent_rgba(&self) -> crate::widgets::Rgba {
        crate::widgets::Rgba(self.accent.0, self.accent.1, self.accent.2, 1.0)
    }

    fn ctx<'a>(&'a self, mono_now: f64, unix_now: f64, hover: Option<&'a Act>) -> Ctx<'a> {
        Ctx {
            data: &self.w.data,
            mono_now,
            unix_now,
            utc_offset: self.w.utc_offset,
            accent: self.accent_rgba(),
            controls: &self.w.controls,
            hover,
            camera_on: self.w.camera.running(),
            hour24: self.w.hour24,
        }
    }

    /// The card under `pt` on the current page of panel `i`: (card index,
    /// card frame).
    fn card_at(&self, i: usize, pt: Point) -> Option<(usize, Rect)> {
        let hp = self.panels.get(i)?.home.as_ref()?;
        if !hp.row.contains(pt) || (hp.pos.value - hp.cur as f64).abs() > 0.3 {
            return None;
        }
        let pv = hp.pages.get(hp.cur)?;
        pv.layout.cards.iter().enumerate().find(|(_, r)| r.contains(pt)).map(|(k, r)| (k, *r))
    }

    /// Hover, page slide, crossfade and card drawing of the Home view.
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
            if let Some(hp) = p.home.as_mut() {
                let before = hp.pos.value;
                hp.pos.step(dt);
                if hp.pos.settled() {
                    hp.pos.value = hp.pos.target;
                    hp.pos.velocity = 0.0;
                }
                if hp.pos.value != before || hp.pages.iter().enumerate().any(|(k, pv)| (pv.alpha > 0.001) != ((k as f64 - hp.pos.value).abs() < 1.0)) {
                    Self::place_pages(hp);
                }
            }
        }
        self.update_switch(i);
        self.draw_tabs(i);
        let p = &self.panels[i];
        let Some(_) = &p.home else { return };
        if p.mode != Mode::Expanded && p.content_alpha <= 0.0 {
            return;
        }
        // Hover (current page only).
        let mut hover = None;
        if p.mode == Mode::Expanded && self.w.view == View::Home && self.press.is_none() {
            if let Some((k, r)) = self.card_at(i, cur) {
                let hp = p.home.as_ref().unwrap();
                if let Some(a) = hp.pages[hp.cur].cards[k].draw.hit_at(Point::new(cur.x - r.x, cur.y - r.y)) {
                    hover = Some((k, a.clone()));
                }
            }
        }
        if let Some(fh) = self.w.forced_hover.clone() {
            hover = Some((0, fh));
        }
        if self.w.slider.is_some() {
            hover = None;
        }
        let (mono_now, unix_now) = self.ctx_parts();
        let hp = p.home.as_ref().unwrap();
        let cur_page = hp.cur;
        let mut draws: Vec<(usize, usize, CardDraw)> = vec![];
        {
            let base = self.ctx(mono_now, unix_now, None);
            for (k, pv) in hp.pages.iter().enumerate() {
                if pv.alpha <= 0.001 {
                    continue;
                }
                for (ci, (cv, r)) in pv.cards.iter().zip(&pv.layout.cards).enumerate() {
                    let mut ctx = base.clone();
                    let h = hover.as_ref().filter(|(hk, _)| k == cur_page && *hk == ci).map(|(_, a)| a);
                    ctx.hover = h;
                    let m = &self.w.models[cv.model];
                    draws.push((k, ci, draw::card(&m.card(), r.w, r.h, &ctx)));
                }
            }
        }
        let scale = p.scale;
        let reqs: Vec<(ImageKey, ImageSource)> = draws.iter().flat_map(|(_, _, d)| d.images.iter().cloned()).collect();
        if !reqs.is_empty() {
            self.load_images(&reqs, scale);
        }
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
            images: Some(&self.w.images),
            images_gen: self.w.images_gen,
        };
        for (k, ci, d) in draws {
            let cv = &mut hp.pages[k].cards[ci];
            cv.canvas.paint(&d.prims, &mut env);
            cv.draw = d;
        }
        // Keep the note editor over its card.
        if self.w.editing.is_some_and(|(pi, _)| pi == i) {
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
        if let Some(k) = hp.tab_frames.iter().position(|f| f.inflate(2.0, 4.0).contains(pt)) {
            let page = hp.pages[k].page;
            self.set_page(page);
            return true;
        }
        let Some((k, r)) = self.card_at(i, pt) else {
            if self.w.editing.is_some() {
                self.end_note_edit();
            }
            return true;
        };
        let cv = &hp.pages[hp.cur].cards[k];
        let model = cv.model;
        let local = Point::new(pt.x - r.x, pt.y - r.y);
        match cv.draw.hit_at(local).cloned() {
            Some(a) => self.perform(i, model, a, local, r),
            None => {
                if self.w.editing.is_some() {
                    self.end_note_edit();
                }
            }
        }
        true
    }

    /// Mouse dragged in the Home view (the volume slider).
    pub(super) fn on_home_dragged(&mut self, pt: Point) -> bool {
        let Some(s) = self.w.slider else { return false };
        let v = crate::widgets::sound::slider_value(pt.x, s.x0, s.x1);
        self.set_volume(s.model, v, false);
        true
    }

    pub(super) fn on_home_mouse_up(&mut self, pt: Point) -> bool {
        let Some(s) = self.w.slider.take() else { return false };
        let v = crate::widgets::sound::slider_value(pt.x, s.x0, s.x1);
        self.set_volume(s.model, v, true);
        true
    }

    fn set_volume(&mut self, _model: usize, v: f32, final_value: bool) {
        self.w.data.sound.volume = Some(v);
        self.shared.data().sound.volume = Some(v);
        let t = mono();
        let due = self.w.slider.map(|s| t - s.last_sent > 0.05).unwrap_or(true);
        if final_value || due {
            if let Some(s) = self.w.slider.as_mut() {
                s.last_sent = t;
            }
            if !self.w.demo {
                let mut r = self.shared.reqs();
                r.sound.retain(|c| !matches!(c, SoundCmd::Volume(_)));
                r.sound.push(SoundCmd::Volume(v));
                drop(r);
                self.shared.signal.notify();
            }
        }
    }

    pub(super) fn on_home_scroll(&mut self, i: usize, dx: f64, dy: f64, precise: bool, began: bool, momentum: bool) -> bool {
        if self.w.view != View::Home || self.panels.get(i).and_then(|p| p.home.as_ref()).is_none() {
            return false;
        }
        let t = mono();
        let cur = {
            let loc = objc2_app_kit::NSEvent::mouseLocation();
            Point::new(loc.x, self.primary_h - loc.y)
        };
        // A list card under the pointer scrolls vertically.
        if dy.abs() >= dx.abs() {
            if let Some((k, r)) = self.card_at(i, cur) {
                let hp = self.panels[i].home.as_ref().unwrap();
                let cv = &hp.pages[hp.cur].cards[k];
                if let Some((area, max)) = cv.draw.scroll.filter(|(_, max)| *max > 0.0) {
                    if area.offset(r.x, r.y).contains(cur) {
                        let model = cv.model;
                        let step = if precise { dy } else { dy * crate::widgets::cards::ROW_H };
                        let m = &mut self.w.models[model];
                        m.ui.scroll = (m.ui.scroll - step).clamp(0.0, max);
                        return true;
                    }
                }
            }
        }
        let horizontal = dx.abs() > dy.abs();
        let mut d = if horizontal { dx } else { dy };
        if !precise {
            d *= 14.0;
        }
        let m = self.metrics;
        // Scroll an overflowing row first.
        {
            let p = &mut self.panels[i];
            let hp = p.home.as_mut().unwrap();
            let k = hp.cur;
            let pv = &hp.pages[k];
            let max = pv.layout.max_scroll;
            let s = (pv.layout.scroll - d).clamp(0.0, max);
            if max > 0.0 && (s - pv.layout.scroll).abs() > 0.01 {
                let specs = pv.specs.clone();
                hp.pages[k].layout = wl::layout_cards(hp.row, &specs, s);
                Self::place_cards(hp, k);
                self.w.gesture = Gesture { acc: 0.0, switched: false, last: t };
                let _ = m;
                return true;
            }
        }
        // At the edge (or no overflow): swipe between pages.
        if momentum {
            return true;
        }
        let g = &mut self.w.gesture;
        if began || t - g.last > 0.3 {
            *g = Gesture::default();
        }
        g.last = t;
        if !precise {
            // A mouse wheel: one page per notch, with a cooldown.
            if !g.switched && d.abs() > 0.0 {
                g.switched = true;
                self.step_page(i, if d < 0.0 { 1.0 } else { -1.0 });
            }
            return true;
        }
        if !horizontal {
            return true;
        }
        g.acc += d;
        if !g.switched && g.acc.abs() > 36.0 {
            g.switched = true;
            let dir = if g.acc < 0.0 { 1.0 } else { -1.0 };
            self.step_page(i, dir);
        }
        true
    }

    fn collapse_all(&mut self) {
        for p in &mut self.panels {
            p.hold = false;
            p.logic.collapse(true);
        }
    }

    fn refresh_instance(&mut self, id: &str) {
        self.shared.reqs().refresh.push(id.to_string());
        self.shared.signal.notify();
    }

    /// Run an item / card action of a custom widget. `trusted`: the action
    /// came from the user's own command or plugin (commands allowed);
    /// otherwise only http(s) links open.
    fn run_action(&mut self, model: usize, a: ItemAction, trusted: bool) {
        let id = self.w.models[model].inst.id.clone();
        match a {
            ItemAction::OpenUrl(u) => {
                let l = u.to_ascii_lowercase();
                if trusted || l.starts_with("https://") || l.starts_with("http://") {
                    rt::open_url(&u);
                    self.collapse_all();
                }
            }
            ItemAction::Copy(text) => {
                pboard::copy_to_clipboard(&[crate::store::ShelfItem { id: 0, kind: crate::store::ItemKind::Text { text }, added_at: 0 }]);
                rt::play_sound("Pop");
            }
            ItemAction::Refresh => self.refresh_instance(&id),
            ItemAction::Run { program, args, shell, terminal, refresh } => {
                if !trusted {
                    log::warn!("notch-shelf: ignored a command from untrusted content");
                    return;
                }
                let argv: Vec<String> = if shell || args.is_empty() && program.contains(' ') && !std::path::Path::new(&program).exists() {
                    vec!["/bin/zsh".into(), "-lc".into(), if args.is_empty() { program } else { format!("{program} {}", args.iter().map(|a| crate::widgets::run::shell_quote(a)).collect::<Vec<_>>().join(" ")) }]
                } else {
                    std::iter::once(program).chain(args).collect()
                };
                if terminal {
                    match crate::widgets::run::terminal_script(&self.w.data_dir.join("notch-run"), &argv) {
                        Ok(p) => rt::open_path(&p.to_string_lossy()),
                        Err(e) => log::warn!("notch-shelf: terminal script: {e}"),
                    }
                } else if let Err(e) = crate::widgets::run::spawn_detached(&argv, std::env::var_os("HOME").map(PathBuf::from).as_deref()) {
                    log::warn!("notch-shelf: {e}");
                }
                if refresh {
                    // Give the command a moment, then re-run the widget.
                    let s = self.shared.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        s.reqs().refresh.push(id);
                        s.signal.notify();
                    });
                }
            }
        }
    }

    /// Text of a card's content for "Copy".
    fn content_text(&self, model: usize) -> String {
        let id = &self.w.models[model].inst.id;
        match self.w.data.content(id).and_then(|s| s.content()) {
            Some(c) => match &c.value {
                Some(v) => format!("{v}{}", if c.unit.is_empty() { String::new() } else { format!(" {}", c.unit) }).trim().to_string(),
                None if !c.body.is_empty() => format!("{}\n{}", c.title, c.body).trim().to_string(),
                None => c.title.clone(),
            },
            None => String::new(),
        }
    }

    pub(super) fn perform(&mut self, i: usize, model: usize, act: Act, local: Point, card: Rect) {
        let t = mono();
        if !matches!(act, Act::EditNote) {
            self.end_note_edit();
        }
        let kind = self.w.models[model].inst.kind;
        let id = self.w.models[model].inst.id.clone();
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
            Act::OpenEvent(eid) => {
                let ev = self.w.data.events.iter().find(|e| e.id == eid).cloned();
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
            Act::TimerPreset(secs) => self.w.models[model].ui.timer.start(secs as f64, t),
            Act::TimerStopwatch => self.w.models[model].ui.timer.start_stopwatch(t),
            Act::TimerPause => self.w.models[model].ui.timer.toggle_pause(t),
            Act::TimerReset => self.w.models[model].ui.timer.reset(),
            Act::TimerAdd(secs) => self.w.models[model].ui.timer.add(secs as f64, t),
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
            Act::EditNote => self.begin_note_edit(i, model),
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
            Act::Refresh => self.refresh_instance(&id),
            Act::CardAction => {
                // The content's own action wins; then the settings' click.
                let content_action = self.w.data.content(&id).and_then(|s| s.content()).and_then(|c| c.action.clone());
                let trusted = kind != WidgetKind::Web;
                if let Some(a) = content_action {
                    return self.run_action(model, a, trusted);
                }
                if let KindSettings::Script(s) = &self.w.models[model].settings {
                    {
                        let target = s.click_target.trim().to_string();
                        match s.click {
                            ClickAction::None => {}
                            ClickAction::Refresh => self.refresh_instance(&id),
                            ClickAction::OpenUrl => self.run_action(model, ItemAction::OpenUrl(target), true),
                            ClickAction::Run => {
                                let argv = crate::widgets::run::argv_for(s.interpreter, &s.interpreter_path, &target);
                                let (program, args) = (argv[0].clone(), argv[1..].to_vec());
                                self.run_action(model, ItemAction::Run { program, args, shell: false, terminal: false, refresh: true }, true)
                            }
                            ClickAction::Copy => {
                                let txt = self.content_text(model);
                                self.run_action(model, ItemAction::Copy(txt), true)
                            }
                        }
                    }
                }
            }
            Act::Item(k) => {
                let a = self.w.data.content(&id).and_then(|s| s.content()).and_then(|c| crate::widgets::cards::item_action(c, k).cloned());
                let refresh = self.w.data.content(&id).and_then(|s| s.content()).and_then(|c| c.items.get(k)).is_some_and(|it| it.refresh);
                if let Some(a) = a {
                    self.run_action(model, a, kind != WidgetKind::Web);
                }
                if refresh {
                    self.refresh_instance(&id);
                }
            }
            Act::Shortcut(k) => {
                let name = match &self.w.models[model].settings {
                    KindSettings::Shortcuts(s) => s.shortcuts.get(k).cloned(),
                    _ => None,
                };
                self.w.models[model].ui.pressed = Some((k, t));
                if let Some(name) = name {
                    if self.w.demo {
                        let st = crate::widgets::ShortcutsState { running: None, last: Some((name.clone(), Ok(format!("Ran \u{201c}{name}\u{201d}")), crate::widgets::unix_now())) };
                        self.shared.data().instances.insert(id.clone(), InstanceData::Shortcuts(st));
                        self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.shared.reqs().shortcuts.push((id.clone(), name));
                        self.shared.signal.notify();
                    }
                }
            }
            Act::Launch(k) => {
                let item = match &self.w.models[model].settings {
                    KindSettings::Launcher(s) => s.items.get(k).cloned(),
                    _ => None,
                };
                self.w.models[model].ui.pressed = Some((k, t));
                if let Some(it) = item {
                    self.launch(&it);
                }
            }
            Act::SoundDevice(k) => {
                if let Some(dev) = self.w.data.sound.devices.get(k).cloned() {
                    self.w.data.sound.current = Some(dev.id.clone());
                    self.shared.data().sound.current = Some(dev.id.clone());
                    if !self.w.demo {
                        self.shared.reqs().sound.push(SoundCmd::Device(dev.id));
                        self.shared.signal.notify();
                    }
                }
            }
            Act::SoundSlider(x0, x1) => {
                let (gx0, gx1) = (card.x + x0, card.x + x1);
                self.w.slider = Some(SliderDrag { model, x0: gx0, x1: gx1, last_sent: 0.0 });
                let v = crate::widgets::sound::slider_value(card.x + local.x, gx0, gx1);
                self.set_volume(model, v, false);
            }
            Act::SoundMute => {
                let m = !self.w.data.sound.muted;
                self.w.data.sound.muted = m;
                self.shared.data().sound.muted = m;
                if !self.w.demo {
                    self.shared.reqs().sound.push(SoundCmd::Mute(m));
                    self.shared.signal.notify();
                }
            }
            Act::ReminderDone(rid) => {
                let ui = &mut self.w.models[model].ui;
                if !ui.done.iter().any(|(x, _)| *x == rid) {
                    ui.done.push((rid.clone(), t));
                    rt::play_sound("Tink");
                    if !self.w.demo {
                        self.shared.reqs().reminders_complete.push(rid);
                        self.shared.signal.notify();
                    }
                }
            }
            Act::OpenReminders => rt::open_path("/System/Applications/Reminders.app"),
            Act::RemindersAllow => {
                self.shared.reqs().reminders_ask = true;
                self.shared.signal.notify();
            }
            Act::OpenUrl(u) => rt::open_url(&u),
        }
    }

    fn launch(&mut self, it: &crate::widgets::catalog::LaunchItem) {
        use crate::widgets::catalog::LaunchKind;
        let target = it.target.trim().to_string();
        match it.kind {
            LaunchKind::App => match rt::resolve_app(&target) {
                Some(p) => rt::open_path(&p),
                None => log::info!("notch-shelf: app not found: {target}"),
            },
            LaunchKind::File | LaunchKind::Folder => rt::open_path(&crate::widgets::xbar::expand_tilde(&target).to_string_lossy()),
            LaunchKind::Url => rt::open_url(&target),
            LaunchKind::Command => {
                let argv = vec!["/bin/zsh".to_string(), "-lc".to_string(), target];
                if let Err(e) = crate::widgets::run::spawn_detached(&argv, std::env::var_os("HOME").map(PathBuf::from).as_deref()) {
                    log::warn!("notch-shelf: {e}");
                }
                return;
            }
        }
        self.collapse_all();
    }

    /// The panel collapsed / went away: stop the camera, end editing.
    pub(super) fn home_closed(&mut self) {
        if self.panels.iter().all(|p| p.mode != Mode::Expanded) {
            self.w.camera.stop();
            self.end_note_edit();
            self.w.slider = None;
        }
    }

    // ----- note editor ----------------------------------------------------------

    fn note_rect(&self, i: usize, model: usize) -> Option<Rect> {
        let p = self.panels.get(i)?;
        let hp = p.home.as_ref()?;
        let pv = hp.pages.get(hp.cur)?;
        let k = pv.cards.iter().position(|c| c.model == model)?;
        let r = pv.layout.cards[k];
        let tr = draw::note_text_rect(r.w, r.h).offset(r.x, r.y);
        Some(Self::local(p, tr))
    }

    fn place_note_view(&mut self, i: usize) {
        let Some((_, model)) = self.w.editing else { return };
        let Some(r) = self.note_rect(i, model) else {
            self.end_note_edit();
            return;
        };
        let p = &self.panels[i];
        if let Some(nv) = &p.note_view {
            let f = NSRect::new(NSPoint::new(r.x, p.win.h - (r.y + r.h)), NSSize::new(r.w, r.h));
            unsafe {
                let _: () = msg_send![&*nv.scroll, setFrame: f];
            }
        }
    }

    fn begin_note_edit(&mut self, i: usize, model: usize) {
        if self.w.editing.is_some() || self.note_rect(i, model).is_none() {
            return;
        }
        let accent = self.accent;
        let p = &mut self.panels[i];
        if p.note_view.is_none() {
            p.note_view = Some(unsafe { make_note_view(&p.content_view, &p.view, accent) });
        }
        let nv = p.note_view.as_ref().unwrap();
        unsafe {
            let _: () = msg_send![&*nv.text, setString: &*ns(&self.w.models[model].ui.note)];
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
        self.w.editing = Some((i, model));
        self.w.models[model].ui.editing = true;
        self.place_note_view(i);
        log::info!("notch-shelf: editing note (panel key: {})", self.panels[i].window.isKeyWindow());
    }

    pub fn end_note_edit(&mut self) {
        let Some((i, model)) = self.w.editing else { return };
        self.save_note();
        self.w.editing = None;
        if let Some(m) = self.w.models.get_mut(model) {
            m.ui.editing = false;
        }
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
        if let Some((i, _)) = self.w.editing {
            if self.panels.get(i).is_none_or(|p| !p.window.isKeyWindow()) {
                self.end_note_edit();
            }
        }
    }

    // ----- diagnostics ------------------------------------------------------------

    fn first_model(&self, kind: WidgetKind) -> Option<usize> {
        self.w.models.iter().position(|m| m.inst.kind == kind)
    }

    /// Widget debug commands (see `Handle::debug`); true if handled.
    pub(super) fn debug_widgets(&mut self, cmd: &str) -> bool {
        let t = mono();
        let arg = |c: &str| c.split_once(':').map(|(_, a)| a.to_string()).unwrap_or_default();
        match cmd {
            "demo" => {
                self.w.demo = true;
                self.shared.demo.store(true, Ordering::Relaxed);
                let art = demo_artwork();
                let mut d = crate::widgets::demo::data(crate::widgets::unix_now(), self.w.utc_offset, art);
                // Keep what the custom widgets (scripts, plugins) produced.
                d.instances = self.shared.data().instances.clone();
                *self.shared.data() = d;
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
                self.sync_widgets();
                self.rebuild_requested = true;
            }
            "timerDone" => {
                if let Some(m) = self.first_model(WidgetKind::Timer) {
                    self.w.models[m].ui.timer.start(1.0, t - 2.0);
                }
                self.sync_widgets();
            }
            "timerReset" => {
                for m in &mut self.w.models {
                    m.ui.timer.reset();
                }
            }
            "pause" => {
                if let Some(n) = self.shared.data().now_playing.as_mut() {
                    n.playing = false;
                }
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
            }
            "editNote" => {
                if let Some(m) = self.first_model(WidgetKind::Note) {
                    if let Some(i) = (0..self.panels.len()).find(|i| self.note_rect(*i, m).is_some()) {
                        self.begin_note_edit(i, m);
                    }
                }
            }
            "endEdit" => self.end_note_edit(),
            c if c.starts_with("timer:") => {
                // timer:TOTAL[:ELAPSED]
                let a = arg(c);
                let mut it = a.split(':').map(|v| v.parse::<f64>().unwrap_or(0.0));
                let total = it.next().unwrap_or(300.0);
                let el = it.next().unwrap_or(0.0);
                if let Some(m) = self.first_model(WidgetKind::Timer) {
                    self.w.models[m].ui.timer.start(total, t - el);
                }
            }
            c if c.starts_with("stopwatch:") => {
                let el: f64 = arg(c).parse().unwrap_or(0.0);
                if let Some(m) = self.first_model(WidgetKind::Timer) {
                    self.w.models[m].ui.timer.start_stopwatch(t - el);
                }
            }
            c if c.starts_with("hoverControl:") => self.w.forced_hover = arg(c).parse().ok().map(Act::Control),
            c if c.starts_with("hover:") => {
                // hover:refresh / hover:item:N / hover:launch:N
                let a = arg(c);
                self.w.forced_hover = match a.split(':').collect::<Vec<_>>().as_slice() {
                    ["refresh"] => Some(Act::Refresh),
                    ["item", n] => n.parse().ok().map(Act::Item),
                    ["launch", n] => n.parse().ok().map(Act::Launch),
                    ["shortcut", n] => n.parse().ok().map(Act::Shortcut),
                    _ => None,
                };
            }
            "noHover" => self.w.forced_hover = None,
            c if c.starts_with("note:") => {
                if let Some(m) = self.first_model(WidgetKind::Note) {
                    self.w.models[m].ui.note = arg(c).replace("\\n", "\n");
                    if let Some(p) = &self.w.models[m].note_path {
                        let _ = std::fs::write(p, &self.w.models[m].ui.note);
                    }
                }
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
            c if c.starts_with("reminders:") => {
                let a = match arg(c).as_str() {
                    "denied" => Access::Denied,
                    "ask" => Access::NotDetermined,
                    _ => Access::Granted,
                };
                self.shared.data().reminders.access = a;
                self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
            }
            c if c.starts_with("page:") => {
                if let Ok(n) = arg(c).parse() {
                    self.set_page(n);
                }
            }
            c if c.starts_with("scroll:") => {
                // scroll:N — list scroll of the first list-like card on the page.
                let v: f64 = arg(c).parse().unwrap_or(0.0);
                for m in &mut self.w.models {
                    m.ui.scroll = v;
                }
            }
            c if c.starts_with("tick:") => {
                // tick:N — tick the Nth shown reminder (demo).
                if let Some(m) = self.first_model(WidgetKind::Reminders) {
                    let n: usize = arg(c).parse().unwrap_or(0);
                    let KindSettings::Reminders(s) = &self.w.models[m].settings else { return true };
                    let ids: Vec<String> = crate::widgets::reminders::visible(&self.w.data.reminders.items, s, crate::widgets::unix_now(), self.w.utc_offset, &[]).iter().map(|r| r.id.clone()).collect();
                    if let Some(id) = ids.get(n) {
                        self.w.models[m].ui.done.push((id.clone(), t));
                    }
                }
            }
            c if c.starts_with("webBody:") => {
                // webBody:INSTANCE_ID|BODY — run a canned response through
                // the Web widget's templates (renders without network).
                let a = arg(c);
                if let Some((id, body)) = a.split_once('|') {
                    if let Some(m) = self.w.models.iter().find(|m| m.inst.id == id) {
                        if let KindSettings::Web(s) = &m.settings {
                            let r = crate::widgets::web::to_content(body, s, self.w.utc_offset);
                            let mut d = self.shared.data();
                            let prev = match d.instances.remove(id) {
                                Some(InstanceData::Content(s)) => s,
                                _ => Status::Loading,
                            };
                            d.instances.insert(id.to_string(), InstanceData::Content(prev.next(r, crate::widgets::unix_now())));
                            drop(d);
                            self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
            c if c.starts_with("error:") => {
                // error:INSTANCE_ID|MESSAGE — a failed refresh.
                let a = arg(c);
                if let Some((id, msg)) = a.split_once('|') {
                    let mut d = self.shared.data();
                    let prev = match d.instances.remove(id) {
                        Some(InstanceData::Content(s)) => s,
                        _ => Status::Loading,
                    };
                    d.instances.insert(id.to_string(), InstanceData::Content(prev.next(Err(msg.to_string()), crate::widgets::unix_now())));
                    drop(d);
                    self.shared.data_gen.fetch_add(1, Ordering::Relaxed);
                }
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
