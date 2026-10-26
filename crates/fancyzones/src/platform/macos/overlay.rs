//! Zone overlay: one borderless, transparent, click-through NSWindow per
//! monitor with an NSBox per zone. All AppKit work happens on the main
//! thread; [`show`]/[`hide`] can be called from any thread and post to the
//! main dispatch queue.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSScreen, NSTextAlignment, NSTextField, NSTitlePosition,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::ffi::dispatch_main;
use super::screens::global_to_cocoa;
use crate::engine::{LabelState, OverlayState};
use crate::geometry::Rect;

/// NSStatusWindowLevel — above normal and floating windows.
const OVERLAY_LEVEL: isize = 25;

struct MonitorOverlay {
    frame: Rect,
    window: Retained<NSWindow>,
    zones: Vec<Rect>,
    boxes: Vec<Retained<NSBox>>,
    show_numbers: bool,
    /// The number labels' color (they are rebuilt when it changes).
    number_color: (u8, u8, u8),
}

thread_local! {
    static OVERLAYS: RefCell<Vec<MonitorOverlay>> = const { RefCell::new(Vec::new()) };
    static LABEL: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}

/// Bumped by every [`show`]; a delayed hide only applies if unchanged.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static LABEL_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Show `state` for `ms` milliseconds (zone flash), unless something else
/// is shown meanwhile.
pub fn flash(state: OverlayState, ms: u64) {
    show(state);
    let g = GENERATION.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        dispatch_main(move || {
            if GENERATION.load(Ordering::SeqCst) == g {
                if let Some(mtm) = MainThreadMarker::new() {
                    apply(mtm, &OverlayState::default());
                }
            }
        });
    });
}

/// Show the layout name label (any thread); it fades out after its duration.
pub fn show_label(label: LabelState) {
    let g = LABEL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let total = label.duration_ms as u64;
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            apply_label(mtm, &label);
        }
    });
    std::thread::spawn(move || {
        let fade = (total / 3).min(300);
        std::thread::sleep(Duration::from_millis(total - fade));
        const STEPS: u64 = 6;
        for i in 1..=STEPS {
            if LABEL_GENERATION.load(Ordering::SeqCst) != g {
                return;
            }
            let alpha = 1.0 - i as f64 / STEPS as f64;
            dispatch_main(move || {
                if LABEL_GENERATION.load(Ordering::SeqCst) != g {
                    return;
                }
                LABEL.with(|l| {
                    if let Some(w) = l.borrow().as_ref() {
                        if alpha <= 0.0 {
                            w.orderOut(None);
                        } else {
                            w.setAlphaValue(alpha);
                        }
                    }
                });
            });
            std::thread::sleep(Duration::from_millis(fade / STEPS));
        }
    });
}

fn apply_label(mtm: MainThreadMarker, l: &LabelState) {
    let text = NSTextField::labelWithString(&NSString::from_str(&l.text), mtm);
    text.setFont(Some(&NSFont::boldSystemFontOfSize(l.font_size as f64)));
    text.setTextColor(Some(&color(l.text_color, 1.0)));
    text.setAlignment(NSTextAlignment::Center);
    text.sizeToFit();
    let ts: NSSize = text.frame().size;
    let max_w = (l.area.width() as f64 * 0.96 - 2.0 * l.padding as f64).max(1.0);
    let tw = ts.width.min(max_w);
    let pad = l.padding as f64;
    let (w, h) = ((tw + 2.0 * pad).ceil() as i32, (ts.height + 2.0 * pad).ceil() as i32);
    let origin = l.chip_origin(w, h);
    let global = Rect::from_xywh(origin.x, origin.y, w, h);
    let frame = global_to_cocoa(global, primary_height(mtm));
    LABEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let window = cell.get_or_insert_with(|| make_window(mtm, frame)).clone();
        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64)));
        let chip = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64)));
        chip.setBoxType(NSBoxType::Custom);
        chip.setTitlePosition(NSTitlePosition::NoTitle);
        chip.setBorderWidth(0.0);
        chip.setCornerRadius(8.0);
        chip.setFillColor(&color(l.background_color, 1.0));
        content.addSubview(&chip);
        text.setFrame(NSRect::new(NSPoint::new(pad, pad), NSSize::new(tw, ts.height)));
        content.addSubview(&text);
        window.setContentView(Some(&content));
        window.setFrame_display(frame, true);
        window.setAlphaValue(1.0);
        window.orderFrontRegardless();
    });
}

/// Show/update the overlay (any thread).
pub fn show(state: OverlayState) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
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
    LABEL_GENERATION.fetch_add(1, Ordering::SeqCst);
    dispatch_main(|| {
        LABEL.with(|l| {
            if let Some(w) = l.borrow_mut().take() {
                w.orderOut(None);
                w.close();
            }
        });
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
    ov.number_color = state.number_color;
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
                    overlays.push(MonitorOverlay {
                        frame: m.frame,
                        window,
                        zones: vec![],
                        boxes: vec![],
                        show_numbers: false,
                        number_color: (0, 0, 0),
                    });
                    used.push(false);
                    overlays.len() - 1
                }
            };
            used[idx] = true;
            let ov = &mut overlays[idx];
            if ov.zones != m.zones
                || ov.show_numbers != state.show_numbers
                || (state.show_numbers && ov.number_color != state.number_color)
                || ov.boxes.len() != m.zones.len()
            {
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
