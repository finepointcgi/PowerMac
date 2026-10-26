//! "Preview selected window": while the switcher is open, the selected
//! window's image is drawn at its real position and size (or centered with
//! a badge when it can't be seen where it is), above every other window and
//! just below the HUD, with the rest of the screen slightly dimmed.
//!
//! It's a borderless, non-activating, click-through `NSPanel` covering one
//! screen; the content is a handful of Core Animation layers rebuilt on
//! every selection change (cheap: ≤ 10 layers). The image is the cached
//! thumbnail first, replaced by a full-resolution ScreenCaptureKit capture
//! as soon as it arrives. Without Screen Recording it's an outline of the
//! window's frame with the app icon and title. Main thread only.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSPanel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::ffi::*;
use super::overlay::{
    self, accent, add, fit, flip, icon_for, layer, set_bg, set_contents_image, set_frame, set_icon, set_radius,
    system_font, text_layer, text_width, to_cocoa, Layer, Model, WEIGHT_MEDIUM, WEIGHT_SEMIBOLD,
};
use super::thumbs;
use crate::model::RectF;
use crate::placement::{self, Placement};

/// Just below the HUD (`NSPopUpMenuWindowLevel` = 101), above the menu
/// bar, the Dock and every app window.
const PREVIEW_LEVEL: isize = 100;

/// What's on screen (for tests / diagnostics; readable from any thread):
/// window id, at its real position, badge.
static SHOWN: std::sync::Mutex<Option<(u32, bool, Option<&'static str>)>> = std::sync::Mutex::new(None);

/// The preview on screen right now: (window id, at its real position, badge).
pub fn shown() -> Option<(u32, bool, Option<&'static str>)> {
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_shown(v: Option<(u32, bool, Option<&'static str>)>) {
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

/// What the preview shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewView {
    pub wid: u32,
    pub pid: i32,
    pub title: String,
    pub app_path: String,
    pub placement: Placement,
    /// Dim the rest of the screen this much (0..1).
    pub dim: Option<f64>,
    /// No Screen Recording: draw an outline with the app icon and title.
    pub outline: bool,
    /// The HUD's frame (global): labels are kept out from under it.
    pub avoid: Option<RectF>,
}

/// Corner radius of macOS windows (bigger since macOS 26).
fn window_radius() -> f64 {
    static R: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        #[repr(C)]
        struct Ver {
            major: isize,
            minor: isize,
            patch: isize,
        }
        unsafe impl objc2::encode::Encode for Ver {
            const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct(
                "?",
                &[isize::ENCODING, isize::ENCODING, isize::ENCODING],
            );
        }
        let v: Ver = unsafe {
            let pi: Retained<AnyObject> = msg_send![class!(NSProcessInfo), processInfo];
            msg_send![&*pi, operatingSystemVersion]
        };
        let _ = (v.minor, v.patch);
        if v.major >= 26 {
            16.0
        } else {
            10.0
        }
    })
}

struct Tree {
    root: Layer,
    wid: u32,
    /// Pixel width of the image shown (0 = placeholder), to only upgrade.
    image_w: f64,
}

/// Build the preview's layers for a `size` (points) canvas whose origin is
/// the placement's screen origin.
unsafe fn build(view: &PreviewView, img: Option<&Image>, size: (f64, f64), scale: f64) -> Tree {
    let (w, h) = size;
    let root = layer();
    set_frame(&root, RectF::new(0.0, 0.0, w, h));
    let p = &view.placement;
    let local = p.frame.offset(-p.screen.x, -p.screen.y);
    let radius = window_radius();

    if let Some(d) = view.dim {
        let dim = layer();
        set_frame(&dim, RectF::new(0.0, 0.0, w, h));
        set_bg(&dim, &OwnedColor::rgba(0.0, 0.0, 0.0, d));
        add(&root, &dim);
    }

    let img = if view.outline { None } else { img };
    // The image keeps its own aspect when centered; at the real position it
    // fills the window's frame exactly.
    let rect = match img {
        Some(i) if !p.real => {
            let (iw, ih) = i.size();
            fit(local, iw, ih)
        }
        _ => local,
    };

    // Lift: a soft, wide shadow plus a tight contact shadow (under the
    // opaque image only; a translucent plate would show them through).
    let shadows: &[(f32, f64, f64)] = if img.is_some() { &[(0.42, 38.0, -18.0), (0.30, 6.0, -2.0)] } else { &[] };
    for &(opacity, blur, dy) in shadows {
        let s = layer();
        set_frame(&s, flip(rect.inset(2.0), h));
        set_radius(&s, radius);
        set_bg(&s, &OwnedColor::white(0.0, 1.0));
        let _: () = msg_send![&*s, setShadowColor: OwnedColor::white(0.0, 1.0).0];
        let _: () = msg_send![&*s, setShadowOpacity: opacity];
        let _: () = msg_send![&*s, setShadowRadius: blur];
        let _: () = msg_send![&*s, setShadowOffset: NSSize::new(0.0, dy)];
        add(&root, &s);
    }

    let mut icons = HashMap::new();
    let icon = icon_for(view.pid, &view.app_path, &mut icons);
    let image_w = match img {
        Some(i) => {
            let l = layer();
            set_frame(&l, flip(rect, h));
            set_radius(&l, radius);
            let _: () = msg_send![&*l, setMasksToBounds: true];
            let g = NSString::from_str("resize");
            let _: () = msg_send![&*l, setContentsGravity: &*g];
            set_contents_image(&l, i);
            add(&root, &l);
            // Hairline edge so dark windows don't melt into the dimming.
            let edge = layer();
            set_frame(&edge, flip(rect, h));
            set_radius(&edge, radius);
            let _: () = msg_send![&*edge, setBorderWidth: 1.0f64 / scale.max(1.0)];
            let _: () = msg_send![&*edge, setBorderColor: OwnedColor::white(1.0, 0.22).0];
            add(&root, &edge);
            i.size().0
        }
        None => {
            placeholder(&root, view, rect, h, radius, icon.as_deref(), scale);
            0.0
        }
    };

    if let Some(b) = p.badge {
        let screen_local = RectF::new(0.0, 0.0, w, h);
        badge(&root, b.label(), rect, screen_local, h, icon.as_deref(), scale);
    }
    Tree { root, wid: view.wid, image_w }
}

/// Outline / loading plate: translucent dark fill, a dashed accent outline
/// (no Screen Recording) or a hairline (image still loading), the app icon
/// and the title.
unsafe fn placeholder(root: &AnyObject, view: &PreviewView, r: RectF, ph: f64, radius: f64, icon: Option<&AnyObject>, scale: f64) {
    let plate = layer();
    set_frame(&plate, flip(r, ph));
    set_radius(&plate, radius);
    set_bg(&plate, &OwnedColor::rgba(0.10, 0.10, 0.12, if view.outline { 0.80 } else { 0.78 }));
    add(root, &plate);

    let (ar, ag, ab) = accent();
    if view.outline {
        // Accent glow + dashed outline of the window's frame.
        let glow = layer();
        set_frame(&glow, flip(r, ph));
        set_radius(&glow, radius);
        let _: () = msg_send![&*glow, setBorderWidth: 1.0f64];
        let _: () = msg_send![&*glow, setBorderColor: OwnedColor::rgba(ar, ag, ab, 0.35).0];
        let _: () = msg_send![&*glow, setShadowColor: OwnedColor::rgba(ar, ag, ab, 1.0).0];
        let _: () = msg_send![&*glow, setShadowOpacity: 0.55f32];
        let _: () = msg_send![&*glow, setShadowRadius: 14.0f64];
        let _: () = msg_send![&*glow, setShadowOffset: NSSize::new(0.0, 0.0)];
        add(root, &glow);
        let shape: Layer = msg_send![class!(CAShapeLayer), layer];
        let fr = flip(r, ph);
        set_frame(&shape, fr);
        let inset = 1.5;
        let path = CGPathCreateWithRoundedRect(
            CGRect::new(inset, inset, (fr.w - 2.0 * inset).max(0.0), (fr.h - 2.0 * inset).max(0.0)),
            (radius - inset).max(0.0),
            (radius - inset).max(0.0),
            std::ptr::null(),
        );
        let _: () = msg_send![&*shape, setPath: CgPath(path)];
        CGPathRelease(path);
        let _: () = msg_send![&*shape, setFillColor: OwnedColor::white(0.0, 0.0).0];
        let _: () = msg_send![&*shape, setStrokeColor: OwnedColor::rgba(ar, ag, ab, 0.95).0];
        let _: () = msg_send![&*shape, setLineWidth: 3.0f64];
        let dash: Retained<AnyObject> = msg_send![class!(NSMutableArray), array];
        for v in [12.0f64, 7.0] {
            let n: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithDouble: v];
            let _: () = msg_send![&*dash, addObject: &*n];
        }
        let _: () = msg_send![&*shape, setLineDashPattern: &*dash];
        let _: () = msg_send![&*shape, setContentsScale: scale];
        add(root, &shape);
    } else {
        let _: () = msg_send![&*plate, setBorderWidth: 1.0f64];
        let _: () = msg_send![&*plate, setBorderColor: OwnedColor::white(1.0, 0.16).0];
    }

    // Icon + title (+ a hint without Screen Recording), centered as a group
    // where the HUD doesn't cover it; the icon shrinks to fit a narrow band.
    let hint = view.outline.then_some("Allow Screen Recording to see the window here");
    let avoid = view.avoid.map(|a| a.offset(-view.placement.screen.x, -view.placement.screen.y));
    let group = |isz: f64| {
        let title_size = if isz >= 96.0 { 18.0 } else { 14.0 };
        isz + 14.0 + title_size + 6.0 + if hint.is_some() { 22.0 } else { 0.0 }
    };
    if r.w < 160.0 {
        return;
    }
    let base = (r.w.min(r.h) * 0.30).clamp(40.0, 128.0).round();
    let fitted = [base, 72.0, 44.0]
        .into_iter()
        .filter(|&s| s <= base)
        .find_map(|s| placement::label_top(r, avoid, group(s)).map(|top| (s, top)));
    let Some((isz, top)) = fitted.or_else(|| {
        // Nowhere visible: centered (partly under the HUD) if it fits at all.
        placement::label_top(r, None, group(base)).map(|t| (base, t))
    }) else {
        return;
    };
    let title_size = if isz >= 96.0 { 18.0 } else { 14.0 };
    let ic = layer();
    set_icon(&ic, icon, isz, scale);
    let g = NSString::from_str("resizeAspect");
    let _: () = msg_send![&*ic, setContentsGravity: &*g];
    let _: () = msg_send![&*ic, setContentsScale: scale];
    set_frame(&ic, flip(RectF::new((r.x + (r.w - isz) / 2.0).round(), top, isz, isz), ph));
    let _: () = msg_send![&*ic, setShadowOpacity: 0.35f32];
    let _: () = msg_send![&*ic, setShadowRadius: 8.0f64];
    let _: () = msg_send![&*ic, setShadowOffset: NSSize::new(0.0, -3.0)];
    add(root, &ic);
    let font = system_font(title_size, WEIGHT_SEMIBOLD);
    let ty = top + isz + 14.0;
    let t = text_layer(&view.title, &font, title_size, &OwnedColor::white(1.0, 0.95), "center", scale);
    set_frame(&t, flip(RectF::new(r.x + 24.0, ty, (r.w - 48.0).max(0.0), title_size + 6.0), ph));
    add(root, &t);
    if let Some(hint) = hint {
        let f = system_font(12.0, WEIGHT_MEDIUM);
        let l = text_layer(hint, &f, 12.0, &OwnedColor::white(1.0, 0.62), "center", scale);
        set_frame(&l, flip(RectF::new(r.x + 24.0, ty + title_size + 12.0, (r.w - 48.0).max(0.0), 16.0), ph));
        add(root, &l);
    }
}

/// A pill at the top center of the preview: app icon + "Minimized" etc.
unsafe fn badge(root: &AnyObject, text: &str, r: RectF, screen: RectF, ph: f64, icon: Option<&AnyObject>, scale: f64) {
    let font = system_font(13.0, WEIGHT_SEMIBOLD);
    let tw = text_width(text, &font);
    let (isz, pad, gap, bh) = (18.0, 12.0, 7.0, 30.0);
    let bw = pad + isz + gap + tw + pad + 2.0;
    let x = (r.x + (r.w - bw) / 2.0).round();
    let y = placement::badge_top(r, screen, bh);
    let pill = layer();
    set_frame(&pill, flip(RectF::new(x, y, bw, bh), ph));
    set_radius(&pill, bh / 2.0);
    set_bg(&pill, &OwnedColor::rgba(0.08, 0.08, 0.10, 0.78));
    let _: () = msg_send![&*pill, setBorderWidth: 0.5f64];
    let _: () = msg_send![&*pill, setBorderColor: OwnedColor::white(1.0, 0.22).0];
    let _: () = msg_send![&*pill, setShadowOpacity: 0.35f32];
    let _: () = msg_send![&*pill, setShadowRadius: 10.0f64];
    let _: () = msg_send![&*pill, setShadowOffset: NSSize::new(0.0, -3.0)];
    add(root, &pill);
    let ic = layer();
    set_icon(&ic, icon, isz, scale);
    let g = NSString::from_str("resizeAspect");
    let _: () = msg_send![&*ic, setContentsGravity: &*g];
    let _: () = msg_send![&*ic, setContentsScale: scale];
    set_frame(&ic, flip(RectF::new(x + pad, y + (bh - isz) / 2.0, isz, isz), ph));
    add(root, &ic);
    let t = text_layer(text, &font, 13.0, &OwnedColor::white(1.0, 0.95), "left", scale);
    set_frame(&t, flip(RectF::new(x + pad + isz + gap, y + (bh - 17.0) / 2.0, tw + 4.0, 17.0), ph));
    add(root, &t);
}

// ------------------------------------------------------------- the panel

struct Panel {
    panel: Retained<NSPanel>,
    canvas: Retained<NSView>,
    tree: Option<Tree>,
    view: Option<PreviewView>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn make_panel(mtm: MainThreadMarker) -> Panel {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
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
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setLevel(PREVIEW_LEVEL);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    unsafe {
        let _: () = msg_send![&*panel, setAnimationBehavior: 2isize];
        // Never in screenshots / screen sharing of other apps, and never
        // captured into our own thumbnails.
        let _: () = msg_send![&*panel, setSharingType: 0usize];
    }
    let canvas = NSView::initWithFrame(NSView::alloc(mtm), frame);
    canvas.setWantsLayer(true);
    panel.setContentView(Some(&canvas));
    Panel { panel, canvas, tree: None, view: None }
}

unsafe fn install(p: &mut Panel, view: &PreviewView, img: Option<&Image>, scale: f64) {
    let _: () = msg_send![class!(CATransaction), begin];
    let _: () = msg_send![class!(CATransaction), setDisableActions: true];
    if let Some(old) = p.tree.take() {
        let _: () = msg_send![&*old.root, removeFromSuperlayer];
    }
    let s = view.placement.screen;
    let tree = build(view, img, (s.w, s.h), scale);
    let host: Option<Layer> = msg_send![&*p.canvas, layer];
    if let Some(host) = host {
        add(&host, &tree.root);
    }
    let _: () = msg_send![class!(CATransaction), commit];
    p.tree = Some(tree);
}

/// Show (or move) the preview for `view`.
pub fn show(mtm: MainThreadMarker, view: PreviewView) {
    autoreleasepool(|_| {
        PANEL.with(|cell| {
            let mut cell = cell.borrow_mut();
            let p = cell.get_or_insert_with(|| make_panel(mtm));
            let s = view.placement.screen;
            let scale = overlay::backing_scale(mtm, s);
            if p.view.as_ref().map(|v| v.placement.screen) != Some(s) {
                let local = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(s.w, s.h));
                p.panel.setFrame_display(to_cocoa(s), false);
                p.canvas.setFrame(local);
            }
            let img = thumbs::best(view.wid);
            unsafe { install(p, &view, img.as_ref(), scale) };
            set_shown(Some((view.wid, view.placement.real, view.placement.badge.map(|b| b.label()))));
            p.view = Some(view);
            p.panel.orderFrontRegardless();
        })
    })
}

/// A capture arrived: show it if it's for the previewed window and sharper
/// than what's shown.
pub fn set_image(mtm: MainThreadMarker, wid: u32, img: Image) {
    autoreleasepool(|_| {
        PANEL.with(|cell| {
            let mut cell = cell.borrow_mut();
            let Some(p) = cell.as_mut() else { return };
            let Some(view) = p.view.clone() else { return };
            if view.wid != wid || view.outline || p.tree.as_ref().is_some_and(|t| t.wid == wid && t.image_w >= img.size().0) {
                return;
            }
            let scale = overlay::backing_scale(mtm, view.placement.screen);
            unsafe { install(p, &view, Some(&img), scale) };
        })
    })
}

/// The previewed window, if any.
pub fn current() -> Option<u32> {
    PANEL.with(|c| c.borrow().as_ref().and_then(|p| p.view.as_ref().map(|v| v.wid)))
}

pub fn hide(_mtm: MainThreadMarker) {
    PANEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(p) = cell.as_mut() else { return };
        p.panel.orderOut(None);
        set_shown(None);
        if let Some(old) = p.tree.take() {
            unsafe {
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![&*old.root, removeFromSuperlayer];
                let _: () = msg_send![class!(CATransaction), commit];
            }
        }
        p.view = None;
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

// ----------------------------------------------------- offscreen rendering

/// A fake desktop window for [`render_scene_png`].
pub struct DesktopWindow {
    /// Global frame (top-left origin).
    pub frame: RectF,
    pub image: Image,
}

/// Render a whole screen offscreen: wallpaper, a menu bar, `desktop`
/// windows, then the preview (dimming + selected window) and the HUD —
/// what the user sees while the switcher is open.
pub fn render_scene_png(
    model: &Model,
    images: &HashMap<u32, Image>,
    preview: Option<(&PreviewView, Option<&Image>)>,
    desktop: &[DesktopWindow],
    screen: RectF,
    path: &str,
    scale: f64,
) -> Result<()> {
    let (w, h) = (screen.w, screen.h);
    autoreleasepool(|_| unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: true];
        let root = layer();
        set_frame(&root, RectF::new(0.0, 0.0, w, h));
        add(&root, &overlay::wallpaper(w, h));
        let radius = window_radius();
        for d in desktop {
            let r = flip(d.frame.offset(-screen.x, -screen.y), h);
            let s = layer();
            set_frame(&s, r);
            set_radius(&s, radius);
            set_bg(&s, &OwnedColor::white(0.0, 1.0));
            let _: () = msg_send![&*s, setShadowOpacity: 0.38f32];
            let _: () = msg_send![&*s, setShadowRadius: 22.0f64];
            let _: () = msg_send![&*s, setShadowOffset: NSSize::new(0.0, -10.0)];
            add(&root, &s);
            let l = layer();
            set_frame(&l, r);
            set_radius(&l, radius);
            let _: () = msg_send![&*l, setMasksToBounds: true];
            set_contents_image(&l, &d.image);
            add(&root, &l);
        }
        // Menu bar.
        let bar = layer();
        set_frame(&bar, flip(RectF::new(0.0, 0.0, w, 24.0), h));
        set_bg(&bar, &OwnedColor::white(1.0, 0.18));
        add(&root, &bar);
        if let Some((view, img)) = preview {
            let tree = build(view, img, (view.placement.screen.w, view.placement.screen.h), scale);
            let ps = view.placement.screen;
            set_frame(&tree.root, flip(RectF::new(ps.x - screen.x, ps.y - screen.y, ps.w, ps.h), h));
            add(&root, &tree.root);
        }
        let p = model.layout.panel;
        let hud = flip(p.offset(-screen.x, -screen.y), h);
        overlay::add_hud(&root, model, hud, scale, &|wid| images.get(&wid).cloned());
        let _: () = msg_send![class!(CATransaction), commit];
        overlay::write_layer_png(&root, w, h, scale, path)
    })
}
