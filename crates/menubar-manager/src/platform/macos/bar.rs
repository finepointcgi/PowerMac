//! The floating bar ("Ice Bar"): a small vibrant panel under the menu bar
//! showing images of the hidden items. Clicking one shows its section in
//! place for a moment and presses the real item.
//!
//! Images: our own items are rendered from their views; other apps' items
//! are captured from their status windows (`CGWindowListCreateImage`, needs
//! Screen Recording, macOS 15 and older, where items are windows); on macOS
//! 26+ (no windows) or without the permission, the owning app's icon is
//! shown instead.

use std::cell::RefCell;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::app::{self, status_button};
use super::ffi::*;
use crate::{MenuBarItem, Rect};

pub struct BarEntry {
    pub item: MenuBarItem,
    /// +1 CGImageRef (as usize so it can cross threads).
    pub cg_image: Option<usize>,
}

pub struct Ivars {
    slots: RefCell<Vec<(f64, f64, String)>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PowerMacMenuBarBarView"]
    #[ivars = Ivars]
    pub struct BarView;

    impl BarView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&AnyObject>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &AnyObject) {
            let p: NSPoint = unsafe { msg_send![e, locationInWindow] };
            let p: NSPoint = unsafe { msg_send![self, convertPoint: p, fromView: std::ptr::null::<AnyObject>()] };
            let id = self.ivars().slots.borrow().iter().find(|(x0, x1, _)| p.x >= *x0 && p.x < *x1).map(|s| s.2.clone());
            if let Some(id) = id {
                app::with(|a| a.bar_clicked(id));
            }
        }
    }
);

/// (image, width, height, item id, tooltip)
type Slot = (Retained<AnyObject>, f64, f64, String, Option<String>);

pub struct Bar {
    panel: Retained<AnyObject>,
    frame: Rect,
}

const H: f64 = 30.0;
const GAP: f64 = 4.0;
const MARGIN: f64 = 6.0;

fn own_item_image(own_windows: &[Retained<AnyObject>], item: &MenuBarItem, ph: f64) -> Option<Retained<AnyObject>> {
    let _ = ph;
    for w in own_windows {
        let p = Retained::as_ptr(w) as *mut AnyObject;
        let Some(b) = status_button(p) else { continue };
        let f: NSRect = unsafe { msg_send![p, frame] };
        let title: Option<Retained<objc2_foundation::NSString>> = unsafe { msg_send![b, title] };
        let same_title = item.title.is_some() && title.map(|t| t.to_string()) == item.title;
        let same_frame = (f.origin.x - item.frame.x).abs() < 2.0 && (f.size.width - item.frame.w).abs() < 2.0;
        if !(same_title || same_frame) {
            continue;
        }
        unsafe {
            let bounds: NSRect = msg_send![b, bounds];
            if bounds.size.width < 1.0 {
                return None;
            }
            let rep: Option<Retained<AnyObject>> = msg_send![b, bitmapImageRepForCachingDisplayInRect: bounds];
            let rep = rep?;
            let _: () = msg_send![b, cacheDisplayInRect: bounds, toBitmapImageRep: &*rep];
            let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
            let img: Retained<AnyObject> = msg_send![a, initWithSize: bounds.size];
            let _: () = msg_send![&*img, addRepresentation: &*rep];
            return Some(img);
        }
    }
    None
}

fn cg_to_nsimage(p: usize, size: NSSize) -> Option<Retained<AnyObject>> {
    unsafe {
        let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        let img: Option<Retained<AnyObject>> = msg_send![a, initWithCGImage: p as *const c_void, size: size];
        CGImageRelease(p as *const c_void);
        img
    }
}

fn app_icon(pid: i32) -> Option<Retained<AnyObject>> {
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        let icon: Option<Retained<AnyObject>> = msg_send![&*app?, icon];
        icon
    }
}

impl Bar {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        mtm: MainThreadMarker,
        entries: Vec<BarEntry>,
        own_windows: &[Retained<AnyObject>],
        anchor: Rect,
        screen: Rect,
        menu_bar_h: f64,
        ph: f64,
        appearance: *mut AnyObject,
    ) -> Option<Bar> {
        // Images and slot widths.
        let mut slots: Vec<Slot> = vec![];
        for e in entries {
            let item_size = NSSize::new(e.item.frame.w.max(16.0), e.item.frame.h.clamp(16.0, 26.0));
            let (img, w, h) = if let Some(p) = e.cg_image {
                match cg_to_nsimage(p, NSSize::new(e.item.frame.w, e.item.frame.h)) {
                    Some(i) => (Some(i), item_size.width, item_size.height),
                    None => (None, 0.0, 0.0),
                }
            } else if e.item.own {
                match own_item_image(own_windows, &e.item, ph) {
                    Some(i) => {
                        let s: NSSize = unsafe { msg_send![&*i, size] };
                        (Some(i), s.width, s.height.min(26.0))
                    }
                    None => (None, 0.0, 0.0),
                }
            } else {
                (None, 0.0, 0.0)
            };
            let (img, w, h) = match img {
                Some(i) => (i, w, h),
                None => match app_icon(e.item.pid) {
                    Some(i) => (i, 20.0, 20.0),
                    None => continue,
                },
            };
            slots.push((img, w, h, e.item.id, e.item.title.or(Some(e.item.owner))));
        }
        let content_w: f64 = if slots.is_empty() { 150.0 } else { slots.iter().map(|s| s.1).sum::<f64>() + GAP * (slots.len() as f64 - 1.0) };
        let w = content_w + 2.0 * MARGIN;
        let x = (anchor.right() - w).clamp(screen.x + 4.0, (screen.right() - w - 4.0).max(screen.x + 4.0));
        let top = screen.y + menu_bar_h + 4.0;
        let frame = Rect::new(x, top, w, H);
        let ns_frame = NSRect::new(NSPoint::new(x, ph - (top + H)), NSSize::new(w, H));
        unsafe {
            // Borderless | NonactivatingPanel.
            let style: usize = 1 << 7;
            let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSPanel), alloc];
            let panel: Retained<AnyObject> = msg_send![a, initWithContentRect: ns_frame, styleMask: style, backing: 2usize, defer: false];
            let _: () = msg_send![&*panel, setReleasedWhenClosed: false];
            let _: () = msg_send![&*panel, setFloatingPanel: true];
            // NSStatusWindowLevel (setFloatingPanel resets the level).
            let _: () = msg_send![&*panel, setLevel: 25isize];
            let _: () = msg_send![&*panel, setOpaque: false];
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![&*panel, setBackgroundColor: clear];
            let _: () = msg_send![&*panel, setHasShadow: true];
            let _: () = msg_send![&*panel, setHidesOnDeactivate: false];
            let _: () = msg_send![&*panel, setBecomesKeyOnlyIfNeeded: true];
            // canJoinAllSpaces | stationary | fullScreenAuxiliary
            let _: () = msg_send![&*panel, setCollectionBehavior: (1usize << 0) | (1usize << 4) | (1usize << 8)];
            if !appearance.is_null() {
                let _: () = msg_send![&*panel, setAppearance: appearance];
            }
            let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, H));
            let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSVisualEffectView), alloc];
            let fx: Retained<AnyObject> = msg_send![a, initWithFrame: bounds];
            // NSVisualEffectMaterialMenu, behind window, active.
            let _: () = msg_send![&*fx, setMaterial: 5isize];
            let _: () = msg_send![&*fx, setBlendingMode: 0isize];
            let _: () = msg_send![&*fx, setState: 1isize];
            let _: () = msg_send![&*fx, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![&*fx, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: 8.0f64];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            let view = BarView::alloc(mtm).set_ivars(Ivars { slots: RefCell::new(vec![]) });
            let view: Retained<BarView> = msg_send![super(view), initWithFrame: bounds];
            let mut xs = vec![];
            let mut cx = MARGIN;
            for (img, iw, ih, id, title) in &slots {
                let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImageView), alloc];
                let r = NSRect::new(NSPoint::new(cx, (H - ih) / 2.0), NSSize::new(*iw, *ih));
                let iv: Retained<AnyObject> = msg_send![a, initWithFrame: r];
                let _: () = msg_send![&*iv, setImage: &**img];
                // NSImageScaleProportionallyUpOrDown
                let _: () = msg_send![&*iv, setImageScaling: 3isize];
                if let Some(t) = title {
                    let _: () = msg_send![&*iv, setToolTip: &*ns(t)];
                }
                // Clicks go to the bar view, not the image view.
                let _: () = msg_send![&*view, addSubview: &*iv];
                let _: () = msg_send![&*iv, setEnabled: false];
                xs.push((cx, cx + iw, id.clone()));
                cx += iw + GAP;
            }
            if slots.is_empty() {
                let label: Retained<AnyObject> = msg_send![class!(NSTextField), labelWithString: &*ns("No hidden icons")];
                let _: () = msg_send![&*label, setFrame: NSRect::new(NSPoint::new(MARGIN, 7.0), NSSize::new(content_w, 16.0))];
                let sec: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
                let _: () = msg_send![&*label, setTextColor: sec];
                let _: () = msg_send![&*view, addSubview: &*label];
            }
            *view.ivars().slots.borrow_mut() = xs;
            let _: () = msg_send![&*fx, addSubview: &*view];
            let _: () = msg_send![&*panel, setContentView: &*fx];
            let _: () = msg_send![&*panel, setAlphaValue: 0.0f64];
            let _: () = msg_send![&*panel, orderFrontRegardless];
            // Fade in.
            let ctx: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
            let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
            let _: () = msg_send![ctx, setDuration: 0.12f64];
            let anim: *mut AnyObject = msg_send![&*panel, animator];
            let _: () = msg_send![anim, setAlphaValue: 1.0f64];
            let _: () = msg_send![class!(NSAnimationContext), endGrouping];
            Some(Bar { panel, frame })
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        self.frame.contains(x, y)
    }

    pub fn close(self) {
        unsafe {
            let _: () = msg_send![&*self.panel, orderOut: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.panel, close];
        }
    }

    /// The bar window's number (for capturing it in tests).
    #[allow(dead_code)]
    pub fn window_number(&self) -> isize {
        unsafe { msg_send![&*self.panel, windowNumber] }
    }
}
