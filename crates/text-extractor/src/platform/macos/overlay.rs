//! The selection overlay: one borderless window per screen above everything
//! (menu bar and full-screen apps included) that dims the screen and lets the
//! user drag a rectangle with a crosshair cursor. Esc or a right click
//! cancels. Main thread only.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSBackingStoreType, NSBezierPath, NSColor,
    NSCursor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSRunningApplication, NSScreen, NSStringDrawing, NSView, NSWindingRule, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use super::capture::GRect;

/// NSScreenSaverWindowLevel: above the menu bar, Dock and full-screen apps.
const LEVEL: isize = 1000;
/// Smallest selection (points) that counts; smaller drags are ignored.
const MIN_SIZE: f64 = 4.0;

pub type SelectCallback = Box<dyn FnOnce(Option<GRect>)>;

struct Session {
    windows: Vec<Retained<OverlayWindow>>,
    views: Vec<Retained<SelectionView>>,
    /// Global Cocoa coordinates (origin bottom-left of the main screen).
    start: Option<NSPoint>,
    current: NSPoint,
    cb: Option<SelectCallback>,
    previous_app: Option<Retained<NSRunningApplication>>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

fn rect_from(a: NSPoint, b: NSPoint) -> NSRect {
    NSRect::new(
        NSPoint::new(a.x.min(b.x), a.y.min(b.y)),
        NSSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}

fn redraw_all() {
    let views: Vec<Retained<SelectionView>> = SESSION.with(|s| {
        s.borrow()
            .as_ref()
            .map(|s| s.views.clone())
            .unwrap_or_default()
    });
    for v in views {
        v.setNeedsDisplay(true);
    }
}

fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .iter()
        .next()
        .map(|s| s.frame().size.height)
        .unwrap_or(0.0)
}

/// End the session: close the windows, give focus back, report.
fn finish(res: Option<NSRect>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(s) = SESSION.with(|s| s.borrow_mut().take()) else {
        return;
    };
    for w in &s.windows {
        w.orderOut(None);
        w.close();
    }
    if let Some(prev) = &s.previous_app {
        if !prev.isTerminated() {
            #[allow(deprecated)]
            prev.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        }
    }
    let ph = primary_height(mtm);
    let out = res.map(|r| GRect {
        x: r.origin.x,
        y: ph - (r.origin.y + r.size.height),
        w: r.size.width,
        h: r.size.height,
    });
    if let Some(cb) = s.cb {
        cb(out);
    }
}

fn attrs(size: f64, bold: bool, color: &NSColor) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    let keys: [&NSString; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let font_obj: &AnyObject = &font;
    let color_obj: &AnyObject = color;
    NSDictionary::from_slices(&keys, &[font_obj, color_obj])
}

/// A dark rounded pill with white text, centered horizontally on `cx`,
/// bottom edge at `y` (view coordinates).
fn draw_pill(text: &str, cx: f64, y: f64, size: f64, bounds: NSRect) {
    let white = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0);
    let a = attrs(size, false, &white);
    let s = NSString::from_str(text);
    let ts = unsafe { s.sizeWithAttributes(Some(&a)) };
    let (pw, ph) = (ts.width + 24.0, ts.height + 10.0);
    let x = (cx - pw / 2.0).clamp(
        bounds.origin.x + 8.0,
        bounds.origin.x + bounds.size.width - pw - 8.0,
    );
    let r = NSRect::new(NSPoint::new(x, y), NSSize::new(pw, ph));
    NSColor::colorWithSRGBRed_green_blue_alpha(0.08, 0.08, 0.1, 0.82).setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, ph / 2.0, ph / 2.0).fill();
    unsafe { s.drawAtPoint_withAttributes(NSPoint::new(x + 12.0, y + 5.0), Some(&a)) };
}

define_class!(
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "PMTextExtractorOverlayWindow"]
    struct OverlayWindow;

    impl OverlayWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            true
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PMTextExtractorSelectionView"]
    struct SelectionView;

    impl SelectionView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::crosshairCursor());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            let p = NSEvent::mouseLocation();
            SESSION.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.start = Some(p);
                    s.current = p;
                }
            });
            redraw_all();
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _e: &NSEvent) {
            let p = NSEvent::mouseLocation();
            SESSION.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.current = p;
                }
            });
            redraw_all();
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, _e: &NSEvent) {
            let p = NSEvent::mouseLocation();
            SESSION.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.current = p;
                }
            });
            NSCursor::crosshairCursor().set();
            redraw_all();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _e: &NSEvent) {
            let p = NSEvent::mouseLocation();
            let sel = SESSION.with(|s| {
                let mut b = s.borrow_mut();
                let s = b.as_mut()?;
                let r = rect_from(s.start.take()?, p);
                s.current = p;
                Some(r)
            });
            match sel {
                Some(r) if r.size.width >= MIN_SIZE && r.size.height >= MIN_SIZE => finish(Some(r)),
                _ => redraw_all(),
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _e: &NSEvent) {
            finish(None);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            // Esc
            if e.keyCode() == 53 {
                finish(None);
            }
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            finish(None);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

impl SelectionView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }

    fn draw(&self) {
        let bounds = self.bounds();
        let origin = self
            .window()
            .map(|w| w.frame().origin)
            .unwrap_or(NSPoint::new(0.0, 0.0));
        let (start, current) = SESSION.with(|s| {
            s.borrow()
                .as_ref()
                .map(|s| (s.start, s.current))
                .unwrap_or((None, NSPoint::new(-1e9, -1e9)))
        });
        let to_local = |r: NSRect| {
            NSRect::new(
                NSPoint::new(r.origin.x - origin.x, r.origin.y - origin.y),
                r.size,
            )
        };
        let sel = start.map(|a| to_local(rect_from(a, current)));
        // Dim everything but the selection.
        let path = NSBezierPath::bezierPathWithRect(bounds);
        if let Some(r) = sel {
            path.appendBezierPathWithRect(r);
            path.setWindingRule(NSWindingRule::EvenOdd);
        }
        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.36).setFill();
        path.fill();
        let mouse_here = {
            let (x, y) = (current.x - origin.x, current.y - origin.y);
            x >= 0.0 && y >= 0.0 && x < bounds.size.width && y < bounds.size.height
        };
        match sel {
            Some(r) => {
                let border = NSBezierPath::bezierPathWithRect(NSRect::new(
                    NSPoint::new(r.origin.x - 0.75, r.origin.y - 0.75),
                    NSSize::new(r.size.width + 1.5, r.size.height + 1.5),
                ));
                border.setLineWidth(1.5);
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.95).setStroke();
                border.stroke();
                if mouse_here {
                    let label = format!("{} × {}", r.size.width.round(), r.size.height.round());
                    let below = r.origin.y - 34.0;
                    let y = if below > 8.0 {
                        below
                    } else {
                        r.origin.y + r.size.height + 8.0
                    };
                    draw_pill(&label, r.origin.x + r.size.width / 2.0, y, 12.0, bounds);
                }
            }
            None if mouse_here => {
                draw_pill(
                    "Drag over text to copy it  ·  Esc to cancel",
                    bounds.size.width / 2.0,
                    bounds.size.height - 120.0,
                    14.0,
                    bounds,
                );
            }
            None => {}
        }
    }
}

/// Whether a selection is on screen.
pub fn is_active() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

/// Show the overlay on every screen; `cb` gets the selection (global
/// top-left points) or `None` if cancelled. Main thread only.
pub fn select(mtm: MainThreadMarker, cb: SelectCallback) {
    if is_active() {
        return;
    }
    let previous_app = NSWorkspace::sharedWorkspace().frontmostApplication();
    let mouse = NSEvent::mouseLocation();
    let mut windows = Vec::new();
    let mut views = Vec::new();
    let mut key_index = 0;
    for (i, screen) in NSScreen::screens(mtm).iter().enumerate() {
        let frame = screen.frame();
        let window: Retained<OverlayWindow> = unsafe {
            msg_send![OverlayWindow::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false]
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setLevel(LEVEL);
        window.setAcceptsMouseMovedEvents(true);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        window.setFrame_display(frame, false);
        let view = SelectionView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
        window.setContentView(Some(&view));
        if mouse.x >= frame.origin.x
            && mouse.x < frame.origin.x + frame.size.width
            && mouse.y >= frame.origin.y
            && mouse.y < frame.origin.y + frame.size.height
        {
            key_index = i;
        }
        windows.push(window);
        views.push(view);
    }
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            windows: windows.clone(),
            views: views.clone(),
            start: None,
            current: mouse,
            cb: Some(cb),
            previous_app,
        })
    });
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    for (i, w) in windows.iter().enumerate() {
        if i == key_index {
            w.makeKeyAndOrderFront(None);
            w.makeFirstResponder(Some(&views[i]));
        } else {
            w.orderFrontRegardless();
        }
    }
    NSCursor::crosshairCursor().set();
}

/// Cancel a running selection (e.g. when stopping).
pub fn cancel() {
    finish(None);
}

/// Draw the overlay over a sample "desktop" into a PNG (offscreen; for
/// checking the look without showing windows or capturing the screen).
/// `sel` is (x, y, w, h) in view coordinates (origin bottom-left).
pub fn debug_render_png(
    mtm: MainThreadMarker,
    w: f64,
    h: f64,
    sel: Option<(f64, f64, f64, f64)>,
    path: &str,
) -> bool {
    use objc2_app_kit::{NSBitmapImageFileType, NSBox, NSBoxType, NSTextField, NSTitlePosition};
    let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
    let root = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    let bg = NSBox::initWithFrame(NSBox::alloc(mtm), bounds);
    bg.setBoxType(NSBoxType::Custom);
    bg.setTitlePosition(NSTitlePosition::NoTitle);
    bg.setBorderWidth(0.0);
    bg.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.96, 0.96, 0.97, 1.0,
    ));
    root.addSubview(&bg);
    for (i, line) in [
        "Quarterly report",
        "Revenue grew 42% year over year.",
        "Contact: hello@example.com",
    ]
    .iter()
    .enumerate()
    {
        let t = NSTextField::labelWithString(&NSString::from_str(line), mtm);
        t.setFont(Some(&NSFont::systemFontOfSize(if i == 0 {
            28.0
        } else {
            18.0
        })));
        t.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.1, 0.1, 0.12, 1.0,
        )));
        t.sizeToFit();
        let sz = t.frame().size;
        t.setFrame(NSRect::new(
            NSPoint::new(80.0, h - 140.0 - i as f64 * 44.0),
            sz,
        ));
        root.addSubview(&t);
    }
    let view = SelectionView::new(mtm, bounds);
    root.addSubview(&view);
    let (start, current) = match sel {
        Some((x, y, sw, sh)) => (Some(NSPoint::new(x, y)), NSPoint::new(x + sw, y + sh)),
        None => (None, NSPoint::new(w / 2.0, h / 2.0)),
    };
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            windows: vec![],
            views: vec![],
            start,
            current,
            cb: None,
            previous_app: None,
        })
    });
    let ok = (|| {
        let rep = root.bitmapImageRepForCachingDisplayInRect(bounds)?;
        root.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(data.writeToFile_atomically(&NSString::from_str(path), true))
    })()
    .unwrap_or(false);
    SESSION.with(|s| s.borrow_mut().take());
    ok
}
