//! Zone overlay: one borderless, transparent, click-through NSWindow per
//! monitor with an NSBox per zone. All AppKit work happens on the main
//! thread; [`show`]/[`hide`] can be called from any thread and post to the
//! main dispatch queue.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSScreen, NSTextAlignment, NSTextField, NSTitlePosition,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::ffi::dispatch_main;
use super::screens::global_to_cocoa;
use crate::engine::OverlayState;
use crate::geometry::Rect;

/// NSStatusWindowLevel — above normal and floating windows.
const OVERLAY_LEVEL: isize = 25;

struct MonitorOverlay {
    frame: Rect,
    window: Retained<NSWindow>,
    zones: Vec<Rect>,
    boxes: Vec<Retained<NSBox>>,
    show_numbers: bool,
}

thread_local! {
    static OVERLAYS: RefCell<Vec<MonitorOverlay>> = const { RefCell::new(Vec::new()) };
}

/// Show/update the overlay (any thread).
pub fn show(state: OverlayState) {
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            apply(mtm, &state);
        }
    });
}

/// Hide all overlay windows (any thread).
pub fn hide() {
    show(OverlayState::default());
}

/// Close and release all overlay windows (any thread).
pub fn destroy() {
    dispatch_main(|| {
        OVERLAYS.with(|o| {
            for ov in o.borrow_mut().drain(..) {
                ov.window.orderOut(None);
                ov.window.close();
            }
        });
    });
}

fn color(c: (u8, u8, u8), alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0 as f64 / 255.0, c.1 as f64 / 255.0, c.2 as f64 / 255.0, alpha)
}

fn primary_height(mtm: MainThreadMarker) -> f64 {
    let screens = NSScreen::screens(mtm);
    screens.iter().next().map(|s| s.frame().size.height).unwrap_or(0.0)
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

fn rebuild(mtm: MainThreadMarker, ov: &mut MonitorOverlay, zones: &[Rect], state: &OverlayState) {
    let Some(content) = ov.window.contentView() else { return };
    for v in content.subviews().iter() {
        v.removeFromSuperview();
    }
    ov.boxes.clear();
    let fh = ov.frame.height() as f64;
    for (i, z) in zones.iter().enumerate() {
        let local = NSRect::new(
            NSPoint::new((z.left - ov.frame.left) as f64, fh - (z.bottom - ov.frame.top) as f64),
            NSSize::new(z.width() as f64, z.height() as f64),
        );
        let b = NSBox::initWithFrame(NSBox::alloc(mtm), local);
        b.setBoxType(NSBoxType::Custom);
        b.setTitlePosition(NSTitlePosition::NoTitle);
        b.setBorderWidth(2.0);
        b.setCornerRadius(8.0);
        content.addSubview(&b);
        if state.show_numbers {
            let label = NSTextField::labelWithString(&NSString::from_str(&(i + 1).to_string()), mtm);
            let size = (z.height().min(z.width()) as f64 / 4.0).clamp(14.0, 72.0);
            label.setFont(Some(&NSFont::boldSystemFontOfSize(size)));
            label.setAlignment(NSTextAlignment::Center);
            label.setTextColor(Some(&color(state.number_color, 1.0)));
            label.sizeToFit();
            let ls: NSSize = label.frame().size;
            label.setFrame(NSRect::new(
                NSPoint::new(local.origin.x + (local.size.width - ls.width) / 2.0, local.origin.y + (local.size.height - ls.height) / 2.0),
                ls,
            ));
            content.addSubview(&label);
        }
        ov.boxes.push(b);
    }
    ov.zones = zones.to_vec();
    ov.show_numbers = state.show_numbers;
}

fn apply(mtm: MainThreadMarker, state: &OverlayState) {
    OVERLAYS.with(|cell| {
        let mut overlays = cell.borrow_mut();
        if state.monitors.is_empty() {
            for ov in overlays.iter() {
                ov.window.orderOut(None);
            }
            return;
        }
        let ph = primary_height(mtm);
        let mut used = vec![false; overlays.len()];
        for m in &state.monitors {
            let idx = match overlays.iter().position(|o| o.frame == m.frame) {
                Some(i) => i,
                None => {
                    let window = make_window(mtm, global_to_cocoa(m.frame, ph));
                    if window.contentView().is_none() {
                        let v = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(m.frame.width() as f64, m.frame.height() as f64)));
                        window.setContentView(Some(&v));
                    }
                    overlays.push(MonitorOverlay { frame: m.frame, window, zones: vec![], boxes: vec![], show_numbers: false });
                    used.push(false);
                    overlays.len() - 1
                }
            };
            used[idx] = true;
            let ov = &mut overlays[idx];
            if ov.zones != m.zones || ov.show_numbers != state.show_numbers || ov.boxes.len() != m.zones.len() {
                rebuild(mtm, ov, &m.zones, state);
            }
            let a = state.opacity.clamp(0.0, 1.0);
            for (i, b) in ov.boxes.iter().enumerate() {
                let hl = m.highlighted.contains(&i);
                let fill = if hl { color(state.highlight_color, a) } else { color(state.zone_color, a) };
                b.setFillColor(&fill);
                b.setBorderColor(&color(if hl { state.highlight_color } else { state.border_color }, 1.0));
            }
            ov.window.setFrame_display(global_to_cocoa(m.frame, ph), true);
            ov.window.orderFrontRegardless();
        }
        for (i, ov) in overlays.iter().enumerate() {
            if !used[i] {
                ov.window.orderOut(None);
            }
        }
    });
}
