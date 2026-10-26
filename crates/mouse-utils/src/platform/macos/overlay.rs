//! Main-thread renderer: one borderless, transparent, click-through NSWindow
//! per screen, drawn with Core Animation layers, ticked by a 60 Hz
//! `CFRunLoopTimer` on the main run loop that only runs while the engine
//! needs frames. Everything here runs on the main thread; other threads
//! reach it through [`super::ffi::dispatch_main`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::ffi::*;
use crate::engine::{Point, RectF, Scene};
use crate::Shared;

/// NSScreenSaverWindowLevel — above the menu bar, Dock and full-screen apps.
const OVERLAY_LEVEL: isize = 1000;
const FRAME_INTERVAL: f64 = 1.0 / 60.0;

type Layer = Retained<AnyObject>;

struct ScreenOverlay {
    /// Global top-left coordinates (points).
    frame: RectF,
    scale: f64,
    window: Retained<NSWindow>,
    root: Option<Layer>,
    container: Layer,
    spot: Layer,
    circles: Vec<Layer>,
    arms: Vec<Layer>,
}

struct Renderer {
    shared: Arc<Shared>,
    timer: CFRunLoopTimerRef,
    overlays: Vec<ScreenOverlay>,
    visible: bool,
    last: Option<Scene>,
    frames: u64,
}

thread_local! {
    static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) };
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn cgrect(r: NSRect) -> CGRect {
    CGRect { origin: CGPoint { x: r.origin.x, y: r.origin.y }, size: CGSize { width: r.size.width, height: r.size.height } }
}

unsafe fn new_layer(class_name: &str) -> Layer {
    let cls = match class_name {
        "CAShapeLayer" => class!(CAShapeLayer),
        _ => class!(CALayer),
    };
    msg_send![cls, layer]
}

unsafe fn set_hidden(l: &AnyObject, hidden: bool) {
    let _: () = msg_send![l, setHidden: hidden];
}

unsafe fn set_frame(l: &AnyObject, r: NSRect) {
    let _: () = msg_send![l, setFrame: r];
}

unsafe fn set_bg(l: &AnyObject, c: crate::config::Color) {
    let col = OwnedColor::new(c);
    let _: () = msg_send![l, setBackgroundColor: col.0];
}

fn make_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setIgnoresMouseEvents(true);
    window.setHasShadow(false);
    window.setLevel(OVERLAY_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window
}

/// Screens as (global top-left frame, cocoa frame, backing scale).
fn screens(mtm: MainThreadMarker) -> Vec<(RectF, NSRect, f64)> {
    let screens = NSScreen::screens(mtm);
    let ph = screens.iter().next().map(|s| s.frame().size.height).unwrap_or(0.0);
    screens
        .iter()
        .map(|s| {
            let f = s.frame();
            let top = ph - (f.origin.y + f.size.height);
            (RectF::new(f.origin.x, top, f.origin.x + f.size.width, top + f.size.height), f, s.backingScaleFactor())
        })
        .collect()
}

fn make_overlay(mtm: MainThreadMarker, frame: RectF, cocoa: NSRect, scale: f64) -> ScreenOverlay {
    let window = make_window(mtm, cocoa);
    let (w, h) = (cocoa.size.width, cocoa.size.height);
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, w, h));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));
    unsafe {
        let root: Option<Layer> = msg_send![&*view, layer];
        let container = new_layer("CALayer");
        set_frame(&container, rect(0.0, 0.0, w, h));
        let _: () = msg_send![&*container, setGeometryFlipped: true];
        if let Some(root) = &root {
            let _: () = msg_send![&**root, addSublayer: &*container];
        }
        let spot = new_layer("CAShapeLayer");
        set_frame(&spot, rect(0.0, 0.0, w, h));
        let _: () = msg_send![&*spot, setContentsScale: scale];
        let _: () = msg_send![&*spot, setFillRule: kCAFillRuleEvenOdd];
        set_hidden(&spot, true);
        let _: () = msg_send![&*container, addSublayer: &*spot];
        ScreenOverlay { frame, scale, window, root, container, spot, circles: vec![], arms: vec![] }
    }
}

impl ScreenOverlay {
    fn pooled(&self, pool: &mut Vec<Layer>, n: usize) {
        while pool.len() < n {
            let l = unsafe { new_layer("CALayer") };
            unsafe {
                let _: () = msg_send![&*l, setContentsScale: self.scale];
                let _: () = msg_send![&*self.container, addSublayer: &*l];
            }
            pool.push(l);
        }
    }

    fn draw(&mut self, scene: &Scene) {
        let o = self.frame;
        let (w, h) = (o.width(), o.height());
        let local = |p: Point| (p.x - o.x0, p.y - o.y0);
        unsafe {
            // Spotlight: even-odd path (screen rect + circle) filled with the backdrop.
            match &scene.spotlight {
                Some(s) => {
                    let (cx, cy) = local(s.center);
                    let path = CGPathCreateMutable();
                    CGPathAddRect(path, std::ptr::null(), cgrect(rect(0.0, 0.0, w, h)));
                    let r = s.radius;
                    CGPathAddEllipseInRect(path, std::ptr::null(), cgrect(rect(cx - r, cy - r, 2.0 * r, 2.0 * r)));
                    let _: () = msg_send![&*self.spot, setPath: CgPath(path)];
                    CGPathRelease(path);
                    let col = OwnedColor::new(s.color);
                    let _: () = msg_send![&*self.spot, setFillColor: col.0];
                    set_hidden(&self.spot, false);
                }
                None => set_hidden(&self.spot, true),
            }

            // Highlighter circles.
            let bounds = RectF::new(o.x0, o.y0, o.x1, o.y1);
            let visible: Vec<_> = scene
                .circles
                .iter()
                .filter(|c| {
                    let r = c.radius;
                    !RectF::new(c.center.x - r, c.center.y - r, c.center.x + r, c.center.y + r).intersect(&bounds).is_empty()
                })
                .collect();
            let mut circles = std::mem::take(&mut self.circles);
            self.pooled(&mut circles, visible.len());
            for (i, l) in circles.iter().enumerate() {
                match visible.get(i) {
                    Some(c) => {
                        let (cx, cy) = local(c.center);
                        set_frame(l, rect(cx - c.radius, cy - c.radius, 2.0 * c.radius, 2.0 * c.radius));
                        let _: () = msg_send![&**l, setCornerRadius: c.radius];
                        set_bg(l, c.color);
                        set_hidden(l, false);
                    }
                    None => set_hidden(l, true),
                }
            }
            self.circles = circles;

            // Crosshairs: one layer per arm, background = line, border = outline.
            let arms = match &scene.crosshairs {
                // Clip a little outside the screen so the far-end border is off-screen.
                Some(x) => x.arms(bounds.inflate(x.border + 2.0), 1.0).into_iter().map(|a| (a, x)).collect(),
                None => vec![],
            };
            let mut pool = std::mem::take(&mut self.arms);
            self.pooled(&mut pool, arms.len());
            for (i, l) in pool.iter().enumerate() {
                match arms.get(i) {
                    Some((a, x)) => {
                        let r = a.outer.offset(-o.x0, -o.y0);
                        set_frame(l, rect(r.x0, r.y0, r.width(), r.height()));
                        set_bg(l, x.color);
                        let bc = OwnedColor::new(x.border_color);
                        let _: () = msg_send![&**l, setBorderColor: bc.0];
                        let _: () = msg_send![&**l, setBorderWidth: x.border];
                        let _: () = msg_send![&**l, setOpacity: x.opacity as f32];
                        set_hidden(l, false);
                    }
                    None => set_hidden(l, true),
                }
            }
            self.arms = pool;
        }
    }
}

impl Renderer {
    fn sync_screens(&mut self, mtm: MainThreadMarker) {
        let now = screens(mtm);
        let same = now.len() == self.overlays.len()
            && now.iter().zip(&self.overlays).all(|((f, _, s), o)| *f == o.frame && *s == o.scale);
        if same {
            return;
        }
        for o in self.overlays.drain(..) {
            o.window.orderOut(None);
            o.window.close();
        }
        self.overlays = now.into_iter().map(|(f, c, s)| make_overlay(mtm, f, c, s)).collect();
        self.visible = false;
        self.last = None;
    }

    fn tick(&mut self, mtm: MainThreadMarker) {
        let p = cursor_location();
        let cursor = Point::new(p.x, p.y);
        let (scene, keep) = {
            let mut st = self.shared.lock();
            let scene = st.engine.frame(Instant::now(), cursor);
            let keep = !scene.is_empty() || st.engine.needs_frames();
            if !keep {
                st.timer_running = false;
            }
            (scene, keep)
        };
        if scene.is_empty() {
            if self.visible {
                for o in &self.overlays {
                    o.window.orderOut(None);
                }
                self.visible = false;
            }
            self.last = None;
        } else {
            if !self.visible || self.frames.is_multiple_of(60) {
                self.sync_screens(mtm);
            }
            self.frames += 1;
            if self.last.as_ref() != Some(&scene) {
                unsafe {
                    let _: () = msg_send![class!(CATransaction), begin];
                    let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                }
                for o in &mut self.overlays {
                    o.draw(&scene);
                }
                unsafe {
                    let _: () = msg_send![class!(CATransaction), commit];
                }
                self.last = Some(scene);
            }
            if !self.visible {
                for o in &self.overlays {
                    o.window.orderFrontRegardless();
                }
                self.visible = true;
            }
        }
        if !keep {
            self.stop_timer();
        }
    }

    fn start_timer(&mut self) {
        if !self.timer.is_null() {
            return;
        }
        unsafe {
            let t = CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent(),
                FRAME_INTERVAL,
                0,
                0,
                timer_cb,
                std::ptr::null_mut(),
            );
            CFRunLoopAddTimer(CFRunLoopGetMain(), t, kCFRunLoopCommonModes);
            self.timer = t;
        }
    }

    fn stop_timer(&mut self) {
        if !self.timer.is_null() {
            unsafe {
                CFRunLoopTimerInvalidate(self.timer);
                CFRelease(self.timer);
            }
            self.timer = std::ptr::null();
        }
    }
}

extern "C" fn timer_cb(_t: CFRunLoopTimerRef, _info: *mut c_void) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    autoreleasepool(|_| {
        RENDERER.with(|r| {
            if let Ok(mut r) = r.try_borrow_mut() {
                if let Some(r) = r.as_mut() {
                    r.tick(mtm);
                }
            }
        })
    });
}

fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    if let Some(mtm) = MainThreadMarker::new() {
        f(mtm);
    } else {
        dispatch_main(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                f(mtm)
            }
        });
    }
}

/// Install the renderer (any thread).
pub fn install(shared: Arc<Shared>) {
    on_main(move |_| {
        RENDERER.with(|r| {
            let mut r = r.borrow_mut();
            if let Some(mut old) = r.take() {
                old.teardown();
            }
            *r = Some(Renderer { shared, timer: std::ptr::null(), overlays: vec![], visible: false, last: None, frames: 0 });
        });
    });
}

/// Make sure the frame timer is running (any thread).
pub fn kick() {
    dispatch_main(|| {
        RENDERER.with(|r| {
            if let Ok(mut r) = r.try_borrow_mut() {
                if let Some(r) = r.as_mut() {
                    r.start_timer();
                }
            }
        })
    });
}

impl Renderer {
    fn teardown(&mut self) {
        self.stop_timer();
        for o in self.overlays.drain(..) {
            o.window.orderOut(None);
            o.window.close();
        }
        self.shared.lock().timer_running = false;
    }
}

/// Stop the timer and close all overlay windows (any thread; synchronous on
/// the main thread).
pub fn uninstall() {
    on_main(|_| {
        RENDERER.with(|r| {
            if let Some(mut old) = r.borrow_mut().take() {
                old.teardown();
            }
        })
    });
}

/// Diagnostics (main thread): software-render the overlay under each
/// `cursor + offset` point with `-[CALayer renderInContext:]` and return its
/// RGBA (premultiplied) pixel; `None` if no overlay covers the point.
/// Hidden overlays read as fully transparent.
pub fn debug_probe(offsets: &[(f64, f64)]) -> Vec<Option<[u8; 4]>> {
    #[repr(transparent)]
    #[derive(Clone, Copy)]
    struct CgContext(*const c_void);
    unsafe impl objc2::encode::Encode for CgContext {
        const ENCODING: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGContext", &[]));
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
        fn CGColorSpaceRelease(cs: *const c_void);
        fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, cs: *const c_void, info: u32) -> *const c_void;
        fn CGBitmapContextGetData(ctx: *const c_void) -> *mut u8;
        fn CGContextRelease(ctx: *const c_void);
    }
    let c = cursor_location();
    RENDERER.with(|r| {
        let r = r.borrow();
        offsets
            .iter()
            .map(|&(dx, dy)| {
                let r = r.as_ref()?;
                let g = Point::new(c.x + dx, c.y + dy);
                let o = r.overlays.iter().find(|o| o.frame.contains(g))?;
                if !r.visible {
                    return Some([0; 4]);
                }
                let root = o.root.as_ref()?;
                let (w, h) = (o.frame.width() as usize, o.frame.height() as usize);
                let (lx, ly) = ((g.x - o.frame.x0) as usize, (g.y - o.frame.y0) as usize);
                unsafe {
                    let cs = CGColorSpaceCreateDeviceRGB();
                    let ctx = CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, w * 4, cs, 1 /* premultiplied last */);
                    CGColorSpaceRelease(cs);
                    if ctx.is_null() {
                        return None;
                    }
                    let _: () = msg_send![&**root, renderInContext: CgContext(ctx)];
                    let data = CGBitmapContextGetData(ctx);
                    let i = (ly.min(h - 1) * w + lx.min(w - 1)) * 4;
                    let px = [*data.add(i), *data.add(i + 1), *data.add(i + 2), *data.add(i + 3)];
                    CGContextRelease(ctx);
                    Some(px)
                }
            })
            .collect()
    })
}
