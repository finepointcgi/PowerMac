//! Main-thread state: one panel per open shelf, the frame loop, pointer
//! tracking, drawing (Core Animation layers), events and actions.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSColor, NSEvent, NSScreen, NSWindowCollectionBehavior};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use pm_shelf_core::anim::{approach, ease_out_cubic, Spring};
use pm_shelf_core::geom::{Point, Rect};
use pm_shelf_core::mac::ffi::*;
use pm_shelf_core::mac::gfx::{self, Layer, Symbol, SymbolCache, TextStyle};
use pm_shelf_core::mac::pboard;
use pm_shelf_core::store::{ItemKind, NewItem, ShelfItem};

use super::view::{self, ShelfView, ShelfWindow};
use crate::layout::{self as lay, Hit, Metrics, Mode};
use crate::model;
use crate::session::DragWatch;
use crate::{fileops, Config, Request, Shared};

/// Above normal windows, floating palettes (3) and modal panels (8); below
/// the menu bar (24), so menus and the notch shelf stay on top.
pub const LEVEL: isize = 20;
const FRAME: f64 = 1.0 / 60.0;
const IDLE: f64 = 1.0 / 15.0;
const ASLEEP: f64 = 1.0;
/// How long a status message replaces the caption.
const FLASH_SECS: f64 = 2.6;

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
}

/// Work for the view to do once the app state is released.
pub enum Deferred {
    /// Move the shelf with the mouse (`performWindowDragWithEvent:`).
    MoveWindow,
    /// (pasteboard writer, frame in window coordinates, drag image)
    DragOut(Vec<(Retained<AnyObject>, Rect, Retained<AnyObject>)>),
    /// (title, tag (<0 = separator), SF symbol, enabled) at a window point.
    Menu { entries: Vec<(String, isize, &'static str, bool)>, at: Point },
    Share { items: Vec<Retained<AnyObject>>, anchor: Rect },
    ChooseFolder { mv: bool, count: usize },
}

const M_OPEN: isize = 1;
const M_REVEAL: isize = 2;
const M_COPY: isize = 3;
const M_SHARE: isize = 4;
const M_COPY_TO: isize = 5;
const M_MOVE_TO: isize = 6;
const M_REMOVE: isize = 7;
const M_CLEAR: isize = 8;
const M_CLOSE: isize = 9;
const M_GRID: isize = 10;

struct Press {
    shelf: u64,
    start: Point,
    hit: Hit,
    mods: Mods,
    dragged: bool,
}

#[derive(Default)]
struct Thumb {
    icon: Option<Image>,
    ql: Option<Image>,
    requested: bool,
}

struct Btn {
    bg: Layer,
    icon: Layer,
}

struct CardL {
    root: Layer,
    img: Layer,
    plate: Layer,
    glyph: Layer,
    text: Layer,
    key: String,
}

#[derive(Clone, Copy, PartialEq, Default)]
struct TileState {
    selected: bool,
    hover: bool,
    remove_hot: bool,
}

struct Tile {
    id: u64,
    root: Layer,
    plate: Layer,
    thumb: Layer,
    glyph: Layer,
    label: Layer,
    remove: Layer,
    remove_icon: Layer,
    state: Option<TileState>,
    key: String,
}

struct Panel {
    id: u64,
    window: Retained<ShelfWindow>,
    view: Retained<ShelfView>,
    scale: f64,
    /// Window frame (global, top-left origin).
    win: Rect,
    /// Panel rect now / animating from / to (global).
    rect: Rect,
    from: Rect,
    to: Rect,
    resize: Spring,
    mode: Mode,
    /// 0 = stack showing, 1 = grid.
    mix: f64,
    appear: Spring,
    appear_at: Point,
    closing: Option<f64>,
    root: Layer,
    group: Layer,
    shadow: Layer,
    content: Layer,
    gloss: Layer,
    stack: Layer,
    cards: Vec<CardL>,
    badge: Layer,
    badge_text: Layer,
    caption: Layer,
    subcaption: Layer,
    empty: Layer,
    empty_border: Layer,
    empty_icon: Layer,
    empty_text: Layer,
    grid: Layer,
    grid_clip: Layer,
    tiles: Vec<Tile>,
    title: Layer,
    close: Btn,
    toggle: Btn,
    menu: Btn,
    ring: Layer,
    items: Vec<ShelfItem>,
    hover: Hit,
    drop_hover: bool,
    scroll: f64,
    selected: BTreeSet<u64>,
    sel_anchor: Option<u64>,
    emptied_at: Option<f64>,
    flash: Option<(String, f64)>,
    /// Content (cards, tiles, captions) needs refilling.
    dirty: bool,
    /// Visual state last drawn (header, ring, empty zone).
    state_key: String,
    /// The window was moved by the user and the new place isn't saved yet.
    move_pending: bool,
}

pub struct App {
    shared: Arc<Shared>,
    cfg: Config,
    cfg_gen: u64,
    gen: u64,
    m: Metrics,
    panels: Vec<Panel>,
    timer: CFRunLoopTimerRef,
    last_tick: f64,
    primary_h: f64,
    /// (frame, visible frame) of each screen, global top-left.
    screens: Vec<(Rect, Rect, f64)>,
    last_screen_poll: f64,
    drag_pb: Retained<AnyObject>,
    watch: DragWatch,
    thumbs: HashMap<u64, Thumb>,
    symbols: SymbolCache,
    accent: (f64, f64, f64),
    press: Option<Press>,
    own_drag: Option<(u64, Vec<u64>)>,
    menu_open: bool,
    /// (shelf, item ids) the open menu acts on, and its anchor (window).
    menu_ctx: Option<(u64, Vec<u64>, Rect)>,
    folder_ctx: Option<(u64, Vec<u64>, bool)>,
    sharing: bool,
    picker: Option<Retained<AnyObject>>,
    /// Diagnostics: skip the spring-in of the next shelves.
    no_appear: bool,
    debug_drop: Option<u64>,
    debug_hover: Option<(u64, Hit)>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` with the app state, unless it's missing or already borrowed.
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

pub fn tick() {
    autoreleasepool(|_| {
        with(|a| a.tick());
    });
}

extern "C" fn timer_cb(_t: CFRunLoopTimerRef, _info: *mut c_void) {
    tick();
}

pub fn install(shared: Arc<Shared>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    uninstall();
    let drag_pb = pboard::drag_pasteboard();
    let watch = DragWatch::new(pboard::change_count(&drag_pb));
    let (screens, primary_h) = read_screens(mtm);
    let mut app = App {
        shared,
        cfg: Config { enabled: false, ..Config::default() },
        cfg_gen: 0,
        gen: 0,
        m: Metrics::default(),
        panels: vec![],
        timer: std::ptr::null(),
        last_tick: now(),
        primary_h,
        screens,
        last_screen_poll: now(),
        drag_pb,
        watch,
        thumbs: HashMap::new(),
        symbols: SymbolCache::default(),
        accent: gfx::accent_rgb(),
        press: None,
        own_drag: None,
        menu_open: false,
        menu_ctx: None,
        folder_ctx: None,
        sharing: false,
        picker: None,
        no_appear: false,
        debug_drop: None,
        debug_hover: None,
    };
    unsafe {
        let t = CFRunLoopTimerCreate(std::ptr::null(), now() + 0.01, FRAME, 0, 0, timer_cb, std::ptr::null_mut());
        CFRunLoopAddTimer(CFRunLoopGetMain(), t, kCFRunLoopCommonModes);
        app.timer = t;
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
    tick();
}

pub fn uninstall() {
    let old = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
    if let Some(mut app) = old {
        if !app.timer.is_null() {
            unsafe {
                CFRunLoopTimerInvalidate(app.timer);
                CFRelease(app.timer);
            }
        }
        for p in app.panels.drain(..) {
            p.window.orderOut(None);
            p.window.close();
        }
    }
}

fn thumb_key(p: &FsPath) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

fn thumb_ready(key: u64, img: Image, icon_like: bool) {
    with(|a| {
        let img = if icon_like {
            img
        } else {
            let scale = a.panels.first().map(|p| p.scale).unwrap_or(2.0);
            gfx::rounded_fit(&img, a.m.card, 9.0, scale).unwrap_or(img)
        };
        a.thumbs.entry(key).or_default().ql = Some(img);
        for p in &mut a.panels {
            p.dirty = true;
        }
        a.schedule(true);
    });
}

/// Screens as (frame, visible frame, scale) in global top-left points, and
/// the primary screen's height.
fn read_screens(mtm: MainThreadMarker) -> (Vec<(Rect, Rect, f64)>, f64) {
    let screens = NSScreen::screens(mtm);
    let ph = screens.iter().next().map(|s| s.frame().size.height).unwrap_or(0.0);
    let tl = |f: NSRect| Rect::new(f.origin.x, ph - (f.origin.y + f.size.height), f.size.width, f.size.height);
    (screens.iter().map(|s| (tl(s.frame()), tl(s.visibleFrame()), s.backingScaleFactor())).collect(), ph)
}

/// Height of the grid group: the tallest grid (its children never move).
fn grid_max_h(m: &Metrics) -> f64 {
    lay::panel_size(Mode::Grid, m.max_cols * m.max_rows, m).1
}

fn white(a: f64) -> Color {
    Color::white(a)
}

fn rotate(angle: f64) -> gfx::AffineTransform {
    let (s, c) = angle.sin_cos();
    gfx::AffineTransform { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
}

impl App {
    fn accent(&self, a: f64) -> Color {
        let (r, g, b) = self.accent;
        Color::rgba(r, g, b, a)
    }

    fn sym(&mut self, name: &str, size: f64, weight: f64, rgba: (f64, f64, f64, f64), scale: f64) -> Option<Symbol> {
        self.symbols.get(name, size, weight, rgba, scale)
    }

    fn idx(&self, id: u64) -> Option<usize> {
        self.panels.iter().position(|p| p.id == id)
    }

    fn cocoa(&self, r: Rect) -> NSRect {
        NSRect::new(NSPoint::new(r.x, self.primary_h - (r.y + r.h)), NSSize::new(r.w, r.h))
    }

    /// Visible frame of the screen holding `p` (or the main one).
    fn visible_at(&self, p: Point) -> Rect {
        self.screens
            .iter()
            .find(|(f, _, _)| f.contains(p))
            .or(self.screens.first())
            .map(|(_, v, _)| *v)
            .unwrap_or(Rect::new(0.0, 0.0, 1440.0, 900.0))
    }

    fn scale_at(&self, p: Point) -> f64 {
        self.screens.iter().find(|(f, _, _)| f.contains(p)).or(self.screens.first()).map(|(_, _, s)| *s).unwrap_or(2.0)
    }

    fn clamp_rect(&self, r: Rect) -> Rect {
        let v = self.visible_at(Point::new(r.center_x(), r.y + 10.0));
        let o = model::placement(Point::new(r.center_x(), r.center_y()), (r.w, r.h), v, &[]);
        Rect::new(o.x, o.y, r.w, r.h)
    }

    fn schedule(&mut self, fast: bool) {
        let next = if fast {
            FRAME
        } else if self.panels.is_empty() {
            ASLEEP
        } else {
            IDLE
        };
        if !self.timer.is_null() {
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, now() + next) };
        }
    }

    // ----- frame loop -------------------------------------------------

    fn tick(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let t = now();
        let dt = (t - self.last_tick).clamp(0.0, 0.05);
        self.last_tick = t;
        if t - self.last_screen_poll > 1.5 {
            self.last_screen_poll = t;
            let (s, ph) = read_screens(mtm);
            self.screens = s;
            self.primary_h = ph;
        }
        let requests = self.sync(mtm);
        if self.shared.lock().stopped {
            return;
        }
        let loc = NSEvent::mouseLocation();
        let cur = Point::new(loc.x, self.primary_h - loc.y);
        let button = NSEvent::pressedMouseButtons() & 1 != 0;
        for r in requests {
            match r {
                Request::RenderPng(dir) => {
                    for i in 0..self.panels.len() {
                        self.refresh(i);
                    }
                    self.render_pngs(&dir);
                }
                Request::Debug(cmd) => self.debug(mtm, &cmd),
            }
        }
        let mut animating = false;
        let mut i = 0;
        while i < self.panels.len() {
            if self.step(i, dt, t, cur, button) {
                animating = true;
            }
            if self.panels[i].closing.is_some_and(|c| c >= 1.0) {
                let p = self.panels.remove(i);
                p.window.orderOut(None);
                p.window.close();
                continue;
            }
            i += 1;
        }
        self.schedule(animating);
    }

    /// Bring the panels in line with the shelves; returns the requests.
    fn sync(&mut self, mtm: MainThreadMarker) -> Vec<Request> {
        let (cfg, cfg_gen, snapshot, to_place, requests, stopped) = {
            let mut st = self.shared.lock();
            let cfg = (st.config_gen != self.cfg_gen).then(|| st.config.clone());
            let snap = (st.gen != self.gen).then(|| {
                let v: Vec<(u64, Vec<ShelfItem>, Point, bool)> = st.set.shelves().iter().map(|s| (s.id, s.items().to_vec(), s.origin, s.expanded)).collect();
                (st.gen, v)
            });
            (cfg, st.config_gen, snap, std::mem::take(&mut st.to_place), std::mem::take(&mut st.requests), st.stopped)
        };
        if stopped {
            for p in self.panels.drain(..) {
                p.window.orderOut(None);
                p.window.close();
            }
            return vec![];
        }
        if let Some(c) = cfg {
            self.cfg = c;
            self.cfg_gen = cfg_gen;
        }
        let Some((gen, shelves)) = snapshot else { return requests };
        self.gen = gen;
        // Closed shelves.
        let ids: Vec<u64> = shelves.iter().map(|s| s.0).collect();
        for i in 0..self.panels.len() {
            if !ids.contains(&self.panels[i].id) && self.panels[i].closing.is_none() {
                self.panels[i].closing = Some(0.0);
            }
        }
        for (id, items, origin, expanded) in shelves {
            match self.idx(id) {
                Some(i) => self.set_items(i, items, true),
                None => {
                    let place = to_place.iter().find(|(x, _)| *x == id).map(|(_, c)| *c);
                    let n = items.len();
                    let mode = if expanded && n > 0 { Mode::Grid } else { Mode::Stack };
                    let size = lay::panel_size(mode, n, &self.m);
                    let (origin, appear_at) = match place {
                        Some(Some(c)) => {
                            let taken: Vec<Point> = self.panels.iter().map(|p| Point::new(p.rect.x, p.rect.y)).collect();
                            (model::placement(c, size, self.visible_at(c), &taken), Some(c))
                        }
                        _ => {
                            let r = self.clamp_rect(Rect::new(origin.x, origin.y, size.0, size.1));
                            (Point::new(r.x, r.y), None)
                        }
                    };
                    self.shared.lock().set.set_origin(id, origin);
                    let p = self.make_panel(mtm, id, Rect::new(origin.x, origin.y, size.0, size.1), mode, appear_at);
                    self.panels.push(p);
                    let i = self.panels.len() - 1;
                    self.set_items(i, items, false);
                    self.refresh(i);
                }
            }
        }
        requests
    }

    fn set_items(&mut self, i: usize, items: Vec<ShelfItem>, animate: bool) {
        let p = &mut self.panels[i];
        if p.items == items {
            return;
        }
        let was = p.items.len();
        let grew = items.len() > was;
        p.items = items;
        p.dirty = true;
        let ids: BTreeSet<u64> = p.items.iter().map(|i| i.id).collect();
        p.selected.retain(|id| ids.contains(id));
        if p.items.is_empty() && was > 0 {
            p.emptied_at = Some(now());
        } else if !p.items.is_empty() {
            p.emptied_at = None;
        }
        if grew && p.mode == Mode::Grid {
            p.scroll = f64::MAX;
        }
        self.retarget(i, animate);
    }

    /// Size the panel for its mode and item count (keeping its top center).
    fn retarget(&mut self, i: usize, animate: bool) {
        let m = self.m;
        let p = &self.panels[i];
        let n = p.items.len();
        let mode = if n == 0 { Mode::Stack } else { p.mode };
        let (w, h) = lay::panel_size(mode, n, &m);
        if p.mode == mode && (p.to.w, p.to.h) == (w, h) {
            return;
        }
        let to = self.clamp_rect(Rect::new(p.rect.center_x() - w / 2.0, p.rect.y, w, h));
        let p = &mut self.panels[i];
        p.mode = mode;
        p.from = p.rect;
        p.to = to;
        if animate {
            let k = 360.0;
            p.resize = Spring::new(k, Spring::damping_for_ratio(k, 0.80), 0.0);
            p.resize.target = 1.0;
        } else {
            p.resize = Spring::new(1.0, 1.0, 1.0);
            p.rect = to;
            p.mix = if mode == Mode::Grid { 1.0 } else { 0.0 };
        }
        let win = p.from.union(&to).inflate(m.margin, m.margin);
        self.set_win(i, win);
        self.shared.lock().set.set_expanded(self.panels[i].id, mode == Mode::Grid);
    }

    fn set_win(&mut self, i: usize, win: Rect) {
        let frame = self.cocoa(win);
        let p = &mut self.panels[i];
        if p.win == win {
            return;
        }
        p.win = win;
        p.window.setFrame_display(frame, true);
        let bounds = Rect::new(0.0, 0.0, win.w, win.h);
        transaction(None, || {
            gfx::set_frame(&p.root, bounds);
            gfx::set_transform(&p.group, gfx::AffineTransform::scale(1.0));
            let a = Point::new(((p.appear_at.x - win.x) / win.w).clamp(0.0, 1.0), (1.0 - (p.appear_at.y - win.y) / win.h).clamp(0.0, 1.0));
            gfx::set_anchor(&p.group, a.x, a.y);
            gfx::set_frame(&p.group, bounds);
        });
        p.dirty = true;
    }

    /// Animate and draw one panel; true while it is moving.
    fn step(&mut self, i: usize, dt: f64, t: f64, cur: Point, button: bool) -> bool {
        // The user moved the window (by its background).
        {
            let f = self.panels[i].window.frame();
            let o = Point::new(f.origin.x, self.primary_h - (f.origin.y + f.size.height));
            let p = &mut self.panels[i];
            let (dx, dy) = (o.x - p.win.x, o.y - p.win.y);
            if (dx.abs() > 0.5 || dy.abs() > 0.5) && p.closing.is_none() {
                p.win = p.win.offset(dx, dy);
                p.rect = p.rect.offset(dx, dy);
                p.from = p.from.offset(dx, dy);
                p.to = p.to.offset(dx, dy);
                p.move_pending = true;
            }
            if p.move_pending && !button {
                p.move_pending = false;
                let (id, o) = (p.id, Point::new(p.to.x, p.to.y));
                self.shared.lock().set.set_origin(id, o);
            }
        }
        // Hover.
        if self.press.is_none() && !self.menu_open && self.own_drag.is_none() {
            let m = self.m;
            let p = &mut self.panels[i];
            let local = Point::new(cur.x - p.rect.x, cur.y - p.rect.y);
            let l = lay::layout(p.mode, p.items.len(), p.scroll, &m);
            let hover_tile = match p.hover {
                Hit::Tile(k) | Hit::Remove(k) => Some(k),
                _ => None,
            };
            let settled = p.resize.settled();
            let mut hit = if settled && p.closing.is_none() { lay::hit_test(&l, p.mode, local, hover_tile, &m) } else { Hit::Outside };
            if let Some((id, h)) = self.debug_hover {
                if id == p.id {
                    hit = h;
                }
            }
            if hit != p.hover {
                p.hover = hit;
                p.state_key.clear();
            }
        }
        // Auto-close.
        {
            let p = &self.panels[i];
            if let Some(at) = p.emptied_at {
                let delay = self.cfg.auto_close_delay().as_secs_f64();
                if self.cfg.auto_close && p.items.is_empty() && !p.drop_hover && p.closing.is_none() && t - at >= delay && self.press.as_ref().is_none_or(|pr| pr.shelf != p.id) {
                    let id = p.id;
                    log::info!("drag shelf: shelf {id} emptied, closing");
                    self.shared.edit(|s| s.close(id));
                    self.panels[i].closing = Some(0.0);
                }
            }
        }
        if self.panels[i].flash.as_ref().is_some_and(|(_, until)| t > *until) {
            self.panels[i].flash = None;
            self.panels[i].dirty = true;
        }
        if self.debug_drop == Some(self.panels[i].id) {
            self.panels[i].drop_hover = true;
        }
        let p = &mut self.panels[i];
        p.resize.step(dt);
        p.appear.step(dt);
        let rect = p.from.lerp(&p.to, p.resize.value);
        let moved = rect != p.rect;
        p.rect = rect;
        let mix_target = if p.mode == Mode::Grid { 1.0 } else { 0.0 };
        let old_mix = p.mix;
        p.mix = approach(p.mix, mix_target, dt, 0.07);
        if let Some(c) = &mut p.closing {
            *c = (*c + dt / 0.16).min(1.0);
        }
        let animating = !p.resize.settled() || !p.appear.settled() || p.mix != mix_target || p.closing.is_some();
        if p.resize.settled() && p.win != p.to.inflate(self.m.margin, self.m.margin) && p.closing.is_none() && !p.move_pending {
            let w = p.to.inflate(self.m.margin, self.m.margin);
            self.set_win(i, w);
        }
        let p = &self.panels[i];
        let wants_fill = p.dirty;
        if wants_fill {
            self.fill(i);
        }
        let p = &self.panels[i];
        if moved || old_mix != p.mix || wants_fill || animating {
            self.place(i);
        }
        self.draw_group(i);
        self.update_states(i);
        animating
    }

    /// Appear / close transform and opacity of the whole panel.
    fn draw_group(&self, i: usize) {
        let p = &self.panels[i];
        let (scale, alpha) = match p.closing {
            Some(c) => {
                let e = ease_out_cubic(c);
                (1.0 - 0.10 * e, 1.0 - e)
            }
            None => {
                let v = p.appear.value;
                (0.55 + 0.45 * v, (v * 1.8).clamp(0.0, 1.0))
            }
        };
        transaction(None, || {
            gfx::set_transform(&p.group, gfx::AffineTransform::scale(scale));
            gfx::set_opacity(&p.group, alpha);
        });
    }

    // ----- panels -----------------------------------------------------

    fn make_panel(&mut self, mtm: MainThreadMarker, id: u64, rect: Rect, mode: Mode, appear_at: Option<Point>) -> Panel {
        let m = self.m;
        let win = rect.inflate(m.margin, m.margin);
        let scale = self.scale_at(Point::new(rect.center_x(), rect.center_y()));
        let window = ShelfWindow::new(mtm, self.cocoa(win));
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setFloatingPanel(true);
        window.setLevel(LEVEL);
        window.setHidesOnDeactivate(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        unsafe {
            let _: () = msg_send![&*window, setAnimationBehavior: 2isize]; // NSWindowAnimationBehaviorNone
            let _: () = msg_send![&*window, setTitle: &*ns("Drag Shelf")];
        }
        let view = ShelfView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(win.w, win.h)), id);
        let bounds = Rect::new(0.0, 0.0, win.w, win.h);
        let root = gfx::layer();
        gfx::set_frame(&root, bounds);
        view::host(&view, &root);
        window.setContentView(Some(&view));

        let appear_at = appear_at.unwrap_or(Point::new(rect.center_x(), rect.center_y()));
        let group = gfx::layer();
        gfx::add(&root, &group);
        let a = Point::new(((appear_at.x - win.x) / win.w).clamp(0.0, 1.0), (1.0 - (appear_at.y - win.y) / win.h).clamp(0.0, 1.0));
        gfx::set_anchor(&group, a.x, a.y);
        gfx::set_frame(&group, bounds);

        let mk = |parent: &AnyObject| {
            let l = gfx::layer();
            gfx::set_scale(&l, scale);
            gfx::add(parent, &l);
            l
        };
        let mkt = |parent: &AnyObject| {
            let l = gfx::text_layer(scale);
            gfx::add(parent, &l);
            l
        };
        let shadow = mk(&group);
        gfx::set_bg(&shadow, Some(&Color::rgba(0.075, 0.075, 0.085, 0.97)));
        gfx::set_corner(&shadow, m.radius);
        gfx::set_border(&shadow, 1.0, Some(&white(0.11)));
        gfx::set_shadow(&shadow, 0.5, 22.0, -10.0);
        let content = mk(&group);
        gfx::set_corner(&content, m.radius);
        gfx::set_masks_to_bounds(&content, true);
        let gloss: Layer = unsafe { msg_send![objc2::class!(CAGradientLayer), layer] };
        gfx::add(&content, &gloss);
        unsafe {
            let (c0, c1) = (white(0.075), white(0.0));
            let objs: [&AnyObject; 2] = [&*(c0.0 .0 as *const AnyObject), &*(c1.0 .0 as *const AnyObject)];
            let colors = objc2_foundation::NSArray::from_slice(&objs);
            let _: () = msg_send![&*gloss, setColors: &*colors];
            let _: () = msg_send![&*gloss, setStartPoint: NSPoint::new(0.5, 1.0)];
            let _: () = msg_send![&*gloss, setEndPoint: NSPoint::new(0.5, 0.0)];
        }

        // Fixed heights: children are placed relative to their parent's
        // height (the layer tree is y-up), so these never change it.
        let stack = mk(&content);
        let (sw, sh) = lay::panel_size(Mode::Stack, 0, &m);
        gfx::set_frame(&stack, Rect::new(0.0, 0.0, sw, sh));
        let mut cards = vec![];
        for _ in 0..3 {
            let root = mk(&stack);
            let img = mk(&root);
            gfx::set_shadow(&img, 0.45, 7.0, -3.0);
            let plate = mk(&root);
            gfx::set_shadow(&plate, 0.4, 7.0, -3.0);
            let glyph = mk(&plate);
            let text = mkt(&plate);
            cards.push(CardL { root, img, plate, glyph, text, key: String::new() });
        }
        let badge = mk(&stack);
        gfx::set_corner(&badge, 10.0);
        gfx::set_border(&badge, 2.0, Some(&Color::rgba(0.075, 0.075, 0.085, 1.0)));
        let badge_text = mkt(&badge);
        let caption = mkt(&stack);
        let subcaption = mkt(&stack);
        let empty = mk(&stack);
        let empty_border = gfx::shape_layer();
        gfx::add(&empty, &empty_border);
        gfx::set_dash(&empty_border, &[5.0, 4.0]);
        let empty_icon = mk(&empty);
        let empty_text = mkt(&empty);

        let grid = mk(&content);
        gfx::set_frame(&grid, Rect::new(0.0, 0.0, sw, grid_max_h(&m)));
        let grid_clip = mk(&grid);
        gfx::set_masks_to_bounds(&grid_clip, true);
        let title = mkt(&content);

        let btn = |parent: &AnyObject| {
            let bg = mk(parent);
            gfx::set_corner(&bg, m.button / 2.0);
            let icon = mk(&bg);
            Btn { bg, icon }
        };
        let close = btn(&content);
        let toggle = btn(&content);
        let menu = btn(&content);
        let ring = mk(&group);
        gfx::set_corner(&ring, m.radius - 1.5);
        gfx::set_hidden(&ring, true);

        let mut appear = Spring::new(430.0, Spring::damping_for_ratio(430.0, 0.62), 0.0);
        appear.target = 1.0;
        if self.no_appear {
            appear = Spring::new(1.0, 1.0, 1.0);
        }
        let p = Panel {
            id,
            window,
            view,
            scale,
            win,
            rect,
            from: rect,
            to: rect,
            resize: Spring::new(1.0, 1.0, 1.0),
            mode,
            mix: if mode == Mode::Grid { 1.0 } else { 0.0 },
            appear,
            appear_at,
            closing: None,
            root,
            group,
            shadow,
            content,
            gloss,
            stack,
            cards,
            badge,
            badge_text,
            caption,
            subcaption,
            empty,
            empty_border,
            empty_icon,
            empty_text,
            grid,
            grid_clip,
            tiles: vec![],
            title,
            close,
            toggle,
            menu,
            ring,
            items: vec![],
            hover: Hit::Outside,
            drop_hover: false,
            scroll: 0.0,
            selected: BTreeSet::new(),
            sel_anchor: None,
            emptied_at: None,
            flash: None,
            dirty: true,
            state_key: String::new(),
            move_pending: false,
        };
        p.window.orderFrontRegardless();
        p
    }

    /// Lay out the panel for its current (animated) rect.
    fn place(&mut self, i: usize) {
        let m = self.m;
        let p = &self.panels[i];
        let local = p.rect.offset(-p.win.x, -p.win.y);
        let (w, h) = (local.w, local.h);
        let n = p.items.len();
        let (sw, sh) = lay::panel_size(Mode::Stack, n, &m);
        let (gw, _) = lay::panel_size(Mode::Grid, n.max(1), &m);
        let gh = grid_max_h(&m);
        let b = m.button;
        let by = (m.header_h - b) / 2.0 + 1.0;
        let close = Rect::new(12.0, by, b, b);
        let menu = Rect::new(w - 12.0 - b, by, b, b);
        let toggle = Rect::new(menu.x - 4.0 - b, by, b, b);
        transaction(None, || {
            gfx::set_frame(&p.shadow, local);
            gfx::set_shadow_path(&p.shadow, &Path::rounded_rect(Rect::new(0.0, 0.0, w, h), m.radius));
            gfx::set_frame(&p.content, local);
            gfx::set_frame(&p.gloss, Rect::new(0.0, 0.0, w, h.min(130.0)));
            gfx::set_frame(&p.stack, Rect::new((w - sw) / 2.0, 0.0, sw, sh));
            gfx::set_opacity(&p.stack, 1.0 - p.mix);
            gfx::set_frame(&p.grid, Rect::new((w - gw) / 2.0, 0.0, gw, gh));
            gfx::set_opacity(&p.grid, p.mix);
            gfx::set_hidden(&p.grid, p.mix < 0.01);
            gfx::set_hidden(&p.stack, p.mix > 0.99);
            for (btn, r) in [(&p.close, close), (&p.toggle, toggle), (&p.menu, menu)] {
                gfx::set_frame(&btn.bg, r);
            }
            gfx::set_frame(&p.title, Rect::new(close.right() + 10.0, by + 4.0, (toggle.x - close.right() - 20.0).max(0.0), 17.0));
            gfx::set_opacity(&p.title, p.mix);
            gfx::set_frame(&p.ring, local.inflate(-1.5, -1.5));
        });
    }

    fn ensure_thumb(&mut self, item: &ShelfItem, scale: f64) {
        let size = self.m.card;
        let Some(path) = item.path() else { return };
        let key = thumb_key(path);
        let th = self.thumbs.entry(key).or_default();
        if th.icon.is_none() {
            let icon = pboard::file_icon(path);
            th.icon = gfx::rasterize(&icon, size, size, scale, None);
        }
        if !th.requested {
            th.requested = true;
            let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            pboard::request_thumbnail(key, &real, size, scale, thumb_ready);
        }
    }

    fn image_of(&self, it: &ShelfItem) -> Option<Image> {
        let th = self.thumbs.get(&thumb_key(it.path()?))?;
        th.ql.clone().or_else(|| th.icon.clone())
    }

    /// Refill and re-place everything (renders, debug).
    fn refresh(&mut self, i: usize) {
        self.panels[i].dirty = true;
        self.fill(i);
        self.place(i);
        self.panels[i].state_key.clear();
        self.update_states(i);
    }

    /// Cards, badge, captions, empty zone, tiles and the header title.
    fn fill(&mut self, i: usize) {
        let m = self.m;
        let scale = self.panels[i].scale;
        let items = self.panels[i].items.clone();
        for it in &items {
            self.ensure_thumb(it, scale);
        }
        self.panels[i].dirty = false;
        let n = items.len();
        let l = lay::layout(Mode::Stack, n, 0.0, &m);
        // Back to front: the newest item is on top.
        let shown: Vec<&ShelfItem> = items.iter().rev().take(3).rev().collect();
        let link_glyph = self.sym("link", 30.0, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.95), scale);
        let text_glyph = self.sym("text.quote", 13.0, gfx::W_SEMIBOLD, (0.35, 0.35, 0.40, 1.0), scale);
        let accent = self.accent(1.0);
        for k in 0..3 {
            let card = l.cards.get(k).copied();
            let it = shown.get(k).copied();
            let image = it.and_then(|it| self.image_of(it));
            let has_ql = it.and_then(|it| it.path()).and_then(|p| self.thumbs.get(&thumb_key(p))).is_some_and(|t| t.ql.is_some());
            let p = &mut self.panels[i];
            let c = &mut p.cards[k];
            let (Some(card), Some(it)) = (card, it) else {
                transaction(None, || gfx::set_hidden(&c.root, true));
                c.key.clear();
                continue;
            };
            let key = format!("{:?}|{:?}|{has_ql}|{}", card, it.kind, n);
            if c.key == key {
                continue;
            }
            c.key = key;
            transaction(None, || {
                gfx::set_hidden(&c.root, false);
                gfx::set_transform(&c.root, gfx::AffineTransform::scale(1.0));
                gfx::set_frame(&c.root, card.rect);
                // Clockwise on screen is negative in the y-up layer space.
                gfx::set_transform(&c.root, rotate(-card.angle));
                let r = Rect::new(0.0, 0.0, card.rect.w, card.rect.h);
                match &it.kind {
                    ItemKind::File { .. } => {
                        gfx::set_hidden(&c.plate, true);
                        gfx::set_hidden(&c.img, false);
                        gfx::set_frame(&c.img, r);
                        gfx::set_image(&c.img, image.as_ref());
                    }
                    ItemKind::Url { url, .. } => {
                        gfx::set_hidden(&c.img, true);
                        gfx::set_hidden(&c.plate, false);
                        let pr = r.inflate(-8.0, -8.0);
                        gfx::set_frame(&c.plate, pr);
                        gfx::set_corner(&c.plate, 22.0);
                        gfx::set_bg(&c.plate, Some(&accent));
                        gfx::set_border(&c.plate, 0.5, Some(&white(0.25)));
                        gfx::place_symbol(&c.glyph, link_glyph.as_ref(), Rect::new(0.0, 8.0, pr.w, pr.h - 30.0));
                        let host = pm_shelf_core::store::url_host(url).unwrap_or(url).to_string();
                        gfx::set_frame(&c.text, Rect::new(6.0, pr.h - 27.0, pr.w - 12.0, 15.0));
                        gfx::set_text(&c.text, &host, &TextStyle { size: 10.5, weight: gfx::W_SEMIBOLD, align: "center", truncate: "middle" }, &white(0.92));
                    }
                    ItemKind::Text { text } => {
                        gfx::set_hidden(&c.img, true);
                        gfx::set_hidden(&c.plate, false);
                        let pr = Rect::new(14.0, 8.0, r.w - 28.0, r.h - 16.0);
                        gfx::set_frame(&c.plate, pr);
                        gfx::set_corner(&c.plate, 10.0);
                        gfx::set_bg(&c.plate, Some(&Color::rgba(0.97, 0.96, 0.92, 1.0)));
                        gfx::set_border(&c.plate, 0.0, None);
                        gfx::place_symbol(&c.glyph, text_glyph.as_ref(), Rect::new(8.0, 7.0, 16.0, 16.0));
                        let snippet: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                        gfx::set_frame(&c.text, Rect::new(9.0, 24.0, pr.w - 16.0, pr.h - 30.0));
                        gfx::set_text(&c.text, &pm_shelf_core::store::truncate_chars(&snippet, 160), &TextStyle { size: 9.5, weight: gfx::W_MEDIUM, align: "left", truncate: "end" }, &Color::rgba(0.20, 0.20, 0.24, 1.0));
                        unsafe {
                            let _: () = msg_send![&*c.text, setWrapped: true];
                        }
                    }
                }
            });
        }
        // Badge, captions, empty zone.
        let (cap, sub) = crate::model::summary(&items);
        let flash = self.panels[i].flash.as_ref().map(|(s, _)| s.clone());
        let accent_c = self.accent(1.0);
        let p = &self.panels[i];
        let count = n.to_string();
        let bw = (gfx::text_width(&count, 11.0, gfx::W_BOLD) + 14.0).max(22.0);
        transaction(None, || {
            gfx::set_hidden(&p.badge, n < 2);
            let b = l.badge;
            gfx::set_frame(&p.badge, Rect::new(b.center_x() - bw / 2.0, b.y, bw, 22.0));
            gfx::set_corner(&p.badge, 11.0);
            gfx::set_bg(&p.badge, Some(&accent_c));
            gfx::set_frame(&p.badge_text, Rect::new(0.0, 3.5, bw, 15.0));
            gfx::set_text(&p.badge_text, &count, &TextStyle { size: 11.0, weight: gfx::W_BOLD, align: "center", truncate: "end" }, &white(1.0));
            gfx::set_hidden(&p.caption, n == 0);
            gfx::set_hidden(&p.subcaption, n == 0);
            gfx::set_frame(&p.caption, l.caption);
            gfx::set_text(&p.caption, &cap, &TextStyle { size: 13.0, weight: gfx::W_SEMIBOLD, align: "center", truncate: "middle" }, &white(0.95));
            gfx::set_frame(&p.subcaption, l.subcaption);
            match &flash {
                Some(f) => gfx::set_text(&p.subcaption, f, &TextStyle { size: 11.0, weight: gfx::W_SEMIBOLD, align: "center", truncate: "end" }, &accent_c),
                None => gfx::set_text(&p.subcaption, &sub, &TextStyle { size: 11.0, weight: gfx::W_MEDIUM, align: "center", truncate: "end" }, &white(0.5)),
            }
            gfx::set_hidden(&p.empty, n > 0);
            gfx::set_frame(&p.empty, l.empty);
            let er = Rect::new(0.0, 0.0, l.empty.w, l.empty.h);
            gfx::set_frame(&p.empty_border, er);
            gfx::set_path(&p.empty_border, &Path::rounded_rect(er.inflate(-1.0, -1.0), m.radius - m.pad + 4.0));
        });
        self.fill_tiles(i);
        let p = &mut self.panels[i];
        p.state_key.clear();
    }

    fn fill_tiles(&mut self, i: usize) {
        let m = self.m;
        let scale = self.panels[i].scale;
        let items = self.panels[i].items.clone();
        let mut old: HashMap<u64, Tile> = self.panels[i].tiles.drain(..).map(|t| (t.id, t)).collect();
        let mut tiles = Vec::with_capacity(items.len());
        let parts = lay::tile_parts(&m);
        let link = self.sym("link", 20.0, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.95), scale);
        let text = self.sym("text.alignleft", 20.0, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.95), scale);
        let xmark = self.sym("xmark", 8.5, gfx::W_BOLD, (1.0, 1.0, 1.0, 0.9), scale);
        let accent = self.accent(0.95);
        for it in &items {
            let mut t = match old.remove(&it.id) {
                Some(t) => t,
                None => self.new_tile(i, it.id),
            };
            let has_ql = it.path().and_then(|p| self.thumbs.get(&thumb_key(p))).is_some_and(|t| t.ql.is_some());
            let key = format!("{:?}|{has_ql}", it.kind);
            if t.key != key {
                t.key = key;
                let image = self.image_of(it);
                let label = it.display_name();
                let st = TextStyle { size: 11.0, weight: gfx::W_MEDIUM, align: "center", truncate: if it.path().is_some() { "middle" } else { "end" } };
                let plate_r = Rect::new(parts.thumb.center_x() - 23.0, parts.thumb.center_y() - 23.0, 46.0, 46.0);
                transaction(None, || {
                    gfx::set_frame(&t.thumb, parts.thumb);
                    gfx::set_frame(&t.label, parts.label);
                    gfx::set_text(&t.label, &label, &st, &white(0.88));
                    gfx::set_frame(&t.remove, parts.remove);
                    gfx::place_symbol(&t.remove_icon, xmark.as_ref(), Rect::new(0.0, 0.0, parts.remove.w, parts.remove.h));
                    let (glyph, color) = match &it.kind {
                        ItemKind::File { .. } => (None, None),
                        ItemKind::Url { .. } => (link.as_ref(), Some(&accent)),
                        ItemKind::Text { .. } => (text.as_ref(), None),
                    };
                    match &it.kind {
                        ItemKind::File { .. } => {
                            gfx::set_hidden(&t.plate, true);
                            gfx::set_image(&t.thumb, image.as_ref());
                        }
                        _ => {
                            let w16 = white(0.16);
                            gfx::set_frame(&t.plate, plate_r);
                            gfx::set_bg(&t.plate, Some(color.unwrap_or(&w16)));
                            gfx::set_hidden(&t.plate, false);
                            gfx::place_symbol(&t.glyph, glyph, Rect::new(0.0, 0.0, plate_r.w, plate_r.h));
                            gfx::set_image(&t.thumb, None);
                        }
                    }
                });
            }
            tiles.push(t);
        }
        for (_, t) in old {
            gfx::remove(&t.root);
        }
        let p = &mut self.panels[i];
        p.tiles = tiles;
        self.place_tiles(i);
    }

    fn new_tile(&self, i: usize, id: u64) -> Tile {
        let p = &self.panels[i];
        let m = self.m;
        let mk = |parent: &AnyObject| {
            let l = gfx::layer();
            gfx::set_scale(&l, p.scale);
            gfx::add(parent, &l);
            l
        };
        let root = mk(&p.grid_clip);
        gfx::set_frame(&root, Rect::new(0.0, 0.0, m.tile_w, m.tile_h));
        gfx::set_corner(&root, 14.0);
        let plate = mk(&root);
        gfx::set_corner(&plate, 12.0);
        let thumb = mk(&root);
        gfx::set_shadow(&thumb, 0.35, 3.0, -1.5);
        let glyph = mk(&plate);
        let label = gfx::text_layer(p.scale);
        gfx::add(&root, &label);
        let remove = mk(&root);
        gfx::set_corner(&remove, 9.0);
        gfx::set_bg(&remove, Some(&Color::rgba(0.30, 0.30, 0.32, 1.0)));
        gfx::set_border(&remove, 0.5, Some(&white(0.25)));
        gfx::set_shadow(&remove, 0.4, 2.0, -1.0);
        let remove_icon = mk(&remove);
        gfx::set_opacity(&remove, 0.0);
        Tile { id, root, plate, thumb, glyph, label, remove, remove_icon, state: None, key: String::new() }
    }

    fn place_tiles(&mut self, i: usize) {
        let m = self.m;
        let p = &mut self.panels[i];
        let n = p.items.len();
        let l = lay::layout(Mode::Grid, n.max(1), p.scroll, &m);
        p.scroll = l.scroll;
        let p = &self.panels[i];
        transaction(None, || {
            gfx::set_frame(&p.grid_clip, l.grid);
            for (t, r) in p.tiles.iter().zip(&l.tiles) {
                gfx::set_frame(&t.root, r.offset(-l.grid.x, -l.grid.y));
            }
        });
    }

    /// Hover / selection / drop-target visuals and the header.
    fn update_states(&mut self, i: usize) {
        let scale = self.panels[i].scale;
        let p = &self.panels[i];
        let n = p.items.len();
        let key = format!("{:?}|{}|{:?}|{}|{}", p.hover, p.drop_hover, p.mode, n, p.selected.len());
        if p.state_key == key {
            return;
        }
        let mode = p.mode;
        let drop = p.drop_hover;
        let close_i = self.sym("xmark", 9.5, gfx::W_BOLD, (1.0, 1.0, 1.0, 0.85), scale);
        let toggle_i = self.sym(if mode == Mode::Grid { "rectangle.stack.fill" } else { "square.grid.2x2.fill" }, 11.5, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.85), scale);
        let menu_i = self.sym("ellipsis", 12.0, gfx::W_BOLD, (1.0, 1.0, 1.0, 0.85), scale);
        let (er, eg, eb) = if drop { self.accent } else { (1.0, 1.0, 1.0) };
        let empty_icon = self.sym("tray.and.arrow.down.fill", 26.0, gfx::W_REGULAR, (er, eg, eb, if drop { 1.0 } else { 0.5 }), scale);
        let sel_bg = self.accent(0.30);
        let sel_border = self.accent(0.85);
        let ring = self.accent(0.95);
        let ring_fill = self.accent(0.10);
        let e_border = if drop { self.accent(0.95) } else { white(0.22) };
        let e_fill = if drop { self.accent(0.12) } else { white(0.025) };
        let e_text = Color::rgba(er, eg, eb, if drop { 1.0 } else { 0.55 });
        let m = self.m;
        let p = &mut self.panels[i];
        p.state_key = key;
        let hover_tile = match p.hover {
            Hit::Tile(k) | Hit::Remove(k) => Some(k),
            _ => None,
        };
        let remove_hot = matches!(p.hover, Hit::Remove(_));
        let mut changed = vec![];
        for (k, t) in p.tiles.iter_mut().enumerate() {
            let st = TileState { selected: p.selected.contains(&t.id), hover: hover_tile == Some(k), remove_hot: remove_hot && hover_tile == Some(k) };
            if t.state != Some(st) {
                t.state = Some(st);
                changed.push(k);
            }
        }
        let p = &self.panels[i];
        let hot = |h: Hit| p.hover == h;
        let title = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        transaction(Some(0.14), || {
            for k in changed {
                let t = &p.tiles[k];
                let st = t.state.unwrap_or_default();
                let hover_bg = white(0.08);
                gfx::set_bg(&t.root, if st.selected { Some(&sel_bg) } else if st.hover { Some(&hover_bg) } else { None });
                gfx::set_border(&t.root, if st.selected { 1.0 } else { 0.0 }, if st.selected { Some(&sel_border) } else { None });
                gfx::set_opacity(&t.remove, if st.hover { 1.0 } else { 0.0 });
                let rbg = if st.remove_hot { Color::rgba(0.42, 0.42, 0.45, 1.0) } else { Color::rgba(0.30, 0.30, 0.32, 1.0) };
                gfx::set_bg(&t.remove, Some(&rbg));
            }
            for (btn, sym, h) in [(&p.close, &close_i, Hit::Close), (&p.toggle, &toggle_i, Hit::Toggle), (&p.menu, &menu_i, Hit::Menu)] {
                let bg = if hot(h) { white(0.20) } else { white(0.09) };
                gfx::set_bg(&btn.bg, Some(&bg));
                gfx::place_symbol(&btn.icon, sym.as_ref(), Rect::new(0.0, 0.0, m.button, m.button));
            }
            gfx::set_hidden(&p.toggle.bg, n == 0);
            gfx::set_text(&p.title, &title, &TextStyle { size: 13.0, weight: gfx::W_SEMIBOLD, align: "left", truncate: "end" }, &white(0.95));
            // Empty zone and drop ring.
            let ez = lay::layout(Mode::Stack, 0, 0.0, &m).empty;
            gfx::place_symbol(&p.empty_icon, empty_icon.as_ref(), Rect::new(0.0, ez.h / 2.0 - 34.0, ez.w, 40.0));
            gfx::set_frame(&p.empty_text, Rect::new(8.0, ez.h / 2.0 + 12.0, ez.w - 16.0, 17.0));
            gfx::set_text(&p.empty_text, if drop { "Release to add" } else { "Drop files here" }, &TextStyle { size: 12.5, weight: gfx::W_MEDIUM, align: "center", truncate: "end" }, &e_text);
            gfx::set_stroke(&p.empty_border, Some(&e_border), 1.2);
            gfx::set_fill(&p.empty_border, Some(&e_fill));
            gfx::set_dash(&p.empty_border, if drop { &[] } else { &[5.0, 4.0] });
            gfx::set_hidden(&p.ring, !(drop && n > 0));
            gfx::set_border(&p.ring, 2.0, Some(&ring));
            gfx::set_bg(&p.ring, Some(&ring_fill));
            gfx::set_opacity(&p.caption, if drop && n > 0 { 0.45 } else { 1.0 });
            if drop && n > 0 {
                gfx::set_text(&p.subcaption, "Release to add", &TextStyle { size: 11.0, weight: gfx::W_SEMIBOLD, align: "center", truncate: "end" }, &e_text);
            }
        });
    }

    // ----- events -----------------------------------------------------

    fn panel_local(&self, i: usize, pt: Point) -> Point {
        let p = &self.panels[i];
        Point::new(pt.x - (p.rect.x - p.win.x), pt.y - (p.rect.y - p.win.y))
    }

    /// A panel-local rect in window coordinates.
    fn to_window(&self, i: usize, r: Rect) -> Rect {
        let p = &self.panels[i];
        r.offset(p.rect.x - p.win.x, p.rect.y - p.win.y)
    }

    fn hit(&self, i: usize, pt: Point) -> Hit {
        let p = &self.panels[i];
        let l = lay::layout(p.mode, p.items.len(), p.scroll, &self.m);
        let hover_tile = match p.hover {
            Hit::Tile(k) | Hit::Remove(k) => Some(k),
            _ => None,
        };
        lay::hit_test(&l, p.mode, self.panel_local(i, pt), hover_tile, &self.m)
    }

    fn ids_of(&self, i: usize, sel_only: bool) -> Vec<u64> {
        let p = &self.panels[i];
        p.items.iter().filter(|it| !sel_only || p.selected.contains(&it.id)).map(|it| it.id).collect()
    }

    fn items_of(&self, shelf: u64, ids: &[u64]) -> Vec<ShelfItem> {
        self.idx(shelf).map(|i| self.panels[i].items.iter().filter(|it| ids.contains(&it.id)).cloned().collect()).unwrap_or_default()
    }

    fn remove_items(&mut self, shelf: u64, ids: &[u64]) {
        if !ids.is_empty() {
            self.shared.edit(|s| s.remove_items(shelf, ids));
        }
    }

    fn toggle_mode(&mut self, i: usize) {
        let p = &mut self.panels[i];
        if p.items.is_empty() {
            return;
        }
        p.mode = if p.mode == Mode::Grid { Mode::Stack } else { Mode::Grid };
        // Forces `retarget` to size the panel for the new mode.
        p.to = Rect::default();
        self.retarget(i, true);
        self.panels[i].state_key.clear();
        self.schedule(true);
    }

    fn close_panel(&mut self, i: usize) {
        let id = self.panels[i].id;
        self.shared.edit(|s| s.close(id));
        self.panels[i].closing = Some(0.0);
        self.schedule(true);
    }

    fn menu_entries(&self, items: &[ShelfItem], selection: bool, grid: bool) -> Vec<(String, isize, &'static str, bool)> {
        let n = items.len();
        let any = n > 0;
        let any_file = items.iter().any(|i| i.path().is_some());
        let suffix = if n > 1 { format!(" {n} Items") } else { String::new() };
        let mut e = vec![
            (format!("Open{suffix}"), M_OPEN, "arrow.up.forward.app", any),
            ("Show in Finder".to_string(), M_REVEAL, "folder", any_file),
            ("Copy".to_string(), M_COPY, "doc.on.doc", any),
            ("Share…".to_string(), M_SHARE, "square.and.arrow.up", any),
            (String::new(), -1, "", false),
            ("Copy to…".to_string(), M_COPY_TO, "folder.badge.plus", any),
            ("Move to…".to_string(), M_MOVE_TO, "arrow.right.doc.on.clipboard", any_file),
            (String::new(), -1, "", false),
        ];
        if selection {
            e.push((format!("Remove{suffix}"), M_REMOVE, "xmark.circle", any));
        } else {
            e.push((if grid { "Show as Stack" } else { "Show All Items" }.to_string(), M_GRID, if grid { "rectangle.stack" } else { "square.grid.2x2" }, any));
            e.push(("Clear Shelf".to_string(), M_CLEAR, "trash", any));
        }
        e.push(("Close Shelf".to_string(), M_CLOSE, "xmark.square", true));
        e
    }

    pub fn on_mouse_down(&mut self, shelf: u64, pt: Point, mods: Mods, clicks: isize) -> Option<Deferred> {
        let i = self.idx(shelf)?;
        if self.panels[i].closing.is_some() {
            return None;
        }
        let hit = self.hit(i, pt);
        self.press = Some(Press { shelf, start: pt, hit, mods, dragged: false });
        self.panels[i].state_key.clear();
        match hit {
            Hit::Close => {
                self.press = None;
                self.close_panel(i);
                None
            }
            Hit::Toggle => {
                self.press = None;
                self.toggle_mode(i);
                None
            }
            Hit::Menu => {
                self.press = None;
                let ids = self.ids_of(i, false);
                let items = self.items_of(shelf, &ids);
                let l = lay::layout(self.panels[i].mode, items.len(), self.panels[i].scroll, &self.m);
                let anchor = self.to_window(i, l.menu);
                self.menu_ctx = Some((shelf, ids, anchor));
                let entries = self.menu_entries(&items, false, self.panels[i].mode == Mode::Grid);
                Some(Deferred::Menu { entries, at: Point::new(anchor.x, anchor.bottom() + 4.0) })
            }
            Hit::Remove(k) => {
                self.press = None;
                let id = self.panels[i].items.get(k)?.id;
                self.remove_items(shelf, &[id]);
                None
            }
            Hit::Tile(k) => {
                let id = self.panels[i].items.get(k)?.id;
                let p = &mut self.panels[i];
                if mods.cmd {
                    if !p.selected.remove(&id) {
                        p.selected.insert(id);
                    }
                } else if mods.shift && p.sel_anchor.is_some() {
                    let a = p.items.iter().position(|d| Some(d.id) == p.sel_anchor).unwrap_or(k);
                    let (lo, hi) = (a.min(k), a.max(k));
                    p.selected = p.items[lo..=hi].iter().map(|d| d.id).collect();
                } else if !p.selected.contains(&id) {
                    p.selected = [id].into_iter().collect();
                }
                if !mods.shift {
                    p.sel_anchor = Some(id);
                }
                if clicks == 2 {
                    if let Some(it) = p.items.iter().find(|d| d.id == id).cloned() {
                        pboard::open(&[it]);
                    }
                }
                None
            }
            Hit::Stack => None,
            Hit::Background => {
                self.press = None;
                if !mods.cmd {
                    self.panels[i].selected.clear();
                }
                Some(Deferred::MoveWindow)
            }
            Hit::Outside => {
                self.press = None;
                None
            }
        }
    }

    pub fn on_mouse_dragged(&mut self, shelf: u64, pt: Point) -> Option<Deferred> {
        let press = self.press.as_ref()?;
        if press.dragged || press.shelf != shelf || (pt.x - press.start.x).hypot(pt.y - press.start.y) < 4.0 {
            return None;
        }
        let hit = press.hit;
        let i = self.idx(shelf)?;
        let ids: Vec<u64> = match hit {
            Hit::Tile(k) => {
                let id = self.panels[i].items.get(k)?.id;
                if self.panels[i].selected.contains(&id) {
                    self.ids_of(i, true)
                } else {
                    vec![id]
                }
            }
            Hit::Stack => self.ids_of(i, false),
            _ => return None,
        };
        if let Some(pr) = self.press.as_mut() {
            pr.dragged = true;
        }
        let m = self.m;
        let p = &self.panels[i];
        let mut out = vec![];
        match hit {
            Hit::Tile(_) => {
                let l = lay::layout(Mode::Grid, p.items.len(), p.scroll, &m);
                for id in &ids {
                    let Some(k) = p.items.iter().position(|d| d.id == *id) else { continue };
                    let it = &p.items[k];
                    let frame = self.to_window(i, l.tiles[k]);
                    let img = p.tiles.get(k).and_then(|t| gfx::render_layer(&t.root, m.tile_w, m.tile_h, p.scale, false));
                    let ns_img = match img {
                        Some(img) => gfx::nsimage_from_cg(&img, m.tile_w, m.tile_h),
                        None => match it.path() {
                            Some(path) => pboard::file_icon(path),
                            None => gfx::symbol_nsimage("doc", 40.0, gfx::W_REGULAR)?,
                        },
                    };
                    out.push((pboard::writer(it), frame, ns_img));
                }
            }
            _ => {
                // The stack: each card as it looks, the rest like the front card.
                let l = lay::layout(Mode::Stack, p.items.len(), 0.0, &m);
                let n = p.items.len();
                let stack_x = (p.rect.w - m.stack_w) / 2.0;
                for (k, it) in p.items.iter().enumerate() {
                    let card_k = (k + 3).checked_sub(n).filter(|c| *c < l.cards.len());
                    let card = card_k.map(|c| l.cards[c]).or(l.cards.last().copied());
                    let Some(card) = card else { continue };
                    let frame = self.to_window(i, card.rect.offset(stack_x, 0.0));
                    let img = card_k.and_then(|c| gfx::render_layer(&p.cards[c].root, card.rect.w, card.rect.h, p.scale, false));
                    let ns_img = match (img, self.image_of(it)) {
                        (Some(img), _) => gfx::nsimage_from_cg(&img, card.rect.w, card.rect.h),
                        (None, Some(img)) => gfx::nsimage_from_cg(&img, card.rect.w, card.rect.h),
                        (None, None) => gfx::symbol_nsimage(if matches!(it.kind, ItemKind::Url { .. }) { "link" } else { "text.alignleft" }, 40.0, gfx::W_REGULAR)?,
                    };
                    out.push((pboard::writer(it), frame, ns_img));
                }
            }
        }
        self.own_drag = Some((shelf, ids));
        self.watch.set_own_drag(true);
        Some(Deferred::DragOut(out))
    }

    pub fn on_mouse_up(&mut self, shelf: u64, pt: Point) -> Option<Deferred> {
        let press = self.press.take()?;
        let i = self.idx(shelf)?;
        self.panels[i].state_key.clear();
        if press.dragged || press.shelf != shelf {
            return None;
        }
        let hit = self.hit(i, pt);
        match press.hit {
            // A click on the stack spreads it into the grid.
            Hit::Stack if hit == Hit::Stack => self.toggle_mode(i),
            Hit::Tile(k) if !press.mods.cmd && !press.mods.shift => {
                let p = &mut self.panels[i];
                if let Some(id) = p.items.get(k).map(|d| d.id) {
                    if p.selected.len() > 1 {
                        p.selected = [id].into_iter().collect();
                    }
                }
            }
            _ => {}
        }
        None
    }

    pub fn on_right_down(&mut self, shelf: u64, pt: Point) -> Option<Deferred> {
        let i = self.idx(shelf)?;
        let hit = self.hit(i, pt);
        let (ids, selection) = match hit {
            Hit::Tile(k) | Hit::Remove(k) => {
                let id = self.panels[i].items.get(k)?.id;
                let p = &mut self.panels[i];
                if !p.selected.contains(&id) {
                    p.selected = [id].into_iter().collect();
                }
                (self.ids_of(i, true), true)
            }
            Hit::Outside => return None,
            _ => (self.ids_of(i, false), false),
        };
        let items = self.items_of(shelf, &ids);
        self.menu_ctx = Some((shelf, ids, Rect::new(pt.x - 1.0, pt.y - 1.0, 2.0, 2.0)));
        self.panels[i].state_key.clear();
        let entries = self.menu_entries(&items, selection, self.panels[i].mode == Mode::Grid);
        Some(Deferred::Menu { entries, at: pt })
    }

    pub fn on_menu(&mut self, tag: isize) -> Option<Deferred> {
        let (shelf, ids, anchor) = self.menu_ctx.clone()?;
        let items = self.items_of(shelf, &ids);
        match tag {
            M_OPEN => pboard::open(&items),
            M_REVEAL => pboard::reveal(&items),
            M_COPY => pboard::copy_to_clipboard(&items),
            M_SHARE => {
                self.sharing = !items.is_empty();
                return Some(Deferred::Share { items: items.iter().map(pboard::writer).collect(), anchor });
            }
            M_COPY_TO | M_MOVE_TO => {
                let mv = tag == M_MOVE_TO;
                let n = items.len();
                self.folder_ctx = Some((shelf, ids, mv));
                return Some(Deferred::ChooseFolder { mv, count: n });
            }
            M_REMOVE => self.remove_items(shelf, &ids),
            M_CLEAR => {
                self.shared.edit(|s| s.clear(shelf));
            }
            M_GRID => {
                if let Some(i) = self.idx(shelf) {
                    self.toggle_mode(i);
                }
            }
            M_CLOSE => {
                if let Some(i) = self.idx(shelf) {
                    self.close_panel(i);
                }
            }
            _ => {}
        }
        None
    }

    pub fn on_folder_chosen(&mut self, dest: Option<PathBuf>) {
        let Some((shelf, ids, mv)) = self.folder_ctx.take() else { return };
        let Some(dest) = dest else { return };
        let items = self.items_of(shelf, &ids);
        log::info!("drag shelf: {} {} item(s) to {}", if mv { "moving" } else { "copying" }, items.len(), dest.display());
        std::thread::spawn(move || {
            let out = fileops::put_into(&items, &dest, mv);
            let msg = fileops::report(&out, &dest, mv);
            let moved: Vec<(u64, PathBuf)> = out.into_iter().filter(|o| o.moved).filter_map(|o| Some((o.id, o.result.ok()?))).collect();
            dispatch_main(move || {
                with(|a| a.folder_done(shelf, moved, msg));
            });
        });
    }

    fn folder_done(&mut self, shelf: u64, moved: Vec<(u64, PathBuf)>, msg: String) {
        log::info!("drag shelf: {msg}");
        if !moved.is_empty() {
            self.shared.edit(|s| {
                s.edit(shelf, |st| {
                    let mut any = false;
                    for (id, p) in &moved {
                        any |= st.relocate(*id, p.clone());
                    }
                    any
                })
            });
            // Thumbnails are keyed by path.
        }
        if let Some(i) = self.idx(shelf) {
            self.panels[i].flash = Some((msg, now() + FLASH_SECS));
            self.panels[i].dirty = true;
        }
        self.schedule(true);
    }

    pub fn on_scroll(&mut self, shelf: u64, dx: f64, dy: f64, precise: bool) {
        let Some(i) = self.idx(shelf) else { return };
        if self.panels[i].mode != Mode::Grid {
            return;
        }
        let mut d = if dy.abs() >= dx.abs() { dy } else { dx };
        if !precise {
            d *= 14.0;
        }
        let p = &mut self.panels[i];
        let before = p.scroll;
        p.scroll = (p.scroll - d).max(0.0);
        self.place_tiles(i);
        if self.panels[i].scroll != before {
            self.panels[i].state_key.clear();
        }
    }

    /// A drag is over the window at `pt` (window coordinates); returns
    /// whether it is over the panel itself.
    pub fn on_drag_over(&mut self, shelf: u64, pt: Point, acceptable: bool) -> bool {
        let Some(i) = self.idx(shelf) else { return false };
        let p = &self.panels[i];
        let inside = p.closing.is_none() && p.rect.offset(-p.win.x, -p.win.y).contains(pt);
        let on = acceptable && inside;
        if self.panels[i].drop_hover != on {
            self.panels[i].drop_hover = on;
            self.panels[i].dirty = true;
            self.schedule(true);
        }
        inside
    }

    pub fn on_drag_hover(&mut self, shelf: u64, on: bool) {
        let Some(i) = self.idx(shelf) else { return };
        if self.panels[i].drop_hover != on {
            self.panels[i].drop_hover = on;
            self.panels[i].dirty = true;
        }
    }

    pub fn on_drop(&mut self, shelf: u64, pb: &AnyObject) -> bool {
        let (items, promises) = pboard::read(pb);
        let n = items.len();
        let ids = self.shared.add(shelf, items);
        if promises {
            let dest = {
                let st = self.shared.lock();
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                st.set.dir_of(shelf).join(model::FILES_DIR).join(stamp.to_string())
            };
            let shared = self.shared.clone();
            pboard::receive_promises(pb, dest, move |p| {
                shared.add(shelf, vec![NewItem::File(p)]);
                tick();
            });
        }
        if self.cfg.haptics && (!ids.is_empty() || promises) {
            pboard::haptic();
        }
        if let Some(i) = self.idx(shelf) {
            self.panels[i].drop_hover = false;
            self.panels[i].dirty = true;
        }
        log::info!("drag shelf: dropped {n} item(s) on shelf {shelf}, added {}{}", ids.len(), if promises { " (+ file promises)" } else { "" });
        self.schedule(true);
        !ids.is_empty() || promises
    }

    pub fn on_drag_out_ended(&mut self, shelf: u64, op: usize) {
        self.watch.set_own_drag(false);
        self.press = None;
        if let Some((from, ids)) = self.own_drag.take() {
            if op != 0 && self.cfg.remove_after_drag_out && from == shelf {
                self.remove_items(from, &ids);
            }
        }
        self.schedule(true);
    }

    pub fn set_sharing(&mut self, on: bool) {
        self.sharing = on;
        if !on {
            self.picker = None;
        }
    }

    pub fn keep_picker(&mut self, p: Retained<AnyObject>) {
        self.picker = Some(p);
        self.sharing = true;
    }

    pub fn set_menu_open(&mut self, on: bool) {
        self.menu_open = on;
    }

    /// The left button went up somewhere (from the input hook).
    pub fn button_up(&mut self) {
        let c = pboard::change_count(&self.drag_pb);
        self.watch.button_up(c);
    }

    /// A shake during a drag (from the input hook): open a shelf if the
    /// drag carries something for it.
    pub fn shake(&mut self, at: Point) {
        if !self.cfg.enabled || !self.cfg.shake_to_open {
            return;
        }
        let button = NSEvent::pressedMouseButtons() & 1 != 0;
        let count = pboard::change_count(&self.drag_pb);
        let types = pboard::types_of(&self.drag_pb);
        let session = self.watch.session(button, count, &types);
        if !session.is_drag() {
            log::debug!("drag shelf: shake without a drag of files / links / text");
            return;
        }
        // Shaking over a shelf that is already there: drop on that one.
        if self.panels.iter().any(|p| p.closing.is_none() && p.rect.inflate(8.0, 8.0).contains(at)) {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else { return };
        match self.shared.new_shelf(Some(at), vec![]) {
            Some(id) => {
                log::info!("drag shelf: shake during a drag ({session:?}), opened shelf {id}");
                self.sync(mtm);
                if self.cfg.haptics {
                    pboard::haptic();
                }
            }
            None => log::info!("drag shelf: shake, but {} shelves are open already", self.cfg.max_shelves()),
        }
        self.schedule(true);
    }

    /// The hotkey: an empty shelf at the pointer.
    pub fn new_shelf_at_cursor(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let loc = NSEvent::mouseLocation();
        let cur = Point::new(loc.x, self.primary_h - loc.y);
        if self.shared.new_shelf(Some(cur), vec![]).is_some() {
            self.sync(mtm);
        }
        self.schedule(true);
    }

    // ----- diagnostics ------------------------------------------------

    fn debug(&mut self, mtm: MainThreadMarker, cmd: &str) {
        let mut parts = cmd.splitn(3, ':');
        let name = parts.next().unwrap_or("");
        let n: Option<usize> = parts.next().and_then(|s| s.parse().ok());
        let arg = parts.next().unwrap_or("");
        let i = n.filter(|n| *n < self.panels.len());
        match (name, i) {
            ("fresh", _) => self.no_appear = true,
            ("dump", Some(i)) => {
                let p = &self.panels[i];
                for (k, c) in p.cards.iter().enumerate() {
                    unsafe {
                        let dump = |name: &str, l: &AnyObject| {
                            let hidden: bool = msg_send![l, isHidden];
                            let f: NSRect = msg_send![l, frame];
                            let o: f32 = msg_send![l, opacity];
                            let sup: Option<Retained<AnyObject>> = msg_send![l, superlayer];
                            log::info!("card {k} {name}: hidden={hidden} opacity={o} frame=({:.0},{:.0},{:.0},{:.0}) super={}", f.origin.x, f.origin.y, f.size.width, f.size.height, sup.is_some());
                        };
                        dump("root", &c.root);
                        dump("img", &c.img);
                        dump("plate", &c.plate);
                    }
                }
            }
            ("dropHover", Some(i)) => {
                self.debug_drop = Some(self.panels[i].id);
                self.panels[i].drop_hover = true;
                self.panels[i].dirty = true;
            }
            ("noDropHover", _) => {
                self.debug_drop = None;
                for p in &mut self.panels {
                    p.drop_hover = false;
                    p.dirty = true;
                }
            }
            ("grid", Some(i)) | ("stack", Some(i)) => {
                let want = if name == "grid" { Mode::Grid } else { Mode::Stack };
                if self.panels[i].mode != want {
                    self.toggle_mode(i);
                }
            }
            ("hover", Some(i)) => {
                let h = match arg {
                    "close" => Hit::Close,
                    "toggle" => Hit::Toggle,
                    "menu" => Hit::Menu,
                    "none" => Hit::Outside,
                    k => Hit::Tile(k.parse().unwrap_or(0)),
                };
                self.debug_hover = Some((self.panels[i].id, h));
                self.panels[i].state_key.clear();
            }
            ("accent", _) => {
                // "accent:blue" (the default accent) or "accent:system".
                self.accent = if arg == "blue" || n.is_none() && cmd.ends_with("blue") { (0.04, 0.52, 1.0) } else { gfx::accent_rgb() };
                for p in &mut self.panels {
                    p.dirty = true;
                    p.state_key.clear();
                    for c in &mut p.cards {
                        c.key.clear();
                    }
                    for t in &mut p.tiles {
                        t.key.clear();
                        t.state = None;
                    }
                }
            }
            ("select", Some(i)) => {
                let k: usize = arg.parse().unwrap_or(0);
                let p = &mut self.panels[i];
                p.selected = p.items.iter().take(k).map(|d| d.id).collect();
                p.state_key.clear();
            }
            ("flash", Some(i)) => {
                self.panels[i].flash = Some((arg.to_string(), now() + 30.0));
                self.panels[i].dirty = true;
            }
            ("shake", _) => {
                // "shake:X:Y": a shelf as if shaken there (no drag check).
                let x = n.unwrap_or(400) as f64;
                let y: f64 = arg.parse().unwrap_or(400.0);
                if self.shared.new_shelf(Some(Point::new(x, y)), vec![]).is_some() {
                    self.sync(mtm);
                }
            }
            _ => log::info!("drag shelf: unknown debug command {cmd}"),
        }
        self.schedule(true);
    }

    /// Render each panel (over a wallpaper-like gradient) to
    /// `<dir>/drag-shelf-<n>.png`.
    fn render_pngs(&mut self, dir: &FsPath) {
        let _ = std::fs::create_dir_all(dir);
        for (n, p) in self.panels.iter().enumerate() {
            let (w, h, s) = (p.win.w, p.win.h, p.scale);
            let (pw, ph) = ((w * s) as usize, (h * s) as usize);
            unsafe {
                let cs = CGColorSpaceCreateDeviceRGB();
                let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, pw * 4, cs, BITMAP_RGBA);
                if ctx.is_null() {
                    CGColorSpaceRelease(cs);
                    continue;
                }
                CGContextScaleCTM(ctx, s, s);
                let comps = [0.33, 0.42, 0.62, 1.0, 0.72, 0.50, 0.60, 1.0];
                let locs = [0.0, 1.0];
                let g = CGGradientCreateWithColorComponents(cs, comps.as_ptr(), locs.as_ptr(), 2);
                CGContextDrawLinearGradient(ctx, g, CGPoint { x: 0.0, y: h }, CGPoint { x: w, y: 0.0 }, 0);
                CGGradientRelease(g);
                CGColorSpaceRelease(cs);
                let root: Layer = msg_send![&*p.view, layer];
                let _: () = msg_send![&*root, renderInContext: CgContext(ctx)];
                let img = Image::from_owned(CGBitmapContextCreateImage(ctx));
                CGContextRelease(ctx);
                if let Some(img) = img {
                    let path = dir.join(format!("drag-shelf-{n}.png"));
                    if gfx::write_png(&img, &path) {
                        log::info!("drag shelf: rendered {}", path.display());
                    }
                }
            }
        }
    }
}
