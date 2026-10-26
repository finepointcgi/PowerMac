//! Paints [`crate::widgets::draw::Prim`] lists into Core Animation layers.
//! A [`Canvas`] keeps one layer (tree) per primitive slot and only touches
//! layers when the primitive list changed.

use std::collections::HashMap;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize};

use super::ffi::*;
use super::gfx::{self, Layer, SymbolCache};
use crate::geometry::{Point, Rect};
use crate::widgets::draw::{Align, Font, ImageKey, Prim, Weight};
use crate::widgets::Rgba;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPathAddArc(p: *const c_void, m: *const c_void, x: f64, y: f64, r: f64, start: f64, end: f64, clockwise: bool);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Rect,
    Text,
    Symbol,
    Image,
    Line,
    Ring,
    Bars,
    Camera,
}

fn tag(p: &Prim) -> Tag {
    match p {
        Prim::Rect { .. } => Tag::Rect,
        Prim::Text { .. } => Tag::Text,
        Prim::Symbol { .. } => Tag::Symbol,
        Prim::Image { .. } => Tag::Image,
        Prim::Line { .. } => Tag::Line,
        Prim::Ring { .. } => Tag::Ring,
        Prim::Bars { .. } => Tag::Bars,
        Prim::Camera { .. } => Tag::Camera,
    }
}

fn color(c: Rgba) -> Color {
    Color::rgba(c.0, c.1, c.2, c.3)
}

fn weight(w: Weight) -> f64 {
    match w {
        Weight::Regular => gfx::W_REGULAR,
        Weight::Medium => gfx::W_MEDIUM,
        Weight::Semibold => gfx::W_SEMIBOLD,
        Weight::Bold => gfx::W_BOLD,
    }
}

/// Things primitives refer to.
pub struct Env<'a> {
    pub scale: f64,
    pub symbols: &'a mut SymbolCache,
    pub fonts: &'a mut FontCache,
    pub artwork: Option<&'a gfx::Image>,
    pub artwork_key: u64,
    pub camera: Option<&'a AnyObject>,
}

#[derive(Default)]
pub struct FontCache {
    map: HashMap<String, Retained<AnyObject>>,
}

impl FontCache {
    pub fn get(&mut self, f: &Font) -> Retained<AnyObject> {
        let key = format!("{}|{:?}|{}|{}", f.size, f.weight, f.mono, f.rounded);
        self.map
            .entry(key)
            .or_insert_with(|| unsafe {
                let w = weight(f.weight);
                let base: Retained<AnyObject> = if f.mono {
                    msg_send![class!(NSFont), monospacedDigitSystemFontOfSize: f.size, weight: w]
                } else {
                    msg_send![class!(NSFont), systemFontOfSize: f.size, weight: w]
                };
                if !f.rounded {
                    return base;
                }
                let desc: Retained<AnyObject> = msg_send![&*base, fontDescriptor];
                let rd: Option<Retained<AnyObject>> = msg_send![&*desc, fontDescriptorWithDesign: &*ns("NSCTFontUIFontDesignRounded")];
                match rd {
                    Some(rd) => {
                        let f2: Option<Retained<AnyObject>> = msg_send![class!(NSFont), fontWithDescriptor: &*rd, size: f.size];
                        f2.unwrap_or(base)
                    }
                    None => base,
                }
            })
            .clone()
    }
}

pub struct Canvas {
    pub root: Layer,
    slots: Vec<(Tag, Layer)>,
    last: Vec<Prim>,
    last_art: u64,
    size: (f64, f64),
}

impl Canvas {
    pub fn new(parent: &AnyObject, scale: f64) -> Canvas {
        let root = gfx::layer();
        gfx::set_scale(&root, scale);
        gfx::add(parent, &root);
        Canvas { root, slots: vec![], last: vec![], last_art: 0, size: (0.0, 0.0) }
    }

    /// Position the canvas (top-left rect in the parent).
    pub fn set_frame(&mut self, r: Rect) {
        transaction(None, || gfx::set_frame(&self.root, r));
        if self.size != (r.w, r.h) {
            self.size = (r.w, r.h);
            self.last.clear();
        }
    }

    pub fn paint(&mut self, prims: &[Prim], env: &mut Env) {
        let uses_art = prims.iter().any(|p| matches!(p, Prim::Image { .. }));
        let uses_cam = prims.iter().any(|p| matches!(p, Prim::Camera { .. }));
        if prims == self.last.as_slice() && (!uses_art || self.last_art == env.artwork_key) && !uses_cam {
            return;
        }
        self.last_art = env.artwork_key;
        transaction(None, || {
            for (i, p) in prims.iter().enumerate() {
                let t = tag(p);
                let reuse = self.slots.get(i).is_some_and(|(st, _)| *st == t);
                if !reuse {
                    let l = make(t, env.scale);
                    if i < self.slots.len() {
                        let old = std::mem::replace(&mut self.slots[i], (t, l));
                        unsafe {
                            let _: () = msg_send![&*self.root, replaceSublayer: &*old.1, with: &*self.slots[i].1];
                        }
                    } else {
                        gfx::add(&self.root, &l);
                        self.slots.push((t, l));
                    }
                }
                let changed = self.last.get(i) != Some(p) || matches!(p, Prim::Image { .. } | Prim::Camera { .. }) || !reuse;
                if changed {
                    let l = self.slots[i].1.clone();
                    gfx::set_hidden(&l, false);
                    apply(&l, p, env);
                }
            }
            for (_, l) in self.slots.iter().skip(prims.len()) {
                gfx::set_hidden(l, true);
            }
        });
        self.last = prims.to_vec();
    }
}

fn make(t: Tag, scale: f64) -> Layer {
    let l = match t {
        Tag::Text => gfx::text_layer(scale),
        Tag::Ring | Tag::Line => {
            let c = gfx::layer();
            for _ in 0..2 {
                let s = gfx::shape_layer();
                gfx::set_scale(&s, scale);
                gfx::add(&c, &s);
            }
            c
        }
        Tag::Bars => {
            let c = gfx::layer();
            for _ in 0..4 {
                let b = gfx::layer();
                gfx::set_scale(&b, scale);
                gfx::add(&c, &b);
            }
            c
        }
        _ => gfx::layer(),
    };
    gfx::set_scale(&l, scale);
    l
}

fn sublayers(l: &AnyObject) -> Vec<Retained<AnyObject>> {
    unsafe {
        let arr: Option<Retained<objc2_foundation::NSArray<AnyObject>>> = msg_send![l, sublayers];
        arr.map(|a| a.to_vec()).unwrap_or_default()
    }
}

/// Path in a layer of height `h` from top-left points.
fn poly_path(pts: &[Point], h: f64, close_to: Option<f64>) -> Path {
    unsafe {
        let p = CGPathCreateMutable();
        let n = std::ptr::null();
        for (i, q) in pts.iter().enumerate() {
            if i == 0 {
                CGPathMoveToPoint(p, n, q.x, h - q.y);
            } else {
                CGPathAddLineToPoint(p, n, q.x, h - q.y);
            }
        }
        if let (Some(base), Some(first), Some(last)) = (close_to, pts.first(), pts.last()) {
            CGPathAddLineToPoint(p, n, last.x, h - base);
            CGPathAddLineToPoint(p, n, first.x, h - base);
            CGPathCloseSubpath(p);
        }
        Path(CgPath(p))
    }
}

fn set_line_style(l: &AnyObject) {
    unsafe {
        let _: () = msg_send![l, setLineCap: &*ns("round")];
        let _: () = msg_send![l, setLineJoin: &*ns("round")];
    }
}

fn apply(l: &AnyObject, p: &Prim, env: &mut Env) {
    match p {
        Prim::Rect { r, radius, fill, stroke } => {
            gfx::set_frame(l, *r);
            gfx::set_corner(l, radius.min(r.h / 2.0).min(r.w / 2.0));
            gfx::set_bg(l, Some(&color(*fill)));
            match stroke {
                Some((c, w)) => gfx::set_border(l, *w, Some(&color(*c))),
                None => gfx::set_border(l, 0.0, None),
            }
        }
        Prim::Text { r, text, font, color: c, align, wrap } => {
            let lh = (font.size * 1.22).ceil();
            let frame = if *wrap { *r } else { Rect::new(r.x, r.center_y() - lh / 2.0, r.w, lh) };
            gfx::set_frame(l, frame);
            let f = env.fonts.get(font);
            let col = color(*c);
            unsafe {
                let _: () = msg_send![l, setString: &*ns(text)];
                let _: () = msg_send![l, setFont: Retained::as_ptr(&f) as *const c_void];
                let _: () = msg_send![l, setFontSize: font.size];
                let _: () = msg_send![l, setForegroundColor: col.0];
                let a = match align {
                    Align::Left => "left",
                    Align::Center => "center",
                    Align::Right => "right",
                };
                let _: () = msg_send![l, setAlignmentMode: &*ns(a)];
                let _: () = msg_send![l, setTruncationMode: &*ns("end")];
                let _: () = msg_send![l, setWrapped: *wrap];
            }
        }
        Prim::Symbol { r, name, size, weight: w, color: c } => {
            let sym = env
                .symbols
                .get(name, *size, weight(*w), (c.0, c.1, c.2, c.3), env.scale)
                .or_else(|| env.symbols.get("questionmark", *size, weight(*w), (c.0, c.1, c.2, c.3), env.scale));
            gfx::place_symbol(l, sym.as_ref(), *r);
        }
        Prim::Image { r, key, radius } => {
            gfx::set_frame(l, *r);
            gfx::set_corner(l, *radius);
            gfx::set_masks_to_bounds(l, true);
            let img = match key {
                ImageKey::Artwork => env.artwork,
            };
            gfx::set_image(l, img);
            unsafe {
                let _: () = msg_send![l, setContentsGravity: &*ns("resizeAspectFill")];
            }
            gfx::set_bg(l, Some(&Color::white(0.08)));
        }
        Prim::Line { pts, color: c, width, fill } => {
            let subs = sublayers(l);
            let (Some(fl), Some(st)) = (subs.first(), subs.get(1)) else { return };
            let (w, h) = sup_size(l);
            gfx::set_frame(l, Rect::new(0.0, 0.0, w, h));
            for s in [fl, st] {
                gfx::set_frame(s, Rect::new(0.0, 0.0, w, h));
            }
            match fill {
                Some((base, fc)) => {
                    gfx::set_path(fl, &poly_path(pts, h, Some(*base)));
                    gfx::set_fill(fl, Some(&color(*fc)));
                    gfx::set_stroke(fl, None, 0.0);
                    gfx::set_hidden(fl, false);
                }
                None => gfx::set_hidden(fl, true),
            }
            gfx::set_path(st, &poly_path(pts, h, None));
            gfx::set_fill(st, None);
            gfx::set_stroke(st, Some(&color(*c)), *width);
            set_line_style(st);
        }
        Prim::Ring { c, radius, width, frac, color: col, track } => {
            let subs = sublayers(l);
            let (Some(tr), Some(arc)) = (subs.first(), subs.get(1)) else { return };
            let box_w = 2.0 * radius + width + 2.0;
            gfx::set_frame(l, Rect::new(c.x - box_w / 2.0, c.y - box_w / 2.0, box_w, box_w));
            let mid = box_w / 2.0;
            for s in [tr, arc] {
                gfx::set_frame(s, Rect::new(0.0, 0.0, box_w, box_w));
                gfx::set_fill(s, None);
                set_line_style(s);
            }
            let circle = |from: f64, to: f64| unsafe {
                let p = CGPathCreateMutable();
                CGPathAddArc(p, std::ptr::null(), mid, mid, *radius, from, to, true);
                Path(CgPath(p))
            };
            let top = std::f64::consts::FRAC_PI_2;
            gfx::set_path(tr, &circle(top, top - 2.0 * std::f64::consts::PI + 1e-4));
            gfx::set_stroke(tr, Some(&color(*track)), *width);
            let f = frac.clamp(0.0, 1.0);
            gfx::set_hidden(arc, f <= 0.0005);
            gfx::set_path(arc, &circle(top, top - 2.0 * std::f64::consts::PI * f.min(0.9999)));
            gfx::set_stroke(arc, Some(&color(*col)), *width);
        }
        Prim::Bars { r, color: c, playing } => {
            gfx::set_frame(l, *r);
            let bars = sublayers(l);
            let n = bars.len().max(1) as f64;
            let gap = 2.0;
            let bw = ((r.w - gap * (n - 1.0)) / n).max(1.0);
            // Static heights (what offscreen renders show) and animation.
            const H: [f64; 4] = [0.55, 1.0, 0.7, 0.85];
            const DUR: [f64; 4] = [0.48, 0.62, 0.41, 0.55];
            for (i, b) in bars.iter().enumerate() {
                gfx::set_anchor(b, 0.5, 0.0);
                gfx::set_frame(b, Rect::new(i as f64 * (bw + gap), 0.0, bw, r.h));
                gfx::set_corner(b, bw / 2.0);
                gfx::set_bg(b, Some(&color(*c)));
                unsafe {
                    let _: () = msg_send![&**b, removeAllAnimations];
                    let t = gfx::AffineTransform { a: 1.0, b: 0.0, c: 0.0, d: if *playing { H[i % 4] } else { 0.25 }, tx: 0.0, ty: 0.0 };
                    gfx::set_transform(b, t);
                    if *playing {
                        let anim: Retained<AnyObject> = msg_send![class!(CABasicAnimation), animationWithKeyPath: &*ns("transform.scale.y")];
                        let from = NSNumber::new_f64(0.25);
                        let to = NSNumber::new_f64(1.0);
                        let _: () = msg_send![&*anim, setFromValue: &*from];
                        let _: () = msg_send![&*anim, setToValue: &*to];
                        let _: () = msg_send![&*anim, setDuration: DUR[i % 4]];
                        let _: () = msg_send![&*anim, setAutoreverses: true];
                        let _: () = msg_send![&*anim, setRepeatCount: f32::INFINITY];
                        let _: () = msg_send![&*anim, setTimeOffset: H[i % 4] * DUR[i % 4]];
                        let tf: Retained<AnyObject> = msg_send![class!(CAMediaTimingFunction), functionWithName: &*ns("easeInEaseOut")];
                        let _: () = msg_send![&*anim, setTimingFunction: &*tf];
                        let _: () = msg_send![&**b, addAnimation: &*anim, forKey: &*ns("eq")];
                    }
                }
            }
        }
        Prim::Camera { r, radius } => {
            gfx::set_frame(l, *r);
            gfx::set_corner(l, *radius);
            gfx::set_masks_to_bounds(l, true);
            gfx::set_bg(l, Some(&Color::black(1.0)));
            if let Some(cam) = env.camera {
                let cur: Option<Retained<AnyObject>> = unsafe { msg_send![cam, superlayer] };
                if cur.as_deref().map(|c| c as *const AnyObject) != Some(l as *const AnyObject) {
                    gfx::add(l, cam);
                }
                let b = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(r.w, r.h));
                unsafe {
                    let _: () = msg_send![cam, setFrame: b];
                }
            }
        }
    }
}

fn sup_size(l: &AnyObject) -> (f64, f64) {
    unsafe {
        let sup: Option<Retained<AnyObject>> = msg_send![l, superlayer];
        match sup {
            Some(s) => {
                let b: NSRect = msg_send![&*s, bounds];
                (b.size.width, b.size.height)
            }
            None => (0.0, 0.0),
        }
    }
}

/// Decode an encoded image (PNG / JPEG / HEIC ...) into a CGImage.
pub fn decode_image(bytes: &[u8]) -> Option<gfx::Image> {
    unsafe {
        let data = objc2_foundation::NSData::with_bytes(bytes);
        let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        let img: Option<Retained<AnyObject>> = msg_send![a, initWithData: &*data];
        let img = img?;
        let mut r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let cg: CgImage = msg_send![&*img, CGImageForProposedRect: &mut r, context: std::ptr::null::<AnyObject>(), hints: std::ptr::null::<AnyObject>()];
        gfx::Image::retain(cg.0)
    }
}
