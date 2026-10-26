//! The switcher overlay: a borderless, non-activating `NSPanel` whose
//! content is a rounded, dark HUD `NSVisualEffectView`; tiles are Core
//! Animation layers (thumbnail or big app icon, small icon + title,
//! status badge) and one selection layer (soft fill + accent ring) that
//! glides between tiles. Everything here runs on the main thread; other
//! threads post through [`super::ffi::dispatch_main`].

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
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::apps;
use super::ffi::*;
use super::thumbs;
use crate::layout::{Layout, Tile};
use crate::model::{RectF, WindowInfo};

pub(super) type Layer = Retained<AnyObject>;

/// `NSPopUpMenuWindowLevel` — above normal, floating and most panels.
const PANEL_LEVEL: isize = 101;
const PANEL_RADIUS: f64 = 20.0;
const TILE_RADIUS: f64 = 14.0;
const THUMB_RADIUS: f64 = 8.0;

/// What one tile shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemView {
    pub wid: u32,
    pub pid: i32,
    pub title: String,
    pub app_path: String,
    /// Window aspect ratio (for the placeholder plate).
    pub aspect: f64,
    pub badge: Option<&'static str>,
    pub dimmed: bool,
}

impl ItemView {
    pub fn from_window(w: &WindowInfo) -> Self {
        let badge = if w.minimized {
            Some("Minimized")
        } else if w.app_hidden {
            Some("Hidden")
        } else if w.fullscreen && w.other_space {
            Some("Full Screen")
        } else if w.other_space {
            Some("Other Desktop")
        } else {
            None
        };
        Self {
            wid: w.id as u32,
            pid: w.pid as i32,
            title: w.display_title().to_string(),
            app_path: w.app_path.clone(),
            aspect: w.aspect(),
            badge,
            dimmed: w.minimized || w.app_hidden,
        }
    }
}

/// Everything needed to draw the overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub items: Vec<ItemView>,
    pub layout: Layout,
    pub selected: usize,
    pub icon_mode: bool,
}

// ------------------------------------------------------------ layer helpers

pub(super) fn nsrect(r: RectF) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

pub(super) unsafe fn layer() -> Layer {
    msg_send![class!(CALayer), layer]
}

pub(super) unsafe fn set_frame(l: &AnyObject, r: RectF) {
    let _: () = msg_send![l, setFrame: nsrect(r)];
}

pub(super) unsafe fn set_bg(l: &AnyObject, c: &OwnedColor) {
    let _: () = msg_send![l, setBackgroundColor: c.0];
}

pub(super) unsafe fn set_radius(l: &AnyObject, r: f64) {
    let _: () = msg_send![l, setCornerRadius: r];
}

pub(super) unsafe fn set_hidden(l: &AnyObject, h: bool) {
    let _: () = msg_send![l, setHidden: h];
}

pub(super) unsafe fn add(parent: &AnyObject, child: &AnyObject) {
    let _: () = msg_send![parent, addSublayer: child];
}

/// Top-left-origin rect → Core Animation's y-up coordinates inside a
/// parent of height `ph`. (We don't use `geometryFlipped`: offscreen
/// `renderInContext:` draws contents upside down under it.)
pub(super) fn flip(r: RectF, ph: f64) -> RectF {
    RectF::new(r.x, ph - r.y - r.h, r.w, r.h)
}

/// Rasterize an `NSImage` (app icon) at `pts` points × `scale`.
pub(super) unsafe fn icon_image(img: &AnyObject, pts: f64, scale: f64) -> Option<Image> {
    let px = (pts * scale).round().max(1.0);
    let mut r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
    let null: *const AnyObject = std::ptr::null();
    let cg: CgImage = msg_send![img, CGImageForProposedRect: &mut r, context: null, hints: null];
    Image::from_borrowed(cg.0)
}

pub(super) unsafe fn set_icon(l: &AnyObject, icon: Option<&AnyObject>, pts: f64, scale: f64) {
    match icon.and_then(|i| icon_image(i, pts, scale)) {
        Some(img) => set_contents_image(l, &img),
        None => {
            let p: *const AnyObject = std::ptr::null();
            let _: () = msg_send![l, setContents: p];
        }
    }
}

pub(super) unsafe fn set_contents_image(l: &AnyObject, img: &Image) {
    let p = img.raw() as *const AnyObject;
    let _: () = msg_send![l, setContents: p];
}

pub(super) unsafe fn system_font(size: f64, weight: f64) -> Retained<AnyObject> {
    msg_send![class!(NSFont), systemFontOfSize: size, weight: weight]
}

/// Width of `s` in `font`.
pub(super) unsafe fn text_width(s: &str, font: &AnyObject) -> f64 {
    let key = NSString::from_str("NSFont");
    let attrs: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionaryWithObject: font, forKey: &*key];
    let ns = NSString::from_str(s);
    let size: NSSize = msg_send![&*ns, sizeWithAttributes: &*attrs];
    size.width.ceil()
}

pub(super) const WEIGHT_REGULAR: f64 = 0.0;
pub(super) const WEIGHT_MEDIUM: f64 = 0.23;
pub(super) const WEIGHT_SEMIBOLD: f64 = 0.3;

pub(super) unsafe fn text_layer(s: &str, font: &AnyObject, size: f64, color: &OwnedColor, align: &str, scale: f64) -> Layer {
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

/// Aspect-fit a `w`×`h` box into `r`, centered.
pub(super) fn fit(r: RectF, w: f64, h: f64) -> RectF {
    if w <= 0.0 || h <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
        return r;
    }
    let k = (r.w / w).min(r.h / h);
    let (fw, fh) = ((w * k).round(), (h * k).round());
    RectF::new((r.x + (r.w - fw) / 2.0).round(), (r.y + (r.h - fh) / 2.0).round(), fw, fh)
}

/// The system accent color (sRGB 0..1).
pub(super) fn accent() -> (f64, f64, f64) {
    let c = NSColor::controlAccentColor();
    match c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) {
        Some(c) => (c.redComponent(), c.greenComponent(), c.blueComponent()),
        None => (0.04, 0.52, 1.0),
    }
}

// ------------------------------------------------------------- layer tree

struct TileLayers {
    wid: u32,
    tile: Tile,
    aspect: f64,
    icon_mode: bool,
    shadow: Layer,
    thumb: Layer,
    plate: Layer,
    big_icon: Layer,
    badge: Option<Layer>,
    title: Layer,
    dimmed: bool,
    /// Panel height, for [`flip`].
    ph: f64,
}

struct Tree {
    ph: f64,
    root: Layer,
    selection: Layer,
    tiles: Vec<TileLayers>,
}

/// Icon providers: real app icons from the pid, else from the bundle path.
pub(super) fn icon_for(pid: i32, path: &str, cache: &mut HashMap<(i32, String), Option<Retained<AnyObject>>>) -> Option<Retained<AnyObject>> {
    cache.entry((pid, path.to_string())).or_insert_with(|| (if pid > 0 { apps::icon(pid) } else { None }).or_else(|| apps::icon_for_path(path))).clone()
}

unsafe fn build_tree(model: &Model, scale: f64, image_for: &dyn Fn(u32) -> Option<Image>) -> Tree {
    let size = model.layout.panel;
    let root = layer();
    let ph = size.h;
    set_frame(&root, RectF::new(0.0, 0.0, size.w, size.h));

    let (ar, ag, ab) = accent();
    let selection = layer();
    set_radius(&selection, TILE_RADIUS);
    set_bg(&selection, &OwnedColor::white(1.0, 0.11));
    let _: () = msg_send![&*selection, setBorderWidth: 2.5f64];
    let _: () = msg_send![&*selection, setBorderColor: OwnedColor::rgba(ar, ag, ab, 1.0).0];
    let _: () = msg_send![&*selection, setShadowColor: OwnedColor::rgba(ar, ag, ab, 1.0).0];
    let _: () = msg_send![&*selection, setShadowOpacity: 0.45f32];
    let _: () = msg_send![&*selection, setShadowRadius: 9.0f64];
    let _: () = msg_send![&*selection, setShadowOffset: NSSize::new(0.0, 0.0)];
    set_hidden(&selection, model.items.is_empty());
    add(&root, &selection);

    if model.items.is_empty() {
        let font = system_font(15.0, WEIGHT_MEDIUM);
        let t = text_layer("No other windows", &font, 15.0, &OwnedColor::white(1.0, 0.72), "center", scale);
        set_frame(&t, flip(RectF::new(0.0, (size.h - 20.0) / 2.0, size.w, 20.0), ph));
        add(&root, &t);
        return Tree { root, selection, tiles: vec![], ph };
    }

    let title_font = system_font(12.5, WEIGHT_MEDIUM);
    let badge_font = system_font(10.0, WEIGHT_SEMIBOLD);
    let mut icons = HashMap::new();
    let mut tiles = Vec::new();
    for (item, tile) in model.items.iter().zip(model.layout.tiles.iter()) {
        let icon = icon_for(item.pid, &item.app_path, &mut icons);
        let icon_ref = icon.as_deref();

        // Thumbnail (+ its shadow), or a placeholder plate with a big icon.
        let shadow = layer();
        let _: () = msg_send![&*shadow, setShadowColor: OwnedColor::white(0.0, 1.0).0];
        let _: () = msg_send![&*shadow, setShadowOpacity: 0.5f32];
        let _: () = msg_send![&*shadow, setShadowRadius: 7.0f64];
        let _: () = msg_send![&*shadow, setShadowOffset: NSSize::new(0.0, -3.0)];
        set_radius(&shadow, THUMB_RADIUS);
        set_bg(&shadow, &OwnedColor::white(0.0, 1.0));
        add(&root, &shadow);

        let thumb = layer();
        set_radius(&thumb, THUMB_RADIUS);
        let _: () = msg_send![&*thumb, setMasksToBounds: true];
        let _: () = msg_send![&*thumb, setBorderWidth: 1.0f64 / scale.max(1.0)];
        let _: () = msg_send![&*thumb, setBorderColor: OwnedColor::white(1.0, 0.14).0];
        let gravity = NSString::from_str("resize");
        let _: () = msg_send![&*thumb, setContentsGravity: &*gravity];
        add(&root, &thumb);

        let plate = layer();
        set_radius(&plate, THUMB_RADIUS);
        set_bg(&plate, &OwnedColor::white(1.0, 0.06));
        let _: () = msg_send![&*plate, setBorderWidth: 1.0f64];
        let _: () = msg_send![&*plate, setBorderColor: OwnedColor::white(1.0, 0.07).0];
        add(&root, &plate);

        let big_icon = layer();
        let big_pts = if model.icon_mode { tile.thumb.h.min(tile.thumb.w) } else { (tile.thumb.h * 0.56).min(96.0) };
        set_icon(&big_icon, icon_ref, big_pts, scale);
        let g = NSString::from_str("resizeAspect");
        let _: () = msg_send![&*big_icon, setContentsGravity: &*g];
        let _: () = msg_send![&*big_icon, setContentsScale: scale];
        add(&root, &big_icon);

        // Label row: small icon + title (thumbnail mode) or centered title (icon mode).
        let label = tile.label;
        let title_color = OwnedColor::white(1.0, 0.9);
        let line_h = 16.0;
        let ty = label.y + ((label.h - line_h) / 2.0).round();
        let title = if model.icon_mode {
            let t = text_layer(&item.title, &title_font, 12.5, &title_color, "center", scale);
            set_frame(&t, flip(RectF::new(label.x, ty, label.w, line_h), ph));
            t
        } else {
            let isz = 18.0;
            let ic = layer();
            set_icon(&ic, icon_ref, isz, scale);
            let _: () = msg_send![&*ic, setContentsGravity: &*g];
            let _: () = msg_send![&*ic, setContentsScale: scale];
            // Center icon + title as a group when the title is short.
            let tw = text_width(&item.title, &title_font).min(label.w - isz - 6.0);
            let group = isz + 6.0 + tw;
            let x0 = (label.x + (label.w - group) / 2.0).round().max(label.x);
            set_frame(&ic, flip(RectF::new(x0, label.y + ((label.h - isz) / 2.0).round(), isz, isz), ph));
            add(&root, &ic);
            let t = text_layer(&item.title, &title_font, 12.5, &title_color, "left", scale);
            set_frame(&t, flip(RectF::new(x0 + isz + 6.0, ty, (label.right() - (x0 + isz + 6.0)).max(0.0), line_h), ph));
            t
        };
        add(&root, &title);

        let badge = item.badge.map(|text| {
            let tw = text_width(text, &badge_font);
            let pill = layer();
            set_radius(&pill, 8.0);
            set_bg(&pill, &OwnedColor::white(0.0, 0.62));
            let _: () = msg_send![&*pill, setBorderWidth: 0.5f64];
            let _: () = msg_send![&*pill, setBorderColor: OwnedColor::white(1.0, 0.18).0];
            let t = text_layer(text, &badge_font, 10.0, &OwnedColor::white(1.0, 0.92), "center", scale);
            set_frame(&t, RectF::new(0.0, 1.5, tw + 14.0, 13.0));
            add(&pill, &t);
            let _: () = msg_send![&*pill, setBounds: nsrect(RectF::new(0.0, 0.0, tw + 14.0, 17.0))];
            add(&root, &pill);
            pill
        });

        let mut tl = TileLayers {
            wid: item.wid,
            tile: *tile,
            aspect: item.aspect,
            icon_mode: model.icon_mode,
            shadow,
            thumb,
            plate,
            big_icon,
            badge,
            title,
            dimmed: item.dimmed,
            ph,
        };
        let img = if model.icon_mode { None } else { image_for(item.wid) };
        tl.set_image(img.as_ref());
        tiles.push(tl);
    }
    let tree = Tree { root, selection, tiles, ph };
    tree.select(model.selected);
    tree
}

impl TileLayers {
    /// Show `img` as the thumbnail, or the placeholder when `None`.
    unsafe fn set_image(&mut self, img: Option<&Image>) {
        let area = self.tile.thumb;
        let opacity: f32 = if self.dimmed { 0.6 } else { 1.0 };
        let content_rect = match img {
            Some(img) => {
                let (w, h) = img.size();
                let r = fit(area, w, h);
                set_contents_image(&self.thumb, img);
                set_frame(&self.thumb, flip(r, self.ph));
                set_frame(&self.shadow, flip(r.inset(1.0), self.ph));
                let _: () = msg_send![&*self.thumb, setOpacity: opacity];
                let _: () = msg_send![&*self.shadow, setOpacity: opacity];
                set_hidden(&self.thumb, false);
                set_hidden(&self.shadow, false);
                set_hidden(&self.plate, true);
                set_hidden(&self.big_icon, true);
                r
            }
            None => {
                set_hidden(&self.thumb, true);
                set_hidden(&self.shadow, true);
                let r = if self.icon_mode {
                    let s = area.w.min(area.h);
                    RectF::new((area.x + (area.w - s) / 2.0).round(), area.y, s, s)
                } else {
                    fit(area, self.aspect, 1.0)
                };
                // The plate only in thumbnail mode (a stand-in for the window).
                set_hidden(&self.plate, self.icon_mode);
                set_frame(&self.plate, flip(r, self.ph));
                let s = if self.icon_mode { r.h } else { (r.h * 0.56).min(r.w * 0.56).min(96.0) };
                let ir = RectF::new((r.x + (r.w - s) / 2.0).round(), (r.y + (r.h - s) / 2.0).round(), s, s);
                set_frame(&self.big_icon, flip(ir, self.ph));
                set_hidden(&self.big_icon, false);
                let _: () = msg_send![&*self.big_icon, setOpacity: opacity];
                r
            }
        };
        if let Some(b) = &self.badge {
            let bounds: NSRect = msg_send![&**b, bounds];
            let (bw, bh) = (bounds.size.width, bounds.size.height);
            let cr = if self.icon_mode { area } else { content_rect };
            set_frame(b, flip(RectF::new(cr.right() - bw - 6.0, cr.y + 6.0, bw, bh), self.ph));
        }
    }
}

impl Tree {
    unsafe fn select(&self, i: usize) {
        if let Some(t) = self.tiles.get(i) {
            set_frame(&self.selection, flip(t.tile.frame, self.ph));
            set_hidden(&self.selection, false);
        }
        for (j, t) in self.tiles.iter().enumerate() {
            let c = if j == i { OwnedColor::white(1.0, 1.0) } else { OwnedColor::white(1.0, 0.82) };
            let _: () = msg_send![&*t.title, setForegroundColor: c.0];
        }
    }
}

// ------------------------------------------------------------- the panel

struct Overlay {
    panel: Retained<NSPanel>,
    canvas: Retained<NSView>,
    effect: Retained<NSVisualEffectView>,
    tree: Option<Tree>,
    model: Option<Model>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

pub(super) fn primary_height() -> f64 {
    unsafe { CGDisplayBounds(CGMainDisplayID()).size.height }
}

pub(super) fn to_cocoa(r: RectF) -> NSRect {
    NSRect::new(NSPoint::new(r.x, primary_height() - (r.y + r.h)), NSSize::new(r.w, r.h))
}

pub(super) fn backing_scale(mtm: MainThreadMarker, r: RectF) -> f64 {
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

fn make_overlay(mtm: MainThreadMarker) -> Overlay {
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
    // Mouse hover / clicks are handled by the event tap.
    panel.setIgnoresMouseEvents(true);
    panel.setLevel(PANEL_LEVEL);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    unsafe {
        // NSWindowAnimationBehaviorNone: appear instantly.
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
            let _: () = msg_send![&*l, setBorderWidth: 1.0f64];
            let _: () = msg_send![&*l, setBorderColor: OwnedColor::white(1.0, 0.13).0];
            // A touch darker than the stock HUD for contrast with thumbnails.
            set_bg(&l, &OwnedColor::rgba(0.07, 0.07, 0.08, 0.30));
        }
    }
    let canvas = NSView::initWithFrame(NSView::alloc(mtm), frame);
    canvas.setWantsLayer(true);
    effect.addSubview(&canvas);
    panel.setContentView(Some(&effect));
    Overlay { panel, canvas, effect, tree: None, model: None }
}

/// Show (or rebuild) the overlay for `model`.
pub fn show(mtm: MainThreadMarker, model: Model) {
    autoreleasepool(|_| {
        OVERLAY.with(|cell| {
            let mut cell = cell.borrow_mut();
            let ov = cell.get_or_insert_with(|| make_overlay(mtm));
            let panel_rect = model.layout.panel;
            let scale = backing_scale(mtm, panel_rect);
            unsafe {
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                if let Some(old) = ov.tree.take() {
                    let _: () = msg_send![&*old.root, removeFromSuperlayer];
                }
                let local = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_rect.w, panel_rect.h));
                ov.panel.setFrame_display(to_cocoa(panel_rect), false);
                ov.effect.setFrame(local);
                ov.canvas.setFrame(local);
                let tree = build_tree(&model, scale, &|wid| thumbs::cached(wid));
                let host: Option<Layer> = msg_send![&*ov.canvas, layer];
                if let Some(host) = host {
                    add(&host, &tree.root);
                }
                let _: () = msg_send![class!(CATransaction), commit];
                ov.tree = Some(tree);
            }
            ov.model = Some(model);
            ov.panel.orderFrontRegardless();
            ov.panel.invalidateShadow();
        })
    })
}

/// Move the highlight to tile `i` (animated).
pub fn select(_mtm: MainThreadMarker, i: usize) {
    OVERLAY.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(ov) = cell.as_mut() else { return };
        if let Some(m) = ov.model.as_mut() {
            m.selected = i;
        }
        let Some(tree) = &ov.tree else { return };
        unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setAnimationDuration: 0.12f64];
            tree.select(i);
            let _: () = msg_send![class!(CATransaction), commit];
        }
    })
}

/// A thumbnail arrived: swap it in if the window is on screen.
pub fn set_thumbnail(_mtm: MainThreadMarker, wid: u32, img: Image) {
    OVERLAY.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(ov) = cell.as_mut() else { return };
        let Some(tree) = ov.tree.as_mut() else { return };
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

pub fn hide(mtm: MainThreadMarker) {
    super::spotlight::hide(mtm);
    OVERLAY.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(ov) = cell.as_mut() else { return };
        ov.panel.orderOut(None);
        if let Some(old) = ov.tree.take() {
            unsafe {
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![&*old.root, removeFromSuperlayer];
                let _: () = msg_send![class!(CATransaction), commit];
            }
        }
        ov.model = None;
    })
}

pub fn destroy(mtm: MainThreadMarker) {
    super::spotlight::destroy(mtm);
    OVERLAY.with(|cell| {
        if let Some(ov) = cell.borrow_mut().take() {
            ov.panel.orderOut(None);
            ov.panel.close();
        }
    })
}

// ----------------------------------------------------- offscreen rendering

/// Render the live overlay's layer tree (if shown) to a PNG — a debugging
/// aid that needs no screen capture. Main thread only.
pub fn render_live_png(path: &str) -> Result<bool> {
    let model = OVERLAY.with(|c| c.borrow().as_ref().and_then(|o| o.model.clone()));
    let Some(model) = model else { return Ok(false) };
    let images: HashMap<u32, Image> = model.items.iter().filter_map(|i| thumbs::cached(i.wid).map(|img| (i.wid, img))).collect();
    render_png(&model, &images, path, 2.0)?;
    Ok(true)
}

/// Render `model` to a PNG (for visual checks without screenshots).
/// Vibrancy can't be captured offscreen, so the HUD is approximated by a
/// translucent dark rounded panel over a colorful backdrop. `images`
/// supplies thumbnails (window id → image).
pub fn render_png(model: &Model, images: &HashMap<u32, Image>, path: &str, scale: f64) -> Result<()> {
    let margin = 56.0;
    let p = model.layout.panel;
    let (w, h) = (p.w + 2.0 * margin, p.h + 2.0 * margin);
    autoreleasepool(|_| unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: true];
        let root = layer();
        set_frame(&root, RectF::new(0.0, 0.0, w, h));
        add(&root, &wallpaper(w, h));
        add_hud(&root, model, RectF::new(margin, margin, p.w, p.h), scale, &|wid| images.get(&wid).cloned());
        let _: () = msg_send![class!(CATransaction), commit];
        write_layer_png(&root, w, h, scale, path)
    })
}

/// A soft wallpaper-like gradient (offscreen renders).
pub(super) unsafe fn wallpaper(w: f64, h: f64) -> Layer {
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
    let _: () = msg_send![&*grad, setStartPoint: NSPoint::new(0.0, 0.0)];
    let _: () = msg_send![&*grad, setEndPoint: NSPoint::new(1.0, 1.0)];
    grad
}

/// The HUD (a stand-in for the vibrant material, which can't be captured
/// offscreen) with its tiles, at `r` in `parent`'s y-up coordinates.
pub(super) unsafe fn add_hud(parent: &AnyObject, model: &Model, r: RectF, scale: f64, image_for: &dyn Fn(u32) -> Option<Image>) {
    let panel = layer();
    set_frame(&panel, r);
    set_radius(&panel, PANEL_RADIUS);
    set_bg(&panel, &OwnedColor::rgba(0.11, 0.11, 0.13, 0.88));
    let _: () = msg_send![&*panel, setBorderWidth: 1.0f64];
    let _: () = msg_send![&*panel, setBorderColor: OwnedColor::white(1.0, 0.13).0];
    let _: () = msg_send![&*panel, setShadowOpacity: 0.45f32];
    let _: () = msg_send![&*panel, setShadowRadius: 24.0f64];
    let _: () = msg_send![&*panel, setShadowOffset: NSSize::new(0.0, -10.0)];
    add(parent, &panel);
    let tree = build_tree(model, scale, image_for);
    set_frame(&tree.root, r);
    add(parent, &tree.root);
}

/// Rasterize a layer tree (`w`×`h` points) to a PNG file.
pub(super) unsafe fn write_layer_png(root: &AnyObject, w: f64, h: f64, scale: f64, path: &str) -> Result<()> {
    let (pw, ph) = ((w * scale) as usize, (h * scale) as usize);
    let space = CGColorSpaceCreateDeviceRGB();
    // kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big
    let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, 0, space, 1 | (4 << 12));
    CGColorSpaceRelease(space);
    if ctx.is_null() {
        bail!("CGBitmapContextCreate failed");
    }
    CGContextScaleCTM(ctx, scale, scale);
    let _: () = msg_send![root, renderInContext: CgContext(ctx)];
    let img = CGBitmapContextCreateImage(ctx);
    CGContextRelease(ctx);
    let Some(img) = Image::from_owned(img) else { bail!("CGBitmapContextCreateImage failed") };
    let rep: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
    let rep: Retained<AnyObject> = msg_send![rep, initWithCGImage: CgImage(img.raw())];
    let props: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
    // NSBitmapImageFileTypePNG = 4
    let data: Option<Retained<AnyObject>> = msg_send![&*rep, representationUsingType: 4usize, properties: &*props];
    let Some(data) = data else { bail!("PNG encoding failed") };
    let ns_path = NSString::from_str(path);
    let ok: bool = msg_send![&*data, writeToFile: &*ns_path, atomically: true];
    if !ok {
        bail!("could not write {path}");
    }
    Ok(())
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
        // Title bar (CG origin is bottom-left).
        let tb = hf * 0.085;
        CGContextSetRGBFillColor(ctx, bar, bar, bar, 1.0);
        CGContextFillRect(ctx, CGRect::new(0.0, hf - tb, wf, tb));
        // Traffic lights.
        for (i, c) in [(1.0, 0.37, 0.34), (1.0, 0.74, 0.18), (0.16, 0.79, 0.25)].iter().enumerate() {
            CGContextSetRGBFillColor(ctx, c.0, c.1, c.2, 1.0);
            let d = tb * 0.42;
            CGContextFillRect(ctx, CGRect::new(tb * 0.5 + i as f64 * d * 1.6, hf - tb / 2.0 - d / 2.0, d, d));
        }
        // Sidebar.
        CGContextSetRGBFillColor(ctx, tint.0, tint.1, tint.2, if dark { 0.35 } else { 0.22 });
        CGContextFillRect(ctx, CGRect::new(0.0, 0.0, wf * 0.24, hf - tb));
        // Header block + text lines.
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

/// Weight constants are used above; keep the regular one referenced.
#[allow(dead_code)]
const _W: f64 = WEIGHT_REGULAR;
