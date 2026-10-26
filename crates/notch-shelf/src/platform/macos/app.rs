//! Main-thread state: one panel per shelf screen, the frame loop, input
//! polling (pointer, drag pasteboard, power), item tiles and actions.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::sync::Arc;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSEvent, NSPanel, NSScreen, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::ffi::*;
use super::gfx::{self, Layer, Symbol, SymbolCache, TextStyle};
use super::pboard;
use super::view::ShelfView;
use crate::anim::{approach, Spring};
use crate::geometry::{self as geo, Button, Hit, Layout, Metrics, Point, Rect, ScreenGeom, ShapeParams};
use crate::logic::{ExpandLogic, Inputs, Mode};
use crate::store::{ItemKind, ShelfItem};
use crate::{Config, Request, Shared};

/// NSMainMenuWindowLevel (24) + 3: above the menu bar and status items.
const LEVEL: isize = 27;
const IDLE_INTERVAL: f64 = 1.0 / 20.0;
const FRAME_INTERVAL: f64 = 1.0 / 60.0;
/// Id of the pseudo item showing the clipboard.
pub const CLIP_ID: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
}

pub struct DragOut {
    /// (pasteboard writer, frame in view coordinates, drag image)
    pub items: Vec<(Retained<AnyObject>, Rect, Retained<AnyObject>)>,
}

pub enum Deferred {
    Share { items: Vec<Retained<AnyObject>>, anchor: Rect },
}

pub struct MenuReq {
    /// (title, tag (<0 = separator), SF symbol, enabled)
    pub entries: Vec<(String, isize, &'static str, bool)>,
}

const M_OPEN: isize = 1;
const M_REVEAL: isize = 2;
const M_COPY: isize = 3;
const M_SHARE: isize = 4;
const M_REMOVE: isize = 5;
const M_CLEAR: isize = 6;
const M_KEEP: isize = 7;

struct Press {
    panel: usize,
    start: Point,
    hit: Hit,
    mods: Mods,
    dragged: bool,
}

#[derive(Default)]
struct Thumb {
    icon: Option<gfx::Image>,
    ql: Option<gfx::Image>,
    requested: bool,
}

#[derive(Clone, Copy, PartialEq, Default)]
struct TileState {
    selected: bool,
    hover: bool,
    close: bool,
}

struct Tile {
    id: u64,
    root: Layer,
    plate: Layer,
    thumb: Layer,
    glyph: Layer,
    label: Layer,
    close: Layer,
    close_icon: Layer,
    state: Option<TileState>,
    content_key: String,
}

struct HeaderButton {
    kind: Button,
    bg: Layer,
    icon: Layer,
}

struct Panel {
    geom: ScreenGeom,
    scale: f64,
    collapsed: Rect,
    expanded: Rect,
    peek: Rect,
    win: Rect,
    window: Retained<NSPanel>,
    view: Retained<ShelfView>,
    _container: Layer,
    shape: Layer,
    rim: Layer,
    content: Layer,
    content_mask: Layer,
    inner: Layer,
    title_icon: Layer,
    title_text: Layer,
    count_bg: Layer,
    count_text: Layer,
    buttons: Vec<HeaderButton>,
    strip: Layer,
    strip_fade: Layer,
    empty: Layer,
    empty_border: Layer,
    empty_icon: Layer,
    empty_text: Layer,
    drop_ring: Layer,
    peek_layer: Layer,
    peek_icon: Layer,
    peek_text: Layer,
    tiles: Vec<Tile>,
    logic: ExpandLogic,
    mode: Mode,
    spring: Spring,
    from: ShapeParams,
    to: ShapeParams,
    shape_now: ShapeParams,
    content_alpha: f64,
    peek_alpha: f64,
    scroll: f64,
    hover: Hit,
    drop_hover: bool,
    hold: bool,
    layout: Layout,
    ignoring: bool,
    header_key: String,
    button_state: Vec<(bool, bool)>,
    drawn: Option<(ShapeParams, f64, f64)>,
}

pub struct App {
    shared: Arc<Shared>,
    cfg: Config,
    cfg_gen: u64,
    items_gen: u64,
    metrics: Metrics,
    items: Vec<ShelfItem>,
    clip: Option<ShelfItem>,
    clip_count: isize,
    clip_hidden_for: isize,
    display: Vec<ShelfItem>,
    panels: Vec<Panel>,
    screens: Vec<(ScreenGeom, f64)>,
    primary_h: f64,
    timer: CFRunLoopTimerRef,
    link: Option<Retained<AnyObject>>,
    link_running: bool,
    last_link_tick: f64,
    last_tick: f64,
    last_screen_poll: f64,
    last_power_poll: f64,
    power: Option<(bool, u8)>,
    drag_pb: Retained<AnyObject>,
    drag_count: isize,
    drag_active: bool,
    button_down: bool,
    selected: BTreeSet<u64>,
    anchor: Option<u64>,
    press: Option<Press>,
    own_drag: Option<Vec<u64>>,
    menu_open: bool,
    menu_ids: Vec<u64>,
    menu_anchor: Rect,
    sharing: bool,
    picker: Option<Retained<AnyObject>>,
    thumbs: HashMap<u64, Thumb>,
    symbols: SymbolCache,
    accent: (f64, f64, f64),
    debug_drop_hover: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` with the app state, unless it's missing or already borrowed.
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

pub fn view_to_global(panel: usize, local: Point) -> Point {
    with(|a| a.panels.get(panel).map(|p| Point::new(p.win.x + local.x, p.win.y + local.y))).flatten().unwrap_or(local)
}

pub fn tick(from_link: bool) {
    autoreleasepool(|_| {
        with(|a| a.tick(from_link));
    });
}

extern "C" fn timer_cb(_t: CFRunLoopTimerRef, _info: *mut c_void) {
    tick(false);
}

pub fn install(shared: Arc<Shared>) {
    let Some(_mtm) = MainThreadMarker::new() else { return };
    uninstall();
    let drag_pb = pboard::drag_pasteboard();
    let drag_count = pboard::change_count(&drag_pb);
    let mut app = App {
        shared,
        cfg: Config { enabled: false, ..Config::default() },
        cfg_gen: 0,
        items_gen: 0,
        metrics: Metrics::default(),
        items: vec![],
        clip: None,
        clip_count: -1,
        clip_hidden_for: -1,
        display: vec![],
        panels: vec![],
        screens: vec![],
        primary_h: 0.0,
        timer: std::ptr::null(),
        link: None,
        link_running: false,
        last_link_tick: 0.0,
        last_tick: now(),
        last_screen_poll: 0.0,
        last_power_poll: 0.0,
        power: None,
        drag_pb,
        drag_count,
        drag_active: false,
        button_down: false,
        selected: BTreeSet::new(),
        anchor: None,
        press: None,
        own_drag: None,
        menu_open: false,
        menu_ids: vec![],
        menu_anchor: Rect::default(),
        sharing: false,
        picker: None,
        thumbs: HashMap::new(),
        symbols: SymbolCache::default(),
        accent: gfx::accent_rgb(),
        debug_drop_hover: false,
    };
    unsafe {
        let t = CFRunLoopTimerCreate(std::ptr::null(), now() + 0.01, FRAME_INTERVAL, 0, 0, timer_cb, std::ptr::null_mut());
        CFRunLoopAddTimer(CFRunLoopGetMain(), t, kCFRunLoopCommonModes);
        app.timer = t;
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
}

pub fn uninstall() {
    let old = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
    if let Some(mut app) = old {
        app.teardown();
    }
}

fn thumb_ready(id: u64, img: gfx::Image, icon_like: bool) {
    with(|a| {
        // Real previews (pictures, documents) get rounded corners and a
        // hairline edge; icons are used as they are.
        let img = if icon_like {
            img
        } else {
            let scale = a.panels.first().map(|p| p.scale).unwrap_or(2.0);
            gfx::rounded_fit(&img, a.metrics.thumb, 7.0, scale).unwrap_or(img)
        };
        a.thumbs.entry(id).or_default().ql = Some(img);
        let display = a.display.clone();
        for i in 0..a.panels.len() {
            a.sync_tiles(i, &display, false);
        }
    });
}

/// Screens as (geometry in global top-left points, backing scale), plus the
/// primary screen height.
fn read_screens(mtm: MainThreadMarker) -> (Vec<(ScreenGeom, f64)>, f64) {
    let screens = NSScreen::screens(mtm);
    let ph = screens.iter().next().map(|s| s.frame().size.height).unwrap_or(0.0);
    let v = screens
        .iter()
        .map(|s| {
            let f = s.frame();
            let top = ph - (f.origin.y + f.size.height);
            let frame = Rect::new(f.origin.x, top, f.size.width, f.size.height);
            let insets = s.safeAreaInsets();
            let mut notch = None;
            if insets.top > 0.0 {
                let l = s.auxiliaryTopLeftArea();
                let r = s.auxiliaryTopRightArea();
                let x0 = l.origin.x + l.size.width;
                let x1 = r.origin.x;
                if x1 > x0 && l.size.width > 0.0 {
                    notch = Some(Rect::new(x0, top, x1 - x0, insets.top));
                }
            }
            let vis = s.visibleFrame();
            let menu_bar_h = ((f.origin.y + f.size.height) - (vis.origin.y + vis.size.height)).max(0.0);
            (ScreenGeom { frame, notch, menu_bar_h }, s.backingScaleFactor())
        })
        .collect();
    (v, ph)
}

fn white(a: f64) -> Color {
    Color::white(a)
}

impl App {
    fn accent(&self, a: f64) -> Color {
        let (r, g, b) = self.accent;
        Color::rgba(r, g, b, a)
    }

    fn teardown(&mut self) {
        if !self.timer.is_null() {
            unsafe {
                CFRunLoopTimerInvalidate(self.timer);
                CFRelease(self.timer);
            }
            self.timer = std::ptr::null();
        }
        self.close_panels();
    }

    fn close_panels(&mut self) {
        if let Some(link) = self.link.take() {
            unsafe {
                let _: () = msg_send![&*link, invalidate];
            }
        }
        self.link_running = false;
        for p in self.panels.drain(..) {
            p.window.orderOut(None);
            p.window.close();
        }
    }

    fn sym(&mut self, name: &str, size: f64, weight: f64, rgba: (f64, f64, f64, f64), scale: f64) -> Option<Symbol> {
        self.symbols.get(name, size, weight, rgba, scale)
    }

    // ----- frame loop -------------------------------------------------

    fn tick(&mut self, from_link: bool) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let t = now();
        if from_link {
            self.last_link_tick = t;
        } else if self.link_running && t - self.last_link_tick < 0.1 {
            // The display link is driving frames.
            return;
        }
        let dt = (t - self.last_tick).clamp(0.0, 0.05);
        self.last_tick = t;

        // Config / items / requests from the Handle.
        let (cfg, cfg_gen, items, items_gen, requests, stopped) = {
            let mut st = self.shared.lock();
            let cfg = (st.config_gen != self.cfg_gen).then(|| st.config.clone());
            let items = (st.items_gen != self.items_gen).then(|| st.store.items().to_vec());
            (cfg, st.config_gen, items, st.items_gen, std::mem::take(&mut st.requests), st.stopped)
        };
        if stopped {
            self.close_panels();
            return;
        }
        let mut rebuild = false;
        if let Some(cfg) = cfg {
            let old = std::mem::replace(&mut self.cfg, cfg);
            self.cfg_gen = cfg_gen;
            self.metrics = self.cfg.metrics();
            rebuild |= old.enabled != self.cfg.enabled
                || old.screens != self.cfg.screens
                || old.virtual_notch_on_non_notch_screens != self.cfg.virtual_notch_on_non_notch_screens
                || old.panel_width != self.cfg.panel_width
                || self.panels.is_empty() && self.cfg.enabled;
            if !self.cfg.show_clipboard {
                self.clip = None;
            }
        }
        if let Some(items) = items {
            self.items = items;
            self.items_gen = items_gen;
            self.refresh_display();
        }
        if t - self.last_screen_poll > 1.0 || rebuild {
            self.last_screen_poll = t;
            let (s, ph) = read_screens(mtm);
            if s != self.screens || ph != self.primary_h {
                self.screens = s;
                self.primary_h = ph;
                rebuild = true;
            }
        }
        if rebuild {
            self.build_panels(mtm);
        }
        if self.panels.is_empty() {
            self.schedule(false);
            return;
        }

        // Pointer, buttons, drag pasteboard.
        let loc = NSEvent::mouseLocation();
        let cur = Point::new(loc.x, self.primary_h - loc.y);
        let btn = NSEvent::pressedMouseButtons() & 1 != 0;
        let pressed = btn && !self.button_down;
        self.button_down = btn;
        let count = pboard::change_count(&self.drag_pb);
        if count != self.drag_count {
            self.drag_count = count;
            if btn && self.own_drag.is_none() {
                self.drag_active = pboard::accepts(&pboard::types_of(&self.drag_pb));
            }
        }
        if !btn {
            self.drag_active = false;
        }

        // Battery peeks.
        if self.cfg.battery_indicator && t - self.last_power_poll > 1.0 {
            self.last_power_poll = t;
            let ps = power_state();
            if let (Some((was_ac, _)), Some((ac, _))) = (self.power, ps) {
                if was_ac != ac {
                    for p in &mut self.panels {
                        p.logic.peek(t, 2.6);
                    }
                }
            }
            self.power = ps;
        }

        for r in requests {
            match r {
                Request::Expand => {
                    for p in &mut self.panels {
                        p.logic.expand();
                        p.hold = true;
                    }
                }
                Request::Collapse => {
                    for p in &mut self.panels {
                        p.hold = false;
                        p.logic.collapse(false);
                    }
                }
                Request::RenderPng(dir) => {
                    self.advance(mtm, cur, pressed, dt, t);
                    self.render_pngs(&dir);
                }
                Request::Debug(cmd) => self.debug(&cmd, t),
            }
        }

        let animating = self.advance(mtm, cur, pressed, dt, t);
        let expanded = self.panels.iter().any(|p| p.mode != Mode::Collapsed);
        self.schedule(animating || expanded);
    }

    fn debug(&mut self, cmd: &str, t: f64) {
        match cmd {
            "dropHover" => self.debug_drop_hover = true,
            "noDropHover" => self.debug_drop_hover = false,
            c if c.starts_with("select:") => {
                let n: usize = c[7..].parse().unwrap_or(0);
                self.selected = self.display.iter().take(n).map(|d| d.id).collect();
                for i in 0..self.panels.len() {
                    self.update_states(i);
                }
            }
            "peek" => {
                self.power = power_state().or(Some((true, 80)));
                for p in &mut self.panels {
                    p.logic.peek(t, 2.6);
                }
            }
            _ => {}
        }
    }

    /// Run logic + animation + drawing for every panel. Returns whether any
    /// panel is still animating.
    fn advance(&mut self, _mtm: MainThreadMarker, cur: Point, pressed: bool, dt: f64, t: f64) -> bool {
        let settings = self.cfg.logic_settings();
        let mut animating = false;
        for i in 0..self.panels.len() {
            let (new_mode, old_mode) = {
                let drag_active = self.drag_active;
                let pinned_global = self.menu_open || self.sharing;
                let p = &mut self.panels[i];
                let in_hover_zone = geo::hover_zone(&p.collapsed).contains(cur);
                let in_keep_zone = geo::keep_open_zone(&p.expanded, &self.metrics).contains(cur);
                let in_drag_zone = geo::drag_zone(&p.expanded, &p.geom.frame).contains(cur);
                let outside_click = pressed && !in_keep_zone && p.mode == Mode::Expanded;
                if outside_click || in_keep_zone {
                    p.hold = false;
                }
                let inputs = Inputs {
                    now: t,
                    in_hover_zone,
                    in_keep_zone,
                    in_drag_zone,
                    button_down: self.button_down,
                    external_drag: drag_active,
                    drag_over_panel: p.drop_hover,
                    clicked_notch: pressed && in_hover_zone && p.mode != Mode::Expanded,
                    mouse_down_outside: outside_click,
                    pinned: pinned_global || p.hold || self.debug_drop_hover,
                };
                let old = p.mode;
                (p.logic.update(&settings, &inputs), old)
            };
            if new_mode != old_mode {
                self.mode_changed(i, new_mode);
            }
            animating |= self.animate(i, dt, cur);
        }
        animating
    }

    fn schedule(&mut self, fast: bool) {
        if let Some(link) = &self.link {
            if fast != self.link_running {
                unsafe {
                    let _: () = msg_send![&**link, setPaused: !fast];
                }
                self.link_running = fast;
            }
        }
        // The timer also covers for a display link that doesn't fire (it
        // pauses while the window is considered occluded).
        let link_alive = self.link_running && now() - self.last_link_tick < 0.1;
        let next = if fast && !link_alive { FRAME_INTERVAL } else { IDLE_INTERVAL };
        if !self.timer.is_null() {
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, now() + next) };
        }
    }

    // ----- panels -----------------------------------------------------

    fn chosen_screens(&self) -> Vec<(ScreenGeom, f64, Rect)> {
        if !self.cfg.enabled {
            return vec![];
        }
        let geoms: Vec<ScreenGeom> = self.screens.iter().map(|(g, _)| *g).collect();
        crate::platform::choose_screens(self.cfg.screens, self.cfg.virtual_notch_on_non_notch_screens, &geoms, &self.metrics)
            .into_iter()
            .map(|(i, c)| (self.screens[i].0, self.screens[i].1, c))
            .collect()
    }

    fn build_panels(&mut self, mtm: MainThreadMarker) {
        self.close_panels();
        let chosen = self.chosen_screens();
        for (idx, (geom, scale, collapsed)) in chosen.into_iter().enumerate() {
            let p = self.make_panel(mtm, idx, geom, scale, collapsed);
            self.panels.push(p);
        }
        // Display-synced frames (macOS 14+); the timer is the fallback.
        if let Some(p) = self.panels.first() {
            let v: &AnyObject = &p.view;
            let ok: bool = unsafe { msg_send![v, respondsToSelector: sel!(displayLinkWithTarget:selector:)] };
            if ok {
                unsafe {
                    let link: Option<Retained<AnyObject>> = msg_send![v, displayLinkWithTarget: v, selector: sel!(displayTick:)];
                    if let Some(link) = link {
                        let rl: Retained<AnyObject> = msg_send![class!(NSRunLoop), mainRunLoop];
                        let _: () = msg_send![&*link, addToRunLoop: &*rl, forMode: &*ns("kCFRunLoopCommonModes")];
                        let _: () = msg_send![&*link, setPaused: true];
                        self.link = Some(link);
                    }
                }
            }
        }
        let display = self.display.clone();
        for i in 0..self.panels.len() {
            self.sync_tiles(i, &display, false);
            self.draw_shape(i);
        }
        log::info!("notch-shelf: {} panel(s)", self.panels.len());
    }

    fn make_panel(&mut self, mtm: MainThreadMarker, idx: usize, geom: ScreenGeom, scale: f64, collapsed: Rect) -> Panel {
        let m = self.metrics;
        let expanded = geo::expanded_rect(&collapsed, &geom.frame, &m);
        let peek = geo::peek_rect(&collapsed, &m);
        let win = geo::window_rect(&expanded, &geom.frame, &m);
        let cocoa = NSRect::new(NSPoint::new(win.x, self.primary_h - (win.y + win.h)), NSSize::new(win.w, win.h));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            cocoa,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        // setFloatingPanel resets the level, so it goes first.
        window.setFloatingPanel(true);
        window.setLevel(LEVEL);
        window.setHidesOnDeactivate(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setIgnoresMouseEvents(true);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        unsafe {
            let _: () = msg_send![&*window, setAnimationBehavior: 2isize]; // NSWindowAnimationBehaviorNone
            let _: () = msg_send![&*window, setTitle: &*ns("Notch Shelf")];
        }
        let view = ShelfView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(win.w, win.h)), idx);
        // Layer-hosting view: we own the (unflipped) root layer, so the
        // flipped container gives top-left coordinates on screen and in
        // offscreen renders alike.
        let bounds = Rect::new(0.0, 0.0, win.w, win.h);
        let root = gfx::layer();
        gfx::set_frame(&root, bounds);
        unsafe {
            let _: () = msg_send![&*view, setLayer: &*root];
        }
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        let rf: bool = unsafe { msg_send![&*root, isGeometryFlipped] };
        log::debug!("notch-shelf: root layer geometryFlipped={rf}");
        let container = gfx::layer();
        gfx::set_frame(&container, bounds);
                gfx::add(&root, &container);

        let shape = gfx::shape_layer();
        gfx::set_frame(&shape, bounds);
        gfx::set_fill(&shape, Some(&Color::black(1.0)));
        gfx::set_shadow(&shape, 0.0, 16.0, -7.0);
        gfx::add(&container, &shape);

        let content = gfx::layer();
        gfx::set_frame(&content, bounds);
        let content_mask = gfx::shape_layer();
        gfx::set_frame(&content_mask, bounds);
        gfx::set_fill(&content_mask, Some(&Color::black(1.0)));
        gfx::set_mask(&content, &content_mask);
        gfx::set_opacity(&content, 0.0);
        gfx::add(&container, &content);

        let inner = gfx::layer();
        gfx::set_anchor(&inner, 0.5, 1.0);
        gfx::set_frame(&inner, bounds);
        gfx::add(&content, &inner);

        let rim = gfx::shape_layer();
        gfx::set_frame(&rim, bounds);
        gfx::set_fill(&rim, None);
        gfx::set_stroke(&rim, Some(&white(0.0)), 1.0);
        gfx::add(&container, &rim);

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
        let title_icon = mk(&inner);
        let title_text = mkt(&inner);
        let count_bg = mk(&inner);
        let count_text = mkt(&count_bg);
        let mut buttons = vec![];
        for b in geo::BUTTONS {
            let bg = mk(&inner);
            gfx::set_corner(&bg, 8.0);
            let icon = mk(&bg);
            buttons.push(HeaderButton { kind: b, bg, icon });
        }
        let strip = mk(&inner);
        gfx::set_masks_to_bounds(&strip, true);
        let strip_fade = gfx::gradient_layer();
        gfx::set_mask(&strip, &strip_fade);
        let empty = mk(&inner);
        let empty_border = gfx::shape_layer();
        gfx::add(&empty, &empty_border);
        gfx::set_dash(&empty_border, &[5.0, 4.0]);
        let empty_icon = mk(&empty);
        let empty_text = mkt(&empty);
        let drop_ring = gfx::shape_layer();
        gfx::add(&inner, &drop_ring);
        gfx::set_hidden(&drop_ring, true);

        let peek_layer = mk(&container);
        gfx::set_frame(&peek_layer, bounds);
        gfx::set_opacity(&peek_layer, 0.0);
        let peek_icon = mk(&peek_layer);
        let peek_text = mkt(&peek_layer);

        let cs = geo::collapsed_shape(&collapsed, &m);
        let mut p = Panel {
            geom,
            scale,
            collapsed,
            expanded,
            peek,
            win,
            window,
            view,
            _container: container,
            shape,
            rim,
            content,
            content_mask,
            inner,
            title_icon,
            title_text,
            count_bg,
            count_text,
            buttons,
            strip,
            strip_fade,
            empty,
            empty_border,
            empty_icon,
            empty_text,
            drop_ring,
            peek_layer,
            peek_icon,
            peek_text,
            tiles: vec![],
            logic: ExpandLogic::default(),
            mode: Mode::Collapsed,
            spring: Spring::new(400.0, 40.0, 1.0),
            from: cs,
            to: cs,
            shape_now: cs,
            content_alpha: 0.0,
            peek_alpha: 0.0,
            scroll: 0.0,
            hover: Hit::Outside,
            drop_hover: false,
            hold: false,
            layout: geo::layout(&expanded, &collapsed, 0, 0.0, &m),
            ignoring: true,
            header_key: String::new(),
            button_state: vec![],
            drawn: None,
        };
        self.layout_static(&mut p);
        // Re-assert the frame: AppKit may have constrained it below the
        // menu bar while the level was still low.
        p.window.setFrame_display(cocoa, false);
        p.window.orderFrontRegardless();
        p
    }

    /// Window-local rect.
    fn local(p: &Panel, r: Rect) -> Rect {
        r.offset(-p.win.x, -p.win.y)
    }

    /// Frames that only depend on geometry (header, strip, empty state).
    fn layout_static(&mut self, p: &mut Panel) {
        let m = self.metrics;
        let l = &p.layout;
        transaction(None, || {
            gfx::set_frame(&p.strip, Self::local(p, l.strip));
            gfx::set_frame(&p.empty, Self::local(p, l.strip));
            let er = Rect::new(0.0, 0.0, l.strip.w, l.strip.h).inflate(-2.0, -2.0);
            let path = Path::rounded_rect(er, 16.0);
            gfx::set_frame(&p.empty_border, Rect::new(0.0, 0.0, l.strip.w, l.strip.h));
            gfx::set_path(&p.empty_border, &path);
            let ring = Path::rounded_rect(Rect::new(0.0, 0.0, l.strip.w, l.strip.h).inflate(-1.0, -1.0), 16.0);
            gfx::set_frame(&p.drop_ring, Self::local(p, l.strip));
            gfx::set_path(&p.drop_ring, &ring);
            for (b, r) in &l.buttons {
                if let Some(hb) = p.buttons.iter().find(|x| x.kind == *b) {
                    gfx::set_frame(&hb.bg, Self::local(p, *r));
                }
            }
        });
        let _ = m;
    }

    fn mode_changed(&mut self, i: usize, mode: Mode) {
        let m = self.metrics;
        if mode == Mode::Expanded {
            // Fresh state on open: stale files, clipboard.
            if self.shared.edit(|s| s.prune_missing()) {
                self.items = self.shared.lock().store.items().to_vec();
            }
            if self.cfg.show_clipboard {
                self.read_clipboard();
            }
            self.refresh_display();
        }
        let p = &mut self.panels[i];
        p.mode = mode;
        let (to, k, zeta) = match mode {
            Mode::Expanded => (geo::expanded_shape(&p.expanded, &m), 340.0, 0.70),
            Mode::Collapsed => (geo::collapsed_shape(&p.collapsed, &m), 420.0, 1.0),
            Mode::Peek => (geo::peek_shape(&p.peek, &m), 380.0, 0.68),
        };
        p.from = p.shape_now;
        p.to = to;
        p.spring = Spring::new(k, Spring::damping_for_ratio(k, zeta), 0.0);
        p.spring.target = 1.0;
        let ignore = mode != Mode::Expanded;
        if ignore != p.ignoring {
            p.window.setIgnoresMouseEvents(ignore);
            p.ignoring = ignore;
        }
        if mode != Mode::Expanded {
            p.drop_hover = false;
            p.hover = Hit::Outside;
            self.selected.clear();
            self.press = None;
        }
        if mode == Mode::Peek {
            self.draw_peek(i);
        }
        if mode == Mode::Expanded {
            let display = self.display.clone();
            self.sync_tiles(i, &display, false);
        }
    }

    /// Step the spring and fades; draw. Returns true while animating.
    fn animate(&mut self, i: usize, dt: f64, cur: Point) -> bool {
        let p = &mut self.panels[i];
        p.spring.step(dt);
        p.shape_now = p.from.lerp(&p.to, p.spring.value);
        // Content fades in once the panel has mostly grown, and out quickly.
        let content_target = if p.mode == Mode::Expanded && p.spring.value > 0.45 { 1.0 } else { 0.0 };
        let tau = if content_target > p.content_alpha { 0.075 } else { 0.03 };
        p.content_alpha = approach(p.content_alpha, content_target, dt, tau);
        let peek_target = if p.mode == Mode::Peek && p.spring.value > 0.5 { 1.0 } else { 0.0 };
        p.peek_alpha = approach(p.peek_alpha, peek_target, dt, 0.06);
        let animating = !p.spring.settled() || p.content_alpha != content_target || p.peek_alpha != peek_target;
        // Hover tracking inside the open panel.
        let hover = if p.mode == Mode::Expanded {
            let close_for = match p.hover {
                Hit::Tile(k) | Hit::Close(k) => Some(k),
                _ => None,
            };
            geo::hit_test(&p.layout, cur, close_for, &self.metrics)
        } else {
            Hit::Outside
        };
        let hover_changed = hover != p.hover;
        p.hover = hover;
        if self.debug_drop_hover {
            p.drop_hover = true;
        }
        let key = (p.shape_now, p.content_alpha, p.peek_alpha);
        if p.drawn != Some(key) {
            p.drawn = Some(key);
            self.draw_shape(i);
        }
        if hover_changed || animating || self.debug_drop_hover {
            self.update_states(i);
        }
        animating
    }

    fn draw_shape(&mut self, i: usize) {
        let m = self.metrics;
        let p = &self.panels[i];
        let o = Point::new(p.win.x, p.win.y);
        let path = Path::from_els(&geo::notch_path(&p.shape_now, o), p.win.h);
        let span = (p.expanded.h - p.collapsed.h).max(1.0);
        let openness = ((p.shape_now.rect.h - p.collapsed.h) / span).clamp(0.0, 1.0);
        let widen = ((p.shape_now.rect.w - p.collapsed.w) / (2.0 * m.peek_wing)).clamp(0.0, 1.0);
        let s = 0.92 + 0.08 * p.content_alpha;
        transaction(None, || {
            gfx::set_path(&p.shape, &path);
            gfx::set_shadow_path(&p.shape, &path);
            gfx::set_shadow_opacity(&p.shape, 0.55 * openness + 0.25 * widen * (1.0 - openness));
            gfx::set_path(&p.content_mask, &path);
            gfx::set_path(&p.rim, &path);
            gfx::set_stroke(&p.rim, Some(&white(0.10 * openness)), 1.0);
            gfx::set_opacity(&p.content, p.content_alpha);
            gfx::set_transform(&p.inner, gfx::AffineTransform::scale(s));
            gfx::set_opacity(&p.peek_layer, p.peek_alpha);
        });
    }

    fn draw_peek(&mut self, i: usize) {
        let (ac, pct) = self.power.unwrap_or((true, 100));
        let scale = self.panels[i].scale;
        let bucket = match pct {
            0..=12 => "0",
            13..=37 => "25",
            38..=62 => "50",
            63..=87 => "75",
            _ => "100",
        };
        let (name, rgba) = if ac {
            ("battery.100percent.bolt".to_string(), (0.20, 0.84, 0.29, 1.0))
        } else if pct <= 20 {
            (format!("battery.{bucket}percent"), (1.0, 0.27, 0.23, 1.0))
        } else {
            (format!("battery.{bucket}percent"), (1.0, 1.0, 1.0, 0.95))
        };
        let sym = self.sym(&name, 15.0, gfx::W_REGULAR, rgba, scale).or_else(|| self.sym("battery.100percent", 15.0, gfx::W_REGULAR, rgba, scale));
        let p = &self.panels[i];
        let m = self.metrics;
        let left = Rect::new(p.peek.x + m.shoulder, p.peek.y, m.peek_wing - m.shoulder, p.peek.h);
        let right = Rect::new(p.collapsed.right(), p.peek.y, m.peek_wing - m.shoulder, p.peek.h);
        let st = TextStyle { size: 13.0, weight: gfx::W_SEMIBOLD, align: "center", truncate: "end" };
        let color = if ac { Color::rgba(0.20, 0.84, 0.29, 1.0) } else { white(0.95) };
        transaction(None, || {
            gfx::place_symbol(&p.peek_icon, sym.as_ref(), Self::local(p, left));
            let tr = Self::local(p, right);
            gfx::set_frame(&p.peek_text, Rect::new(tr.x, tr.center_y() - 8.5, tr.w, 17.0));
            gfx::set_text(&p.peek_text, &format!("{pct}%"), &st, &color);
        });
    }

    // ----- tiles ------------------------------------------------------

    fn refresh_display(&mut self) {
        let mut d = Vec::with_capacity(self.items.len() + 1);
        if let Some(c) = &self.clip {
            d.push(c.clone());
        }
        d.extend(self.items.iter().cloned());
        let ids: BTreeSet<u64> = d.iter().map(|i| i.id).collect();
        self.selected.retain(|id| ids.contains(id));
        self.thumbs.retain(|id, _| ids.contains(id));
        self.display = d;
        let display = self.display.clone();
        for i in 0..self.panels.len() {
            let animate = self.panels[i].mode == Mode::Expanded;
            self.sync_tiles(i, &display, animate);
        }
    }

    fn read_clipboard(&mut self) {
        let (count, kind) = pboard::clipboard_item();
        if count == self.clip_count {
            return;
        }
        self.clip_count = count;
        self.clip = if count == self.clip_hidden_for { None } else { kind.map(|kind| ShelfItem { id: CLIP_ID, kind, added_at: 0 }) };
    }

    fn ensure_thumb(&mut self, item: &ShelfItem, scale: f64) {
        let size = self.metrics.thumb;
        let Some(path) = item.path() else { return };
        let th = self.thumbs.entry(item.id).or_default();
        if th.icon.is_none() {
            let icon = pboard::file_icon(path);
            th.icon = gfx::rasterize(&icon, size, size, scale, None);
        }
        if !th.requested {
            th.requested = true;
            let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            pboard::request_thumbnail(item.id, &real, size, scale, thumb_ready);
        }
    }

    /// Create / remove / reorder tile layers to match `display`, then lay
    /// them out (animated when `animate`).
    fn sync_tiles(&mut self, i: usize, display: &[ShelfItem], animate: bool) {
        let m = self.metrics;
        let scale = self.panels[i].scale;
        for it in display {
            self.ensure_thumb(it, scale);
        }
        let mut old: HashMap<u64, Tile> = self.panels[i].tiles.drain(..).map(|t| (t.id, t)).collect();
        let mut tiles = Vec::with_capacity(display.len());
        let mut fresh = vec![];
        for it in display {
            let t = match old.remove(&it.id) {
                Some(t) => t,
                None => {
                    fresh.push(it.id);
                    self.new_tile(i, it.id)
                }
            };
            tiles.push(t);
        }
        for (_, t) in old {
            gfx::remove(&t.root);
        }
        // Content of each tile (only when it changed).
        for (t, it) in tiles.iter_mut().zip(display) {
            self.fill_tile(t, it, scale);
        }
        let p = &mut self.panels[i];
        p.tiles = tiles;
        p.layout = geo::layout(&p.expanded, &p.collapsed, display.len(), p.scroll, &m);
        p.scroll = p.layout.scroll;
        self.place_tiles(i, animate, &fresh);
        self.update_header(i);
        self.update_states(i);
    }

    fn new_tile(&self, i: usize, id: u64) -> Tile {
        let p = &self.panels[i];
        let mk = |parent: &AnyObject| {
            let l = gfx::layer();
            gfx::set_scale(&l, p.scale);
            gfx::add(parent, &l);
            l
        };
        let root = mk(&p.strip);
        gfx::set_frame(&root, Rect::new(0.0, 0.0, self.metrics.tile_w, self.metrics.tile_h));
        gfx::set_corner(&root, 14.0);
        let plate = mk(&root);
        gfx::set_corner(&plate, 12.0);
        let thumb = mk(&root);
        gfx::set_shadow(&thumb, 0.35, 3.0, -1.5);
        let glyph = mk(&plate);
        let label = gfx::text_layer(p.scale);
        gfx::add(&root, &label);
        let close = mk(&root);
        gfx::set_corner(&close, 9.0);
        gfx::set_bg(&close, Some(&Color::rgba(0.30, 0.30, 0.32, 1.0)));
        gfx::set_border(&close, 0.5, Some(&white(0.25)));
        gfx::set_shadow(&close, 0.4, 2.0, -1.0);
        let close_icon = mk(&close);
        gfx::set_opacity(&close, 0.0);
        Tile { id, root, plate, thumb, glyph, label, close, close_icon, state: None, content_key: String::new() }
    }

    fn fill_tile(&mut self, t: &mut Tile, it: &ShelfItem, scale: f64) {
        let m = self.metrics;
        let has_ql = self.thumbs.get(&it.id).is_some_and(|th| th.ql.is_some());
        let key = format!("{:?}|{}", it.kind, has_ql);
        if t.content_key == key {
            return;
        }
        t.content_key = key;
        let label = if it.id == CLIP_ID { "Clipboard".to_string() } else { it.display_name() };
        let tile = Rect::new(0.0, 0.0, m.tile_w, m.tile_h);
        let parts = geo::tile_parts(&tile, &m);
        let plate_r = Rect::new(parts.thumb.center_x() - 23.0, parts.thumb.center_y() - 23.0, 46.0, 46.0);
        let st = TextStyle { size: 11.0, weight: gfx::W_MEDIUM, align: "center", truncate: if it.path().is_some() { "middle" } else { "end" } };
        let (glyph_name, plate_color): (&str, Option<Color>) = match &it.kind {
            ItemKind::File { .. } => ("", None),
            ItemKind::Url { .. } => ("link", Some(self.accent(0.95))),
            ItemKind::Text { .. } => ("text.alignleft", Some(white(0.16))),
        };
        let glyph = if glyph_name.is_empty() { None } else { self.sym(glyph_name, 20.0, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.95), scale) };
        let xmark = self.sym("xmark", 8.5, gfx::W_BOLD, (1.0, 1.0, 1.0, 0.9), scale);
        let image = self.thumbs.get(&it.id).and_then(|th| th.ql.clone().or_else(|| th.icon.clone()));
        let clip_badge = if it.id == CLIP_ID { self.sym("doc.on.clipboard", 11.0, gfx::W_SEMIBOLD, (1.0, 1.0, 1.0, 0.9), scale) } else { None };
        transaction(None, || {
            gfx::set_frame(&t.thumb, parts.thumb);
            gfx::set_frame(&t.label, parts.label);
            gfx::set_text(&t.label, &label, &st, &white(0.88));
            gfx::set_frame(&t.close, parts.close);
            gfx::place_symbol(&t.close_icon, xmark.as_ref(), Rect::new(0.0, 0.0, parts.close.w, parts.close.h));
            match plate_color {
                Some(c) => {
                    gfx::set_frame(&t.plate, plate_r);
                    gfx::set_bg(&t.plate, Some(&c));
                    gfx::set_hidden(&t.plate, false);
                    gfx::place_symbol(&t.glyph, glyph.as_ref(), Rect::new(0.0, 0.0, plate_r.w, plate_r.h));
                    gfx::set_image(&t.thumb, None);
                    if let Some(b) = &clip_badge {
                        gfx::set_frame(&t.thumb, Rect::new(plate_r.right() - b.w / 2.0 - 4.0, plate_r.bottom() - b.h / 2.0 - 4.0, b.w, b.h));
                        gfx::set_image(&t.thumb, Some(&b.image));
                    }
                }
                None => {
                    gfx::set_hidden(&t.plate, true);
                    gfx::set_image(&t.thumb, image.as_ref());
                }
            }
        });
    }

    fn place_tiles(&mut self, i: usize, animate: bool, fresh: &[u64]) {
        let p = &self.panels[i];
        let strip = p.layout.strip;
        let frames: Vec<Rect> = p.layout.tiles.iter().map(|t| t.offset(-strip.x, -strip.y)).collect();
        // Fresh tiles start small and transparent, then grow in.
        if animate {
            transaction(None, || {
                for (t, f) in p.tiles.iter().zip(&frames) {
                    if fresh.contains(&t.id) {
                        gfx::set_frame(&t.root, *f);
                        gfx::set_opacity(&t.root, 0.0);
                        gfx::set_transform(&t.root, gfx::AffineTransform::scale(0.6));
                    }
                }
            });
        }
        transaction(if animate { Some(0.28) } else { None }, || {
            for (t, f) in p.tiles.iter().zip(&frames) {
                gfx::set_transform(&t.root, gfx::AffineTransform::scale(1.0));
                gfx::set_frame(&t.root, *f);
                gfx::set_opacity(&t.root, 1.0);
            }
        });
        let empty = p.tiles.is_empty();
        // Edge fades where the strip has more content to scroll to.
        let l = &p.layout;
        let fade = 22.0 / l.strip.w.max(1.0);
        let left = if l.scroll > 0.5 { fade } else { 0.0 };
        let right = if l.scroll < l.max_scroll - 0.5 { fade } else { 0.0 };
        transaction(if animate { Some(0.2) } else { None }, || {
            gfx::set_opacity(&p.empty, if empty { 1.0 } else { 0.0 });
            gfx::set_hidden(&p.strip, empty);
        });
        transaction(None, || {
            gfx::set_frame(&p.strip_fade, Rect::new(0.0, 0.0, l.strip.w, l.strip.h));
            gfx::set_gradient_h(&p.strip_fade, left, right);
        });
    }

    fn update_header(&mut self, i: usize) {
        let n = self.display.iter().filter(|d| d.id != CLIP_ID).count();
        let drop_hover = self.panels[i].drop_hover;
        let key = format!("{n}|{drop_hover}");
        if self.panels[i].header_key == key {
            return;
        }
        self.panels[i].header_key = key;
        let scale = self.panels[i].scale;
        let icon = self.sym(if n > 0 { "tray.full.fill" } else { "tray.fill" }, 13.0, gfx::W_MEDIUM, (1.0, 1.0, 1.0, 0.92), scale);
        let p = &self.panels[i];
        let l = &p.layout;
        let title = Self::local(p, l.title);
        let st = TextStyle { size: 13.0, weight: gfx::W_SEMIBOLD, align: "left", truncate: "end" };
        let text = "Shelf";
        let tw = gfx::text_width(text, 13.0, gfx::W_SEMIBOLD);
        let count = n.to_string();
        let cw = gfx::text_width(&count, 11.0, gfx::W_SEMIBOLD) + 12.0;
        transaction(None, || {
            let icon_r = Rect::new(title.x, title.y, 18.0, title.h);
            gfx::place_symbol(&p.title_icon, icon.as_ref(), icon_r);
            gfx::set_frame(&p.title_text, Rect::new(icon_r.right() + 6.0, title.center_y() - 8.5, tw + 4.0, 17.0));
            gfx::set_text(&p.title_text, text, &st, &white(0.95));
            let cx = icon_r.right() + 6.0 + tw + 7.0;
            gfx::set_frame(&p.count_bg, Rect::new(cx, title.center_y() - 8.0, cw, 16.0));
            gfx::set_corner(&p.count_bg, 8.0);
            gfx::set_bg(&p.count_bg, Some(&white(0.14)));
            gfx::set_frame(&p.count_text, Rect::new(0.0, 1.5, cw, 14.0));
            gfx::set_text(&p.count_text, &count, &TextStyle { size: 11.0, weight: gfx::W_SEMIBOLD, align: "center", truncate: "end" }, &white(0.85));
            gfx::set_hidden(&p.count_bg, n == 0);
        });
        // Empty-state text and icon follow the drop hover.
        let (txt, rgba) = if drop_hover { ("Release to add to the shelf", (self.accent.0, self.accent.1, self.accent.2, 1.0)) } else { ("Drop files, images, links or text here", (1.0, 1.0, 1.0, 0.5)) };
        let eicon = self.sym("tray.and.arrow.down.fill", 22.0, gfx::W_REGULAR, rgba, scale);
        let p = &self.panels[i];
        let sw = p.layout.strip.w;
        let sh = p.layout.strip.h;
        let border = if drop_hover { self.accent(0.9) } else { white(0.20) };
        let fill = if drop_hover { self.accent(0.12) } else { white(0.03) };
        let tcol = Color::rgba(rgba.0, rgba.1, rgba.2, if drop_hover { 1.0 } else { 0.55 });
        transaction(Some(0.15), || {
            gfx::place_symbol(&p.empty_icon, eicon.as_ref(), Rect::new(0.0, sh / 2.0 - 30.0, sw, 34.0));
            gfx::set_frame(&p.empty_text, Rect::new(8.0, sh / 2.0 + 10.0, sw - 16.0, 17.0));
            gfx::set_text(&p.empty_text, txt, &TextStyle { size: 12.5, weight: gfx::W_MEDIUM, align: "center", truncate: "end" }, &tcol);
            gfx::set_stroke(&p.empty_border, Some(&border), 1.2);
            gfx::set_fill(&p.empty_border, Some(&fill));
            let ring = self.accent(0.9);
            let ring_fill = self.accent(0.08);
            gfx::set_stroke(&p.drop_ring, Some(&ring), 2.0);
            gfx::set_fill(&p.drop_ring, Some(&ring_fill));
            gfx::set_hidden(&p.drop_ring, !(drop_hover && !p.tiles.is_empty()));
        });
    }

    /// Selection / hover visuals of tiles and buttons.
    fn update_states(&mut self, i: usize) {
        self.update_header(i);
        let scale = self.panels[i].scale;
        let has_items = !self.display.is_empty();
        let mut syms = vec![];
        for b in geo::BUTTONS {
            let name = match b {
                Button::DragAll => "square.stack.3d.up.fill",
                Button::Share => "square.and.arrow.up",
                Button::Clear => "trash",
            };
            syms.push(self.sym(name, 13.0, gfx::W_MEDIUM, (1.0, 1.0, 1.0, 0.9), scale));
        }
        let sel_bg = self.accent(0.30);
        let sel_border = self.accent(0.85);
        let hover_bg = white(0.08);
        let p = &mut self.panels[i];
        let hover_tile = match p.hover {
            Hit::Tile(k) | Hit::Close(k) => Some(k),
            _ => None,
        };
        let close_hot = matches!(p.hover, Hit::Close(_));
        let mut changed = vec![];
        for (k, t) in p.tiles.iter_mut().enumerate() {
            let st = TileState { selected: self.selected.contains(&t.id), hover: hover_tile == Some(k), close: hover_tile == Some(k) };
            if t.state != Some(st) {
                t.state = Some(st);
                changed.push(k);
            }
        }
        let bstate: Vec<(bool, bool)> = p.buttons.iter().map(|b| (p.hover == Hit::Button(b.kind), has_items)).collect();
        let buttons_changed = bstate != p.button_state;
        p.button_state = bstate.clone();
        let p = &self.panels[i];
        if !changed.is_empty() {
            transaction(Some(0.14), || {
                for k in changed {
                    let t = &p.tiles[k];
                    let st = t.state.unwrap_or_default();
                    gfx::set_bg(&t.root, if st.selected { Some(&sel_bg) } else if st.hover { Some(&hover_bg) } else { None });
                    gfx::set_border(&t.root, if st.selected { 1.0 } else { 0.0 }, if st.selected { Some(&sel_border) } else { None });
                    gfx::set_opacity(&t.close, if st.close { 1.0 } else { 0.0 });
                    let cbg = if close_hot && st.close { Color::rgba(0.42, 0.42, 0.45, 1.0) } else { Color::rgba(0.30, 0.30, 0.32, 1.0) };
                    gfx::set_bg(&t.close, Some(&cbg));
                }
            });
        }
        if buttons_changed {
            transaction(Some(0.12), || {
                for (k, b) in p.buttons.iter().enumerate() {
                    let (hot, enabled) = bstate[k];
                    let r = p.layout.buttons.iter().find(|(x, _)| *x == b.kind).map(|(_, r)| *r).unwrap_or_default();
                    gfx::place_symbol(&b.icon, syms[k].as_ref(), Rect::new(0.0, 0.0, r.w, r.h));
                    let hot_bg = white(0.14);
                    gfx::set_bg(&b.bg, if hot && enabled { Some(&hot_bg) } else { None });
                    gfx::set_opacity(&b.bg, if enabled { 1.0 } else { 0.35 });
                }
            });
        }
    }

    // ----- events -----------------------------------------------------

    fn item(&self, id: u64) -> Option<&ShelfItem> {
        self.display.iter().find(|d| d.id == id)
    }

    fn selection_items(&self) -> Vec<ShelfItem> {
        self.display.iter().filter(|d| self.selected.contains(&d.id)).cloned().collect()
    }

    fn remove_ids(&mut self, ids: &[u64]) {
        if ids.contains(&CLIP_ID) {
            self.clip = None;
            self.clip_hidden_for = self.clip_count;
            self.refresh_display();
        }
        let real: Vec<u64> = ids.iter().copied().filter(|i| *i != CLIP_ID).collect();
        if !real.is_empty() {
            self.shared.edit(|s| s.remove(&real));
        }
        // Applied on the next tick through items_gen.
    }

    pub fn on_mouse_down(&mut self, i: usize, pt: Point, mods: Mods, clicks: isize) {
        let Some(p) = self.panels.get(i) else { return };
        if p.mode != Mode::Expanded {
            return;
        }
        let close_for = match p.hover {
            Hit::Tile(k) | Hit::Close(k) => Some(k),
            _ => None,
        };
        let hit = geo::hit_test(&p.layout, pt, close_for, &self.metrics);
        self.press = Some(Press { panel: i, start: pt, hit, mods, dragged: false });
        match hit {
            Hit::Close(k) => {
                if let Some(id) = self.display.get(k).map(|d| d.id) {
                    self.remove_ids(&[id]);
                }
                self.press = None;
            }
            Hit::Tile(k) => {
                let Some(id) = self.display.get(k).map(|d| d.id) else { return };
                if mods.cmd {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                    }
                } else if mods.shift && self.anchor.is_some() {
                    let a = self.display.iter().position(|d| Some(d.id) == self.anchor).unwrap_or(k);
                    let (lo, hi) = (a.min(k), a.max(k));
                    self.selected = self.display[lo..=hi].iter().map(|d| d.id).collect();
                } else if !self.selected.contains(&id) {
                    self.selected = [id].into_iter().collect();
                }
                if !mods.shift {
                    self.anchor = Some(id);
                }
                if clicks == 2 {
                    if let Some(it) = self.item(id).cloned() {
                        pboard::open(&[it]);
                    }
                }
            }
            Hit::Button(_) => {}
            _ => {
                if !mods.cmd {
                    self.selected.clear();
                }
            }
        }
        self.update_states(i);
    }

    pub fn on_mouse_dragged(&mut self, i: usize, pt: Point) -> Option<DragOut> {
        let press = self.press.as_mut()?;
        if press.dragged || press.panel != i {
            return None;
        }
        if (pt.x - press.start.x).hypot(pt.y - press.start.y) < 4.0 {
            return None;
        }
        let ids: Vec<u64> = match press.hit {
            Hit::Tile(k) => {
                let id = self.display.get(k)?.id;
                if self.selected.contains(&id) {
                    self.display.iter().filter(|d| self.selected.contains(&d.id)).map(|d| d.id).collect()
                } else {
                    vec![id]
                }
            }
            Hit::Button(Button::DragAll) => self.display.iter().map(|d| d.id).collect(),
            _ => return None,
        };
        press.dragged = true;
        let start = press.start;
        let p = &self.panels[i];
        let m = self.metrics;
        let mut items = vec![];
        for id in &ids {
            let Some(k) = self.display.iter().position(|d| d.id == *id) else { continue };
            let it = &self.display[k];
            let frame = p.layout.tiles.get(k).copied().unwrap_or(Rect::new(start.x - m.tile_w / 2.0, start.y - m.tile_h / 2.0, m.tile_w, m.tile_h));
            let tile = &p.tiles[k];
            let img = gfx::render_layer(&tile.root, m.tile_w, m.tile_h, p.scale, false);
            let ns_img = match img {
                Some(img) => gfx::nsimage_from_cg(&img, m.tile_w, m.tile_h),
                None => match it.path() {
                    Some(path) => pboard::file_icon(path),
                    None => gfx::symbol_nsimage("doc", 40.0, gfx::W_REGULAR)?,
                },
            };
            items.push((pboard::writer(it), Self::local(p, frame), ns_img));
        }
        self.own_drag = Some(ids);
        Some(DragOut { items })
    }

    pub fn on_mouse_up(&mut self, i: usize, pt: Point) -> Option<Deferred> {
        let press = self.press.take()?;
        if press.dragged || press.panel != i {
            return None;
        }
        let p = self.panels.get(i)?;
        let hit = geo::hit_test(&p.layout, pt, None, &self.metrics);
        match press.hit {
            Hit::Button(b) if hit == Hit::Button(b) => match b {
                Button::Share => {
                    let items = if self.selected.is_empty() { self.display.clone() } else { self.selection_items() };
                    let anchor = p.layout.buttons.iter().find(|(x, _)| *x == b).map(|(_, r)| Self::local(p, *r)).unwrap_or_default();
                    self.sharing = !items.is_empty();
                    return Some(Deferred::Share { items: items.iter().map(pboard::writer).collect(), anchor });
                }
                Button::Clear => {
                    self.selected.clear();
                    if self.clip.is_some() {
                        self.remove_ids(&[CLIP_ID]);
                    }
                    self.shared.edit(|s| s.clear());
                }
                Button::DragAll => {
                    self.selected = self.display.iter().map(|d| d.id).collect();
                    self.update_states(i);
                }
            },
            Hit::Tile(k) if !press.mods.cmd && !press.mods.shift => {
                if let Some(id) = self.display.get(k).map(|d| d.id) {
                    if self.selected.len() > 1 {
                        self.selected = [id].into_iter().collect();
                        self.update_states(i);
                    }
                }
            }
            _ => {}
        }
        None
    }

    pub fn on_right_down(&mut self, i: usize, pt: Point) -> Option<MenuReq> {
        let p = self.panels.get(i)?;
        if p.mode != Mode::Expanded {
            return None;
        }
        let hit = geo::hit_test(&p.layout, pt, None, &self.metrics);
        let mut entries = vec![];
        match hit {
            Hit::Tile(k) => {
                let id = self.display.get(k)?.id;
                if !self.selected.contains(&id) {
                    self.selected = [id].into_iter().collect();
                }
                self.menu_ids = self.display.iter().filter(|d| self.selected.contains(&d.id)).map(|d| d.id).collect();
                self.menu_anchor = Self::local(p, p.layout.tiles[k]);
                let items = self.selection_items();
                let any_file = items.iter().any(|i| i.path().is_some());
                let n = items.len();
                let suffix = if n > 1 { format!(" {n} Items") } else { String::new() };
                entries.push((format!("Open{suffix}"), M_OPEN, "arrow.up.forward.app", true));
                entries.push(("Show in Finder".to_string(), M_REVEAL, "folder", any_file));
                entries.push(("Copy".to_string(), M_COPY, "doc.on.doc", true));
                entries.push(("Share…".to_string(), M_SHARE, "square.and.arrow.up", true));
                if self.menu_ids.contains(&CLIP_ID) {
                    entries.push(("Keep on Shelf".to_string(), M_KEEP, "pin", true));
                }
                entries.push((String::new(), -1, "", false));
                entries.push((format!("Remove{suffix}"), M_REMOVE, "xmark.circle", true));
            }
            _ => {
                self.menu_ids = self.display.iter().map(|d| d.id).collect();
                self.menu_anchor = Rect::new(pt.x - p.win.x - 1.0, pt.y - p.win.y - 1.0, 2.0, 2.0);
                let any = !self.menu_ids.is_empty();
                entries.push(("Share All…".to_string(), M_SHARE, "square.and.arrow.up", any));
            }
        }
        entries.push((String::new(), -1, "", false));
        entries.push(("Clear Shelf".to_string(), M_CLEAR, "trash", !self.display.is_empty()));
        self.update_states(i);
        Some(MenuReq { entries })
    }

    pub fn on_menu(&mut self, tag: isize) -> Option<Deferred> {
        let items: Vec<ShelfItem> = self.menu_ids.iter().filter_map(|id| self.item(*id).cloned()).collect();
        match tag {
            M_OPEN => pboard::open(&items),
            M_REVEAL => pboard::reveal(&items),
            M_COPY => pboard::copy_to_clipboard(&items),
            M_SHARE => {
                self.sharing = !items.is_empty();
                return Some(Deferred::Share { items: items.iter().map(pboard::writer).collect(), anchor: self.menu_anchor });
            }
            M_REMOVE => {
                let ids = self.menu_ids.clone();
                self.remove_ids(&ids);
            }
            M_CLEAR => {
                self.remove_ids(&[CLIP_ID]);
                self.shared.edit(|s| s.clear());
            }
            M_KEEP => {
                if let Some(c) = self.clip.clone() {
                    let n = match c.kind {
                        ItemKind::File { path, .. } => crate::NewItem::File(path),
                        ItemKind::Url { url, title } => crate::NewItem::Url { url, title },
                        ItemKind::Text { text } => crate::NewItem::Text(text),
                    };
                    self.shared.add(vec![n]);
                }
            }
            _ => {}
        }
        None
    }

    pub fn on_scroll(&mut self, i: usize, dx: f64, dy: f64, precise: bool) {
        let m = self.metrics;
        let Some(p) = self.panels.get_mut(i) else { return };
        let mut d = if dx.abs() > dy.abs() { dx } else { dy };
        if !precise {
            d *= 14.0;
        }
        let s = geo::clamp_scroll(p.scroll - d, p.layout.max_scroll);
        if s == p.scroll {
            return;
        }
        p.scroll = s;
        p.layout = geo::layout(&p.expanded, &p.collapsed, p.tiles.len(), s, &m);
        self.place_tiles(i, false, &[]);
    }

    pub fn on_drag_hover(&mut self, i: usize, on: bool) {
        let Some(p) = self.panels.get_mut(i) else { return };
        if p.drop_hover != on {
            p.drop_hover = on;
            if on && p.mode != Mode::Expanded {
                p.logic.expand();
            }
            self.update_header(i);
        }
    }

    pub fn on_drop(&mut self, i: usize, pb: &AnyObject) -> bool {
        let (items, promises) = pboard::read(pb);
        let n = items.len();
        let ids = self.shared.add(items);
        if promises {
            let dest = {
                let st = self.shared.lock();
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                st.store.files_dir().join(stamp.to_string())
            };
            pboard::receive_promises(pb, self.shared.clone(), dest, || {});
        }
        if self.cfg.haptics && (!ids.is_empty() || promises) {
            pboard::haptic();
        }
        if let Some(p) = self.panels.get_mut(i) {
            p.drop_hover = false;
            // Show the newest items.
            p.scroll = f64::MAX;
            p.hold = false;
        }
        self.update_header(i);
        log::info!("notch-shelf: dropped {n} item(s), added {}{}", ids.len(), if promises { " (+ file promises)" } else { "" });
        !ids.is_empty() || promises
    }

    pub fn on_drag_out_ended(&mut self, op: usize) {
        if let Some(ids) = self.own_drag.take() {
            if op != 0 && self.cfg.remove_after_drag_out {
                let ids: Vec<u64> = ids.into_iter().filter(|i| *i != CLIP_ID).collect();
                self.remove_ids(&ids);
            }
        }
        // The drag pasteboard changed under us; don't treat it as foreign.
        self.drag_count = pboard::change_count(&self.drag_pb);
        self.press = None;
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

    // ----- diagnostics ------------------------------------------------

    /// Render each panel over a mock wallpaper + menu bar + hardware notch.
    fn render_pngs(&mut self, dir: &std::path::Path) {
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
                // Wallpaper-ish gradient (y up in this context).
                let comps = [0.33, 0.42, 0.62, 1.0, 0.72, 0.50, 0.60, 1.0];
                let locs = [0.0, 1.0];
                let g = CGGradientCreateWithColorComponents(cs, comps.as_ptr(), locs.as_ptr(), 2);
                CGContextDrawLinearGradient(ctx, g, CGPoint { x: 0.0, y: h }, CGPoint { x: w, y: 0.0 }, 0);
                CGGradientRelease(g);
                CGColorSpaceRelease(cs);
                // Translucent menu bar.
                let mb = if p.geom.menu_bar_h > 0.0 { p.geom.menu_bar_h } else { p.collapsed.h };
                CGContextSetRGBFillColor(ctx, 0.12, 0.12, 0.16, 0.55);
                CGContextFillRect(ctx, CGRect { origin: CGPoint { x: 0.0, y: h - mb }, size: CGSize { width: w, height: mb } });
                // Hardware notch.
                if p.geom.notch.is_some() {
                    let nr = Self::local(p, p.collapsed);
                    let path = Path::rounded_rect(Rect::new(nr.x, h - nr.bottom(), nr.w, nr.h + 12.0), 9.0);
                    CGContextSetRGBFillColor(ctx, 0.0, 0.0, 0.0, 1.0);
                    CGContextAddPath(ctx, path.0 .0);
                    CGContextFillPath(ctx);
                }
                let root: Layer = msg_send![&*p.view, layer];
                let _: () = msg_send![&*root, renderInContext: CgContext(ctx)];
                let img = gfx::Image::from_owned(CGBitmapContextCreateImage(ctx));
                CGContextRelease(ctx);
                if let Some(img) = img {
                    let path = dir.join(format!("notch-shelf-{n}.png"));
                    if gfx::write_png(&img, &path) {
                        log::info!("notch-shelf: rendered {}", path.display());
                    }
                }
            }
        }
    }
}

