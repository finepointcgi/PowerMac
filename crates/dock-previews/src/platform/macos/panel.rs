//! The preview panel: a borderless, non-activating `NSPanel` whose content
//! is a rounded, dark HUD `NSVisualEffectView` (the window switcher's
//! look). A header row (app icon, name, window count) and one tile per
//! window drawn with Core Animation: thumbnail (or a plate with the app
//! icon without Screen Recording), title, status badge, hover highlight,
//! close / minimize buttons, and an accent ring for the scroll selection.
//! Everything here runs on the main thread; other threads post through
//! [`super::ffi::dispatch_main`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use anyhow::{bail, Result};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSColorSpace, NSPanel, NSScreen, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};

use super::apps;
use super::ffi::*;
use super::thumbs;
use crate::geom::RectF;
use crate::layout::{fit, Hit, Layout, Tile};

type Layer = Retained<AnyObject>;

/// `NSPopUpMenuWindowLevel`: above the Dock (which is at 20) and floating panels.
const PANEL_LEVEL: isize = 101;
const PANEL_RADIUS: f64 = 16.0;
const TILE_RADIUS: f64 = 11.0;
const THUMB_RADIUS: f64 = 7.0;

/// What one tile shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TileView {
    pub wid: u32,
    pub title: String,
    /// Window aspect ratio (for the placeholder plate).
    pub aspect: f64,
    pub badge: Option<&'static str>,
    pub dimmed: bool,
}

/// Everything needed to draw the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub pid: i32,
    pub app_name: String,
    pub app_path: String,
    pub items: Vec<TileView>,
    pub layout: Layout,
    /// Global frame (top-left origin); only the size matters offscreen.
    pub frame: RectF,
    /// Hovered tile and what in it.
    pub hover: Hit,
    /// Scroll selection.
    pub selected: Option<usize>,
    /// No Screen Recording: icons instead of thumbnails.
    pub icon_mode: bool,
}

// ------------------------------------------------------------ layer helpers

fn nsrect(r: RectF) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

unsafe fn layer() -> Layer {
    msg_send![class!(CALayer), layer]
}

unsafe fn set_frame(l: &AnyObject, r: RectF) {
    let _: () = msg_send![l, setFrame: nsrect(r)];
}

unsafe fn set_bg(l: &AnyObject, c: &OwnedColor) {
    let _: () = msg_send![l, setBackgroundColor: c.0];
}

unsafe fn set_border(l: &AnyObject, w: f64, c: &OwnedColor) {
    let _: () = msg_send![l, setBorderWidth: w];
    let _: () = msg_send![l, setBorderColor: c.0];
}

unsafe fn set_radius(l: &AnyObject, r: f64) {
    let _: () = msg_send![l, setCornerRadius: r];
}

unsafe fn set_hidden(l: &AnyObject, h: bool) {
    let _: () = msg_send![l, setHidden: h];
}

unsafe fn set_opacity(l: &AnyObject, o: f32) {
    let _: () = msg_send![l, setOpacity: o];
}

unsafe fn add(parent: &AnyObject, child: &AnyObject) {
    let _: () = msg_send![parent, addSublayer: child];
}

/// Top-left-origin rect → Core Animation's y-up coordinates inside a
/// parent of height `ph` (no `geometryFlipped`: offscreen `renderInContext:`
/// draws contents upside down under it).
fn flip(r: RectF, ph: f64) -> RectF {
    RectF::new(r.x, ph - r.y - r.h, r.w, r.h)
}

unsafe fn icon_image(img: &AnyObject, pts: f64, scale: f64) -> Option<Image> {
    let px = (pts * scale).round().max(1.0);
    let mut r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
    let null: *const AnyObject = std::ptr::null();
    let cg: CgImage = msg_send![img, CGImageForProposedRect: &mut r, context: null, hints: null];
    Image::from_borrowed(cg.0)
}

unsafe fn set_icon(l: &AnyObject, icon: Option<&AnyObject>, pts: f64, scale: f64) {
    match icon.and_then(|i| icon_image(i, pts, scale)) {
        Some(img) => set_contents_image(l, &img),
        None => {
            let p: *const AnyObject = std::ptr::null();
            let _: () = msg_send![l, setContents: p];
        }
    }
    let g = NSString::from_str("resizeAspect");
    let _: () = msg_send![l, setContentsGravity: &*g];
    let _: () = msg_send![l, setContentsScale: scale];
}

unsafe fn set_contents_image(l: &AnyObject, img: &Image) {
    let p = img.raw() as *const AnyObject;
    let _: () = msg_send![l, setContents: p];
}

unsafe fn system_font(size: f64, weight: f64) -> Retained<AnyObject> {
    msg_send![class!(NSFont), systemFontOfSize: size, weight: weight]
}

unsafe fn text_width(s: &str, font: &AnyObject) -> f64 {
    let key = NSString::from_str("NSFont");
    let attrs: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionaryWithObject: font, forKey: &*key];
    let ns = NSString::from_str(s);
    let size: NSSize = msg_send![&*ns, sizeWithAttributes: &*attrs];
    size.width.ceil()
}

const WEIGHT_MEDIUM: f64 = 0.23;
const WEIGHT_SEMIBOLD: f64 = 0.3;

unsafe fn text_layer(s: &str, font: &AnyObject, size: f64, color: &OwnedColor, align: &str, scale: f64) -> Layer {
    let l: Layer = msg_send![class!(CATextLayer), layer];
    let _: () = msg_send![&*l, setFont: font as *const AnyObject as *const c_void];
    let _: () = msg_send![&*l, setFontSize: size];
    let _: () = msg_send![&*l, setForegroundColor: color.0];
    let ns = NSString::from_str(s);
    let _: () = msg_send![&*l, setString: &*ns];
    let a = NSString::from_str(align);
    let _: () = msg_send![&*l, setAlignmentMode: &*a];
    let t = NSString::from_str("end");
    let _: () = msg_send![&*l, setTruncationMode: &*t];
    let _: () = msg_send![&*l, setWrapped: false];
    let _: () = msg_send![&*l, setContentsScale: scale];
    l
}

/// Rotate a layer about its center (KVC `transform.rotation.z`).
unsafe fn rotate(l: &AnyObject, radians: f64) {
    let n = NSNumber::new_f64(radians);
    let k = NSString::from_str("transform.rotation.z");
    let _: () = msg_send![l, setValue: &*n, forKeyPath: &*k];
}

/// The system accent color (sRGB 0..1).
fn accent() -> (f64, f64, f64) {
    let c = NSColor::controlAccentColor();
    match c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) {
        Some(c) => (c.redComponent(), c.greenComponent(), c.blueComponent()),
        None => (0.04, 0.52, 1.0),
    }
}

// ------------------------------------------------------------- layer tree

/// A traffic-light style round button with a glyph made of bars (exactly
/// centered at any scale, unlike a text glyph).
unsafe fn round_button(r: RectF, ph: f64, fill: (f64, f64, f64), glyph: (f64, f64, f64), cross: bool) -> Layer {
    let b = layer();
    set_frame(&b, flip(r, ph));
    set_radius(&b, r.w / 2.0);
    set_bg(&b, &OwnedColor::rgba(fill.0, fill.1, fill.2, 1.0));
    set_border(&b, 0.5, &OwnedColor::white(0.0, 0.25));
    let _: () = msg_send![&*b, setShadowColor: OwnedColor::white(0.0, 1.0).0];
    let _: () = msg_send![&*b, setShadowOpacity: 0.35f32];
    let _: () = msg_send![&*b, setShadowRadius: 2.0f64];
    let _: () = msg_send![&*b, setShadowOffset: NSSize::new(0.0, -1.0)];
    let len = (r.w * 0.5).round();
    let th = 1.6;
    let bars: &[f64] = if cross { &[std::f64::consts::FRAC_PI_4, -std::f64::consts::FRAC_PI_4] } else { &[0.0] };
    for &a in bars {
        let bar = layer();
        let _: () = msg_send![&*bar, setBounds: nsrect(RectF::new(0.0, 0.0, len, th))];
        let _: () = msg_send![&*bar, setPosition: NSPoint::new(r.w / 2.0, r.h / 2.0)];
        set_radius(&bar, th / 2.0);
        set_bg(&bar, &OwnedColor::rgba(glyph.0, glyph.1, glyph.2, 0.9));
        rotate(&bar, a);
        add(&b, &bar);
    }
    b
}

struct TileLayers {
    wid: u32,
    tile: Tile,
    aspect: f64,
    icon_mode: bool,
    dimmed: bool,
    hover_bg: Layer,
    ring: Layer,
    shadow: Layer,
    thumb: Layer,
    plate: Layer,
    big_icon: Layer,
    badge: Option<Layer>,
    title: Layer,
    buttons: Layer,
    close: Layer,
    minimize: Layer,
    ph: f64,
}

struct Tree {
    root: Layer,
    tiles: Vec<TileLayers>,
}

unsafe fn build_tree(model: &Model, scale: f64, image_for: &dyn Fn(u32) -> Option<Image>) -> Tree {
    let l = &model.layout;
    let ph = l.h;
    let root = layer();
    set_frame(&root, RectF::new(0.0, 0.0, l.w, l.h));
    let icon = (if model.pid > 0 { apps::icon(model.pid) } else { None }).or_else(|| apps::icon_for_path(&model.app_path));

    // Header: icon, app name, window count.
    let hdr = l.header;
    let isz = 20.0;
    let hi = layer();
    set_icon(&hi, icon.as_deref(), isz, scale);
    set_frame(&hi, flip(RectF::new(hdr.x, hdr.y + ((hdr.h - isz) / 2.0).round(), isz, isz), ph));
    add(&root, &hi);
    let count = match model.items.len() {
        1 => "1 window".to_string(),
        n => format!("{n} windows"),
    };
    let cfont = system_font(11.5, WEIGHT_MEDIUM);
    let cw = text_width(&count, &cfont);
    let ct = text_layer(&count, &cfont, 11.5, &OwnedColor::white(1.0, 0.5), "right", scale);
    set_frame(&ct, flip(RectF::new(hdr.right() - cw - 2.0, hdr.y + ((hdr.h - 15.0) / 2.0).round() + 1.0, cw + 2.0, 15.0), ph));
    add(&root, &ct);
    let nfont = system_font(13.0, WEIGHT_SEMIBOLD);
    let nx = hdr.x + isz + 8.0;
    let nt = text_layer(&model.app_name, &nfont, 13.0, &OwnedColor::white(1.0, 0.95), "left", scale);
    set_frame(&nt, flip(RectF::new(nx, hdr.y + ((hdr.h - 17.0) / 2.0).round(), (hdr.right() - cw - 12.0 - nx).max(0.0), 17.0), ph));
    add(&root, &nt);

    let (ar, ag, ab) = accent();
    let title_font = system_font(12.0, WEIGHT_MEDIUM);
    let badge_font = system_font(10.0, WEIGHT_SEMIBOLD);
    let mut tiles = Vec::new();
    for (item, tile) in model.items.iter().zip(l.tiles.iter()) {
        let hover_bg = layer();
        set_frame(&hover_bg, flip(tile.frame, ph));
        set_radius(&hover_bg, TILE_RADIUS);
        set_bg(&hover_bg, &OwnedColor::white(1.0, 0.10));
        set_border(&hover_bg, 1.0, &OwnedColor::white(1.0, 0.08));
        set_hidden(&hover_bg, true);
        add(&root, &hover_bg);

        let ring = layer();
        set_frame(&ring, flip(tile.frame, ph));
        set_radius(&ring, TILE_RADIUS);
        set_border(&ring, 2.5, &OwnedColor::rgba(ar, ag, ab, 1.0));
        let _: () = msg_send![&*ring, setShadowColor: OwnedColor::rgba(ar, ag, ab, 1.0).0];
        let _: () = msg_send![&*ring, setShadowOpacity: 0.45f32];
        let _: () = msg_send![&*ring, setShadowRadius: 8.0f64];
        let _: () = msg_send![&*ring, setShadowOffset: NSSize::new(0.0, 0.0)];
        set_hidden(&ring, true);
        add(&root, &ring);

        let shadow = layer();
        let _: () = msg_send![&*shadow, setShadowColor: OwnedColor::white(0.0, 1.0).0];
        let _: () = msg_send![&*shadow, setShadowOpacity: 0.5f32];
        let _: () = msg_send![&*shadow, setShadowRadius: 6.0f64];
        let _: () = msg_send![&*shadow, setShadowOffset: NSSize::new(0.0, -2.0)];
        set_radius(&shadow, THUMB_RADIUS);
        set_bg(&shadow, &OwnedColor::white(0.0, 1.0));
        add(&root, &shadow);

        let thumb = layer();
        set_radius(&thumb, THUMB_RADIUS);
        let _: () = msg_send![&*thumb, setMasksToBounds: true];
        set_border(&thumb, 1.0 / scale.max(1.0), &OwnedColor::white(1.0, 0.14));
        let gravity = NSString::from_str("resize");
        let _: () = msg_send![&*thumb, setContentsGravity: &*gravity];
        add(&root, &thumb);

        let plate = layer();
        set_radius(&plate, THUMB_RADIUS);
        set_bg(&plate, &OwnedColor::white(1.0, 0.06));
        set_border(&plate, 1.0, &OwnedColor::white(1.0, 0.08));
        add(&root, &plate);

        let big_icon = layer();
        set_icon(&big_icon, icon.as_deref(), (tile.thumb.h * 0.5).min(96.0), scale);
        add(&root, &big_icon);

        let title = text_layer(&item.title, &title_font, 12.0, &OwnedColor::white(1.0, 0.86), "center", scale);
        let lb = tile.label;
        set_frame(&title, flip(RectF::new(lb.x, lb.y + ((lb.h - 16.0) / 2.0).round(), lb.w, 16.0), ph));
        add(&root, &title);

        let badge = item.badge.map(|text| {
            let tw = text_width(text, &badge_font);
            let pill = layer();
            set_radius(&pill, 8.0);
            set_bg(&pill, &OwnedColor::white(0.0, 0.62));
            set_border(&pill, 0.5, &OwnedColor::white(1.0, 0.18));
            let t = text_layer(text, &badge_font, 10.0, &OwnedColor::white(1.0, 0.92), "center", scale);
            set_frame(&t, RectF::new(0.0, 1.5, tw + 14.0, 13.0));
            add(&pill, &t);
            let _: () = msg_send![&*pill, setBounds: nsrect(RectF::new(0.0, 0.0, tw + 14.0, 17.0))];
            add(&root, &pill);
            pill
        });

        // Buttons (a container so hover shows/hides them together).
        let buttons = layer();
        set_frame(&buttons, RectF::new(0.0, 0.0, l.w, l.h));
        let close = round_button(tile.close, ph, (1.0, 0.37, 0.34), (0.30, 0.0, 0.0), true);
        let minimize = round_button(tile.minimize, ph, (1.0, 0.74, 0.18), (0.42, 0.24, 0.0), false);
        add(&buttons, &close);
        add(&buttons, &minimize);
        set_hidden(&buttons, true);

        let mut tl = TileLayers {
            wid: item.wid,
            tile: *tile,
            aspect: item.aspect,
            icon_mode: model.icon_mode,
            dimmed: item.dimmed,
            hover_bg,
            ring,
            shadow,
            thumb,
            plate,
            big_icon,
            badge,
            title,
            buttons,
            close,
            minimize,
            ph,
        };
        let img = if model.icon_mode { None } else { image_for(item.wid) };
        tl.set_image(img.as_ref());
        tiles.push(tl);
    }
    // Buttons above everything else.
    for t in &tiles {
        add(&root, &t.buttons);
    }
    let tree = Tree { root, tiles };
    tree.set_state(model.hover, model.selected);
    tree
}

impl TileLayers {
    /// Show `img` as the thumbnail, or the placeholder when `None`.
    unsafe fn set_image(&mut self, img: Option<&Image>) {
        let area = self.tile.thumb;
        let opacity: f32 = if self.dimmed { 0.55 } else { 1.0 };
        let content = match img {
            Some(img) => {
                let (w, h) = img.size();
                let r = fit(area, w, h);
                set_contents_image(&self.thumb, img);
                set_frame(&self.thumb, flip(r, self.ph));
                set_frame(&self.shadow, flip(r.inset(1.0), self.ph));
                set_opacity(&self.thumb, opacity);
                set_opacity(&self.shadow, opacity);
                set_hidden(&self.thumb, false);
                set_hidden(&self.shadow, false);
                set_hidden(&self.plate, true);
                set_hidden(&self.big_icon, true);
                r
            }
            None => {
                set_hidden(&self.thumb, true);
                set_hidden(&self.shadow, true);
                let r = fit(area, self.aspect.clamp(crate::layout::MIN_ASPECT, crate::layout::MAX_ASPECT), 1.0);
                set_hidden(&self.plate, false);
                set_frame(&self.plate, flip(r, self.ph));
                let s = (r.h * 0.5).min(r.w * 0.5).min(96.0).round();
                let ir = RectF::new((r.x + (r.w - s) / 2.0).round(), (r.y + (r.h - s) / 2.0).round(), s, s);
                set_frame(&self.big_icon, flip(ir, self.ph));
                set_hidden(&self.big_icon, false);
                set_opacity(&self.big_icon, opacity);
                r
            }
        };
        let _ = self.icon_mode;
        if let Some(b) = &self.badge {
            let bounds: NSRect = msg_send![&**b, bounds];
            let (bw, bh) = (bounds.size.width, bounds.size.height);
            set_frame(b, flip(RectF::new(content.right() - bw - 6.0, content.bottom() - bh - 6.0, bw, bh), self.ph));
        }
    }
}

impl Tree {
    unsafe fn set_state(&self, hover: Hit, selected: Option<usize>) {
        let hovered = match hover {
            Hit::Tile(i) | Hit::Close(i) | Hit::Minimize(i) => Some(i),
            _ => None,
        };
        for (j, t) in self.tiles.iter().enumerate() {
            let h = hovered == Some(j);
            set_hidden(&t.hover_bg, !h);
            set_hidden(&t.buttons, !h);
            set_hidden(&t.ring, selected != Some(j));
            let c = if h || selected == Some(j) { OwnedColor::white(1.0, 1.0) } else { OwnedColor::white(1.0, 0.82) };
            let _: () = msg_send![&*t.title, setForegroundColor: c.0];
            // The button under the pointer is fully opaque, the other dimmed a bit.
            set_opacity(&t.close, if matches!(hover, Hit::Minimize(_)) { 0.8 } else { 1.0 });
            set_opacity(&t.minimize, if matches!(hover, Hit::Close(_)) { 0.8 } else { 1.0 });
        }
    }
}

// ------------------------------------------------------------- the panel

struct Panel {
    panel: Retained<NSPanel>,
    canvas: Retained<NSView>,
    effect: Retained<NSVisualEffectView>,
    tree: Option<Tree>,
    model: Option<Model>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn primary_height() -> f64 {
    unsafe { CGDisplayBounds(CGMainDisplayID()).size.height }
}

fn to_cocoa(r: RectF) -> NSRect {
    NSRect::new(NSPoint::new(r.x, primary_height() - (r.y + r.h)), NSSize::new(r.w, r.h))
}

fn backing_scale(mtm: MainThreadMarker, r: RectF) -> f64 {
    let c = to_cocoa(r);
    let (cx, cy) = (c.origin.x + c.size.width / 2.0, c.origin.y + c.size.height / 2.0);
    let screens = NSScreen::screens(mtm);
    for s in screens.iter() {
        let f = s.frame();
        if cx >= f.origin.x && cx < f.origin.x + f.size.width && cy >= f.origin.y && cy < f.origin.y + f.size.height {
            return s.backingScaleFactor();
        }
    }
    screens.iter().next().map(|s| s.backingScaleFactor()).unwrap_or(2.0)
}

fn make_panel(mtm: MainThreadMarker) -> Panel {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 200.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    // The panel takes the pointer (so whatever is underneath gets no hover
    // or cursor changes); clicks / scrolls are handled by the event tap.
    panel.setIgnoresMouseEvents(false);
    panel.setAcceptsMouseMovedEvents(false);
    panel.setLevel(PANEL_LEVEL);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    unsafe {
        let _: () = msg_send![&*panel, setAnimationBehavior: 2isize];
    }
    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    unsafe {
        let name = NSString::from_str("NSAppearanceNameDarkAqua");
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSAppearance), appearanceNamed: &*name];
        if let Some(app) = app {
            let _: () = msg_send![&*effect, setAppearance: &*app];
        }
        let l: Option<Layer> = msg_send![&*effect, layer];
        if let Some(l) = l {
            set_radius(&l, PANEL_RADIUS);
            let _: () = msg_send![&*l, setMasksToBounds: true];
            set_border(&l, 1.0, &OwnedColor::white(1.0, 0.13));
            set_bg(&l, &OwnedColor::rgba(0.07, 0.07, 0.08, 0.30));
        }
    }
    let canvas = NSView::initWithFrame(NSView::alloc(mtm), frame);
    canvas.setWantsLayer(true);
    effect.addSubview(&canvas);
    panel.setContentView(Some(&effect));
    Panel { panel, canvas, effect, tree: None, model: None }
}

unsafe fn fade_in(l: &AnyObject) {
    let key = NSString::from_str("opacity");
    let anim: Layer = msg_send![class!(CABasicAnimation), animationWithKeyPath: &*key];
    let from = NSNumber::new_f64(0.0);
    let to = NSNumber::new_f64(1.0);
    let _: () = msg_send![&*anim, setFromValue: &*from];
    let _: () = msg_send![&*anim, setToValue: &*to];
    let _: () = msg_send![&*anim, setDuration: 0.12f64];
    let _: () = msg_send![l, addAnimation: &*anim, forKey: &*key];
}

/// Show (or rebuild) the panel for `model`. `animate`: fade in (first show).
pub fn show(mtm: MainThreadMarker, model: Model, animate: bool) {
    autoreleasepool(|_| {
        PANEL.with(|cell| {
            let mut cell = cell.borrow_mut();
            let p = cell.get_or_insert_with(|| make_panel(mtm));
            let f = model.frame;
            let scale = backing_scale(mtm, f);
            let was_visible = p.panel.isVisible();
            unsafe {
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                if let Some(old) = p.tree.take() {
                    let _: () = msg_send![&*old.root, removeFromSuperlayer];
                }
                let local = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f.w, f.h));
                p.panel.setFrame_display(to_cocoa(f), false);
                p.effect.setFrame(local);
                p.canvas.setFrame(local);
                let tree = build_tree(&model, scale, &|wid| thumbs::cached(wid));
                let host: Option<Layer> = msg_send![&*p.canvas, layer];
                if let Some(host) = host {
                    add(&host, &tree.root);
                }
                let _: () = msg_send![class!(CATransaction), commit];
                p.tree = Some(tree);
                if animate && !was_visible {
                    let l: Option<Layer> = msg_send![&*p.effect, layer];
                    if let Some(l) = l {
                        fade_in(&l);
                    }
                }
            }
            p.model = Some(model);
            p.panel.orderFrontRegardless();
            p.panel.invalidateShadow();
        })
    })
}

/// Update hover / selection highlights.
pub fn set_state(_mtm: MainThreadMarker, hover: Hit, selected: Option<usize>) {
    PANEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(p) = cell.as_mut() else { return };
        if let Some(m) = p.model.as_mut() {
            m.hover = hover;
            m.selected = selected;
        }
        let Some(tree) = &p.tree else { return };
        unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setAnimationDuration: 0.08f64];
            tree.set_state(hover, selected);
            let _: () = msg_send![class!(CATransaction), commit];
        }
    })
}

/// A thumbnail arrived: swap it in if that window is shown.
pub fn set_thumbnail(_mtm: MainThreadMarker, wid: u32, img: Image) {
    PANEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(p) = cell.as_mut() else { return };
        let Some(tree) = p.tree.as_mut() else { return };
        unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            for t in tree.tiles.iter_mut().filter(|t| t.wid == wid && !t.icon_mode) {
                t.set_image(Some(&img));
            }
            let _: () = msg_send![class!(CATransaction), commit];
        }
    })
}

pub fn hide(_mtm: MainThreadMarker) {
    PANEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(p) = cell.as_mut() else { return };
        p.panel.orderOut(None);
        if let Some(old) = p.tree.take() {
            unsafe {
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![&*old.root, removeFromSuperlayer];
                let _: () = msg_send![class!(CATransaction), commit];
            }
        }
        p.model = None;
    })
}

pub fn destroy(_mtm: MainThreadMarker) {
    PANEL.with(|cell| {
        if let Some(p) = cell.borrow_mut().take() {
            p.panel.orderOut(None);
            p.panel.close();
        }
    })
}

/// The model currently shown (for debugging / live tests).
pub fn current_model() -> Option<Model> {
    PANEL.with(|c| c.borrow().as_ref().and_then(|p| p.model.clone()))
}

// ----------------------------------------------------- offscreen rendering

/// Render the live panel's layer tree (if shown) to a PNG — no screen
/// capture needed. Main thread only.
pub fn render_live_png(path: &str) -> Result<bool> {
    let Some(model) = current_model() else { return Ok(false) };
    let images: HashMap<u32, Image> = model.items.iter().filter_map(|i| thumbs::cached(i.wid).map(|img| (i.wid, img))).collect();
    render_png(&model, &images, path, 2.0)?;
    Ok(true)
}

/// Render `model` to a PNG for visual checks. Vibrancy can't be captured
/// offscreen, so the HUD is approximated by a translucent dark rounded
/// panel over a wallpaper-like gradient, with a mock Dock strip below.
/// `images` supplies thumbnails (window id → image).
pub fn render_png(model: &Model, images: &HashMap<u32, Image>, path: &str, scale: f64) -> Result<()> {
    let margin = 48.0;
    let dock_h = 64.0;
    let l = &model.layout;
    let (w, h) = (l.w + 2.0 * margin, l.h + 2.0 * margin + dock_h);
    autoreleasepool(|_| unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: true];
        let root = layer();
        set_frame(&root, RectF::new(0.0, 0.0, w, h));

        let grad: Layer = msg_send![class!(CAGradientLayer), layer];
        set_frame(&grad, RectF::new(0.0, 0.0, w, h));
        let c1 = OwnedColor::rgba(0.16, 0.30, 0.55, 1.0);
        let c2 = OwnedColor::rgba(0.45, 0.25, 0.50, 1.0);
        let c3 = OwnedColor::rgba(0.85, 0.50, 0.40, 1.0);
        let colors: Retained<AnyObject> = msg_send![class!(NSMutableArray), array];
        for c in [&c1, &c2, &c3] {
            let _: () = msg_send![&*colors, addObject: c.0 .0 as *const AnyObject];
        }
        let _: () = msg_send![&*grad, setColors: &*colors];
        let _: () = msg_send![&*grad, setStartPoint: NSPoint::new(0.0, 1.0)];
        let _: () = msg_send![&*grad, setEndPoint: NSPoint::new(1.0, 0.0)];
        add(&root, &grad);

        // Mock Dock strip at the bottom with the app's icon under the panel.
        let dock = layer();
        let dw = (l.w * 0.8).max(200.0);
        set_frame(&dock, RectF::new((w - dw) / 2.0, 4.0, dw, dock_h - 8.0));
        set_radius(&dock, 16.0);
        set_bg(&dock, &OwnedColor::white(1.0, 0.22));
        set_border(&dock, 0.5, &OwnedColor::white(1.0, 0.35));
        add(&root, &dock);
        let icon = (if model.pid > 0 { apps::icon(model.pid) } else { None }).or_else(|| apps::icon_for_path(&model.app_path));
        let di = layer();
        set_icon(&di, icon.as_deref(), 46.0, scale);
        set_frame(&di, RectF::new((w - 46.0) / 2.0, 9.0, 46.0, 46.0));
        add(&root, &di);
        let dot = layer();
        set_frame(&dot, RectF::new(w / 2.0 - 2.0, 5.0, 4.0, 4.0));
        set_radius(&dot, 2.0);
        set_bg(&dot, &OwnedColor::white(1.0, 0.8));
        add(&root, &dot);

        let py = dock_h + margin - 40.0;
        let panel = layer();
        set_frame(&panel, RectF::new(margin, py, l.w, l.h));
        set_radius(&panel, PANEL_RADIUS);
        set_bg(&panel, &OwnedColor::rgba(0.11, 0.11, 0.13, 0.84));
        set_border(&panel, 1.0, &OwnedColor::white(1.0, 0.13));
        let _: () = msg_send![&*panel, setShadowOpacity: 0.45f32];
        let _: () = msg_send![&*panel, setShadowRadius: 22.0f64];
        let _: () = msg_send![&*panel, setShadowOffset: NSSize::new(0.0, -8.0)];
        add(&root, &panel);

        let tree = build_tree(model, scale, &|wid| images.get(&wid).cloned());
        set_frame(&tree.root, RectF::new(margin, py, l.w, l.h));
        add(&root, &tree.root);
        let _: () = msg_send![class!(CATransaction), commit];

        let (pw, ph) = ((w * scale) as usize, (h * scale) as usize);
        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, 0, space, 1 | (4 << 12));
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            bail!("CGBitmapContextCreate failed");
        }
        CGContextScaleCTM(ctx, scale, scale);
        let _: () = msg_send![&*root, renderInContext: CgContext(ctx)];
        let img = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        let Some(img) = Image::from_owned(img) else { bail!("CGBitmapContextCreateImage failed") };
        let rep: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: Retained<AnyObject> = msg_send![rep, initWithCGImage: CgImage(img.raw())];
        let props: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        let data: Option<Retained<AnyObject>> = msg_send![&*rep, representationUsingType: 4usize, properties: &*props];
        let Some(data) = data else { bail!("PNG encoding failed") };
        let ns_path = NSString::from_str(path);
        let ok: bool = msg_send![&*data, writeToFile: &*ns_path, atomically: true];
        if !ok {
            bail!("could not write {path}");
        }
        Ok(())
    })
}

/// A synthetic "window" image for previews: title bar, sidebar and text
/// lines in the given tint.
pub fn fake_window_image(w: usize, h: usize, tint: (f64, f64, f64), dark: bool) -> Option<Image> {
    unsafe {
        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space, 1 | (4 << 12));
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return None;
        }
        let (wf, hf) = (w as f64, h as f64);
        let (bg, bar, line) = if dark { (0.13, 0.17, 0.32) } else { (0.97, 0.91, 0.80) };
        CGContextSetRGBFillColor(ctx, bg, bg, bg, 1.0);
        CGContextFillRect(ctx, CGRect::new(0.0, 0.0, wf, hf));
        let tb = hf * 0.085;
        CGContextSetRGBFillColor(ctx, bar, bar, bar, 1.0);
        CGContextFillRect(ctx, CGRect::new(0.0, hf - tb, wf, tb));
        for (i, c) in [(1.0, 0.37, 0.34), (1.0, 0.74, 0.18), (0.16, 0.79, 0.25)].iter().enumerate() {
            CGContextSetRGBFillColor(ctx, c.0, c.1, c.2, 1.0);
            let d = tb * 0.42;
            CGContextFillRect(ctx, CGRect::new(tb * 0.5 + i as f64 * d * 1.6, hf - tb / 2.0 - d / 2.0, d, d));
        }
        CGContextSetRGBFillColor(ctx, tint.0, tint.1, tint.2, if dark { 0.35 } else { 0.22 });
        CGContextFillRect(ctx, CGRect::new(0.0, 0.0, wf * 0.24, hf - tb));
        CGContextSetRGBFillColor(ctx, tint.0, tint.1, tint.2, 0.9);
        CGContextFillRect(ctx, CGRect::new(wf * 0.29, hf - tb - hf * 0.16, wf * 0.4, hf * 0.06));
        for i in 0..8 {
            let y = hf - tb - hf * 0.26 - i as f64 * hf * 0.075;
            if y < hf * 0.05 {
                break;
            }
            let len = [0.62, 0.55, 0.66, 0.4, 0.6, 0.5, 0.64, 0.3][i];
            CGContextSetRGBFillColor(ctx, line, line, line, 1.0);
            CGContextFillRect(ctx, CGRect::new(wf * 0.29, y, wf * len, hf * 0.028));
        }
        let img = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        Image::from_owned(img)
    }
}
