//! Core Animation / AppKit drawing helpers (main thread).

use std::collections::HashMap;
use std::ffi::c_void;

use objc2::encode::{Encode, Encoding};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSSize};

use super::ffi::*;
pub use super::ffi::Image;
use crate::geometry::Rect;

pub type Layer = Retained<AnyObject>;

pub fn nsrect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AffineTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

unsafe impl Encode for AffineTransform {
    const ENCODING: Encoding = Encoding::Struct("CGAffineTransform", &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING]);
}

impl AffineTransform {
    pub fn scale(s: f64) -> Self {
        AffineTransform { a: s, b: 0.0, c: 0.0, d: s, tx: 0.0, ty: 0.0 }
    }
}

pub fn layer() -> Layer {
    unsafe { msg_send![class!(CALayer), layer] }
}

pub fn shape_layer() -> Layer {
    unsafe { msg_send![class!(CAShapeLayer), layer] }
}

pub fn text_layer(scale: f64) -> Layer {
    let l: Layer = unsafe { msg_send![class!(CATextLayer), layer] };
    unsafe {
        let _: () = msg_send![&*l, setContentsScale: scale];
    }
    l
}

pub fn add(parent: &AnyObject, child: &AnyObject) {
    unsafe {
        let _: () = msg_send![parent, addSublayer: child];
    }
}

pub fn remove(l: &AnyObject) {
    unsafe {
        let _: () = msg_send![l, removeFromSuperlayer];
    }
}

/// Set a frame given in top-left coordinates of the superlayer (the layer
/// tree itself is y-up and never `geometryFlipped`, so on-screen and
/// offscreen renders agree). The superlayer's height must already be set.
pub fn set_frame(l: &AnyObject, r: Rect) {
    unsafe {
        let sup: Option<Retained<AnyObject>> = msg_send![l, superlayer];
        let ph = match sup {
            Some(s) => {
                let b: NSRect = msg_send![&*s, bounds];
                b.size.height
            }
            None => r.y + r.h,
        };
        let up = Rect::new(r.x, ph - (r.y + r.h), r.w, r.h);
        let _: () = msg_send![l, setFrame: nsrect(up)];
    }
}

pub fn set_hidden(l: &AnyObject, h: bool) {
    unsafe {
        let _: () = msg_send![l, setHidden: h];
    }
}

pub fn set_opacity(l: &AnyObject, o: f64) {
    unsafe {
        let _: () = msg_send![l, setOpacity: o.clamp(0.0, 1.0) as f32];
    }
}

pub fn set_bg(l: &AnyObject, c: Option<&Color>) {
    let p = c.map(|c| c.0).unwrap_or(CgColor(std::ptr::null()));
    unsafe {
        let _: () = msg_send![l, setBackgroundColor: p];
    }
}

pub fn set_corner(l: &AnyObject, r: f64) {
    unsafe {
        let _: () = msg_send![l, setCornerRadius: r];
        // Continuous ("squircle") corners like system UI.
        let _: () = msg_send![l, setCornerCurve: &*ns("continuous")];
    }
}

pub fn set_border(l: &AnyObject, w: f64, c: Option<&Color>) {
    let p = c.map(|c| c.0).unwrap_or(CgColor(std::ptr::null()));
    unsafe {
        let _: () = msg_send![l, setBorderWidth: w];
        let _: () = msg_send![l, setBorderColor: p];
    }
}

pub fn set_shadow(l: &AnyObject, opacity: f64, radius: f64, dy: f64) {
    unsafe {
        let black = Color::black(1.0);
        let _: () = msg_send![l, setShadowColor: black.0];
        let _: () = msg_send![l, setShadowOpacity: opacity as f32];
        let _: () = msg_send![l, setShadowRadius: radius];
        let _: () = msg_send![l, setShadowOffset: NSSize::new(0.0, dy)];
    }
}

pub fn set_shadow_opacity(l: &AnyObject, opacity: f64) {
    unsafe {
        let _: () = msg_send![l, setShadowOpacity: opacity.clamp(0.0, 1.0) as f32];
    }
}

pub fn set_shadow_path(l: &AnyObject, p: &Path) {
    unsafe {
        let _: () = msg_send![l, setShadowPath: p.0];
    }
}

pub fn set_scale(l: &AnyObject, s: f64) {
    unsafe {
        let _: () = msg_send![l, setContentsScale: s];
    }
}

pub fn set_mask(l: &AnyObject, m: &AnyObject) {
    unsafe {
        let _: () = msg_send![l, setMask: m];
    }
}

pub fn set_masks_to_bounds(l: &AnyObject, b: bool) {
    unsafe {
        let _: () = msg_send![l, setMasksToBounds: b];
    }
}

pub fn set_transform(l: &AnyObject, t: AffineTransform) {
    unsafe {
        let _: () = msg_send![l, setAffineTransform: t];
    }
}

pub fn set_anchor(l: &AnyObject, x: f64, y: f64) {
    unsafe {
        let _: () = msg_send![l, setAnchorPoint: NSPoint::new(x, y)];
    }
}

pub fn set_image(l: &AnyObject, img: Option<&Image>) {
    unsafe {
        let p: *mut AnyObject = img.map(|i| i.as_obj()).unwrap_or(std::ptr::null_mut());
        let _: () = msg_send![l, setContents: p];
        let _: () = msg_send![l, setContentsGravity: &*ns("resizeAspect")];
    }
}

pub fn set_path(l: &AnyObject, p: &Path) {
    unsafe {
        let _: () = msg_send![l, setPath: p.0];
    }
}

pub fn set_fill(l: &AnyObject, c: Option<&Color>) {
    let p = c.map(|c| c.0).unwrap_or(CgColor(std::ptr::null()));
    unsafe {
        let _: () = msg_send![l, setFillColor: p];
    }
}

pub fn set_stroke(l: &AnyObject, c: Option<&Color>, w: f64) {
    let p = c.map(|c| c.0).unwrap_or(CgColor(std::ptr::null()));
    unsafe {
        let _: () = msg_send![l, setStrokeColor: p];
        let _: () = msg_send![l, setLineWidth: w];
    }
}

pub fn set_dash(l: &AnyObject, dash: &[f64]) {
    let nums: Vec<Retained<NSNumber>> = dash.iter().map(|d| NSNumber::new_f64(*d)).collect();
    let arr = NSArray::from_retained_slice(&nums);
    unsafe {
        let _: () = msg_send![l, setLineDashPattern: &*arr];
    }
}

pub fn font(size: f64, weight: f64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSFont), systemFontOfSize: size, weight: weight] }
}

pub const W_REGULAR: f64 = 0.0;
pub const W_MEDIUM: f64 = 0.23;
pub const W_SEMIBOLD: f64 = 0.3;
pub const W_BOLD: f64 = 0.4;

pub struct TextStyle {
    pub size: f64,
    pub weight: f64,
    pub align: &'static str,
    pub truncate: &'static str,
}

pub fn set_text(l: &AnyObject, text: &str, st: &TextStyle, color: &Color) {
    let f = font(st.size, st.weight);
    unsafe {
        let _: () = msg_send![l, setString: &*ns(text)];
        let _: () = msg_send![l, setFont: Retained::as_ptr(&f) as *const c_void];
        let _: () = msg_send![l, setFontSize: st.size];
        let _: () = msg_send![l, setForegroundColor: color.0];
        let _: () = msg_send![l, setAlignmentMode: &*ns(st.align)];
        let _: () = msg_send![l, setTruncationMode: &*ns(st.truncate)];
        let _: () = msg_send![l, setWrapped: false];
    }
}

/// Width of `text` in the system font.
pub fn text_width(text: &str, size: f64, weight: f64) -> f64 {
    let f = font(size, weight);
    unsafe {
        let key = ns("NSFont");
        let dict: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionaryWithObject: &*f, forKey: &*key];
        let sz: NSSize = msg_send![&*ns(text), sizeWithAttributes: &*dict];
        sz.width.ceil()
    }
}

/// Draw an `NSImage` into a new bitmap of `w`×`h` points at `scale`,
/// optionally tinting it with `tint` (alpha-preserving).
pub fn rasterize(img: &AnyObject, w: f64, h: f64, scale: f64, tint: Option<(f64, f64, f64, f64)>) -> Option<Image> {
    let (pw, ph) = ((w * scale).ceil().max(1.0) as usize, (h * scale).ceil().max(1.0) as usize);
    unsafe {
        let cs = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, pw * 4, cs, BITMAP_RGBA);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        CGContextScaleCTM(ctx, scale, scale);
        let g: Option<Retained<AnyObject>> = msg_send![class!(NSGraphicsContext), graphicsContextWithCGContext: CgContext(ctx), flipped: false];
        if let Some(g) = g {
            let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
            let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: &*g];
            let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            let _: () = msg_send![img, drawInRect: r, fromRect: zero, operation: 2usize, fraction: 1.0f64];
            let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        }
        if let Some((r, gg, b, a)) = tint {
            CGContextSetBlendMode(ctx, kCGBlendModeSourceIn);
            CGContextSetRGBFillColor(ctx, r, gg, b, a);
            CGContextFillRect(ctx, CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w, height: h } });
        }
        let out = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        Image::from_owned(out)
    }
}

/// An SF Symbol `NSImage` at a point size / weight.
pub fn symbol_nsimage(name: &str, size: f64, weight: f64) -> Option<Retained<AnyObject>> {
    unsafe {
        let img: Option<Retained<AnyObject>> =
            msg_send![class!(NSImage), imageWithSystemSymbolName: &*ns(name), accessibilityDescription: std::ptr::null::<AnyObject>()];
        let img = img?;
        let cfg: Retained<AnyObject> = msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: size, weight: weight];
        msg_send![&*img, imageWithSymbolConfiguration: &*cfg]
    }
}

/// A rasterized, tinted SF Symbol and its size in points.
#[derive(Clone)]
pub struct Symbol {
    pub image: Image,
    pub w: f64,
    pub h: f64,
}

#[derive(Default)]
pub struct SymbolCache {
    map: HashMap<String, Option<Symbol>>,
}

impl SymbolCache {
    pub fn get(&mut self, name: &str, size: f64, weight: f64, rgba: (f64, f64, f64, f64), scale: f64) -> Option<Symbol> {
        let key = format!("{name}|{size}|{weight}|{rgba:?}|{scale}");
        self.map
            .entry(key)
            .or_insert_with(|| {
                let img = symbol_nsimage(name, size, weight)?;
                let sz: NSSize = unsafe { msg_send![&*img, size] };
                let image = rasterize(&img, sz.width, sz.height, scale, Some(rgba))?;
                Some(Symbol { image, w: sz.width, h: sz.height })
            })
            .clone()
    }
}

/// Put a symbol into `l`, centered in `r`.
pub fn place_symbol(l: &AnyObject, sym: Option<&Symbol>, r: Rect) {
    match sym {
        Some(s) => {
            set_frame(l, Rect::new(r.center_x() - s.w / 2.0, r.center_y() - s.h / 2.0, s.w, s.h));
            set_image(l, Some(&s.image));
            set_hidden(l, false);
        }
        None => set_hidden(l, true),
    }
}

/// The system accent color as sRGB components.
pub fn accent_rgb() -> (f64, f64, f64) {
    unsafe {
        let c: Option<Retained<AnyObject>> = msg_send![class!(NSColor), controlAccentColor];
        let cs: Retained<AnyObject> = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let c: Option<Retained<AnyObject>> = c.and_then(|c| msg_send![&*c, colorUsingColorSpace: &*cs]);
        match c {
            Some(c) => {
                let r: f64 = msg_send![&*c, redComponent];
                let g: f64 = msg_send![&*c, greenComponent];
                let b: f64 = msg_send![&*c, blueComponent];
                (r, g, b)
            }
            None => (0.04, 0.52, 1.0),
        }
    }
}

/// Render a layer tree into a CGImage of `w`×`h` points.
pub fn render_layer(l: &AnyObject, w: f64, h: f64, scale: f64, flip: bool) -> Option<Image> {
    let (pw, ph) = ((w * scale).ceil().max(1.0) as usize, (h * scale).ceil().max(1.0) as usize);
    unsafe {
        let cs = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, pw * 4, cs, BITMAP_RGBA);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        if flip {
            CGContextTranslateCTM(ctx, 0.0, ph as f64);
            CGContextScaleCTM(ctx, scale, -scale);
        } else {
            CGContextScaleCTM(ctx, scale, scale);
        }
        let _: () = msg_send![l, renderInContext: CgContext(ctx)];
        let out = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        Image::from_owned(out)
    }
}

/// `NSImage` wrapping a CGImage at a point size.
pub fn nsimage_from_cg(img: &Image, w: f64, h: f64) -> Retained<AnyObject> {
    unsafe {
        let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        msg_send![a, initWithCGImage: img.0, size: NSSize::new(w, h)]
    }
}

/// Write a CGImage as PNG.
pub fn write_png(img: &Image, path: &std::path::Path) -> bool {
    unsafe {
        let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: Retained<AnyObject> = msg_send![a, initWithCGImage: img.0];
        let props: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        // NSBitmapImageFileTypePNG = 4
        let data: Option<Retained<AnyObject>> = msg_send![&*rep, representationUsingType: 4usize, properties: &*props];
        match data {
            Some(d) => msg_send![&*d, writeToFile: &*ns(&path.to_string_lossy()), atomically: true],
            None => false,
        }
    }
}

pub fn gradient_layer() -> Layer {
    let l: Layer = unsafe { msg_send![class!(CAGradientLayer), layer] };
    unsafe {
        let _: () = msg_send![&*l, setStartPoint: NSPoint::new(0.0, 0.5)];
        let _: () = msg_send![&*l, setEndPoint: NSPoint::new(1.0, 0.5)];
    }
    l
}

/// Horizontal alpha mask: transparent → opaque over `left` (fraction of
/// the width), opaque → transparent over the last `right`.
pub fn set_gradient_h(l: &AnyObject, left: f64, right: f64) {
    let clear = Color::black(0.0);
    let solid = Color::black(1.0);
    let (c0, c3) = (if left > 0.0 { &clear } else { &solid }, if right > 0.0 { &clear } else { &solid });
    unsafe {
        let objs: [&AnyObject; 4] = [&*(c0.0 .0 as *const AnyObject), &*(solid.0 .0 as *const AnyObject), &*(solid.0 .0 as *const AnyObject), &*(c3.0 .0 as *const AnyObject)];
        let colors = NSArray::from_slice(&objs);
        let locs: Vec<Retained<NSNumber>> = [0.0, left.max(0.0001), 1.0 - right.max(0.0001), 1.0].iter().map(|v| NSNumber::new_f64(*v)).collect();
        let locs = NSArray::from_retained_slice(&locs);
        let _: () = msg_send![l, setColors: &*colors];
        let _: () = msg_send![l, setLocations: &*locs];
    }
}

/// Aspect-fit `img` into a `box_pts` square with rounded corners and a
/// hairline edge.
pub fn rounded_fit(img: &Image, box_pts: f64, radius: f64, scale: f64) -> Option<Image> {
    let (iw, ih) = unsafe { (CGImageGetWidth(img.0 .0) as f64, CGImageGetHeight(img.0 .0) as f64) };
    if iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    let k = (box_pts / iw).min(box_pts / ih);
    let (w, h) = ((iw * k).max(8.0), (ih * k).max(8.0));
    let (pw, ph) = ((w * scale).ceil() as usize, (h * scale).ceil() as usize);
    unsafe {
        let cs = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, pw * 4, cs, BITMAP_RGBA);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        CGContextScaleCTM(ctx, scale, scale);
        CGContextSetInterpolationQuality(ctx, 3);
        let r = Rect::new(0.0, 0.0, w, h);
        let rad = radius.min(w / 4.0).min(h / 4.0);
        let path = Path::rounded_rect(r, rad);
        CGContextSaveGState(ctx);
        CGContextAddPath(ctx, path.0 .0);
        CGContextClip(ctx);
        CGContextDrawImage(ctx, r.into(), img.0 .0);
        CGContextRestoreGState(ctx);
        let edge = Path::rounded_rect(r.inflate(-0.25, -0.25), rad);
        CGContextAddPath(ctx, edge.0 .0);
        CGContextSetRGBStrokeColor(ctx, 1.0, 1.0, 1.0, 0.18);
        CGContextSetLineWidth(ctx, 0.5);
        CGContextStrokePath(ctx);
        let out = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        Image::from_owned(out)
    }
}
