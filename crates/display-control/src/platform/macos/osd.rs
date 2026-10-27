//! The on-screen level indicator: a borderless, non-activating, translucent
//! HUD panel (rounded, vibrancy material) with an SF Symbol icon and a level
//! bar, shown near the bottom of the target display and faded out. All
//! AppKit work runs on the main queue; [`show`] can be called from any
//! thread (the shared event tap thread).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSImage, NSImageScaling, NSImageView, NSPanel, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::ffi::{dispatch_main, CGDisplayBounds, CGMainDisplayID};
use crate::engine::Feedback;
use crate::OsdKind;

const SIZE: f64 = 200.0;
const BAR_X: f64 = 24.0;
const BAR_Y: f64 = 28.0;
const BAR_W: f64 = SIZE - 2.0 * BAR_X;
const BAR_H: f64 = 7.0;
/// NSPopUpMenuWindowLevel: above normal, floating and status windows.
const LEVEL: isize = 101;
const VISIBLE_MS: u64 = 1300;
const FADE_MS: u64 = 300;

struct Osd {
    panel: Retained<NSPanel>,
    icon: Retained<NSImageView>,
    fill: Retained<NSBox>,
}

thread_local! {
    static OSD: RefCell<Option<Osd>> = const { RefCell::new(None) };
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Show the OSD for `fb` (any thread).
pub(crate) fn show(fb: Feedback) {
    let g = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            apply(mtm, &fb);
        }
    });
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(VISIBLE_MS));
        const STEPS: u64 = 6;
        for i in 1..=STEPS {
            if GENERATION.load(Ordering::SeqCst) != g {
                return;
            }
            let alpha = 1.0 - i as f64 / STEPS as f64;
            dispatch_main(move || {
                if GENERATION.load(Ordering::SeqCst) != g {
                    return;
                }
                OSD.with(|o| {
                    if let Some(o) = o.borrow().as_ref() {
                        if alpha <= 0.0 {
                            o.panel.orderOut(None);
                        } else {
                            o.panel.setAlphaValue(alpha);
                        }
                    }
                });
            });
            std::thread::sleep(Duration::from_millis(FADE_MS / STEPS));
        }
    });
}

/// Close and release the panel (any thread).
pub(crate) fn close() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    dispatch_main(|| {
        OSD.with(|o| {
            if let Some(o) = o.borrow_mut().take() {
                o.panel.orderOut(None);
                o.panel.close();
            }
        });
    });
}

fn symbol(kind: OsdKind, pct: u8) -> &'static str {
    match kind {
        OsdKind::Brightness if pct < 35 => "sun.min.fill",
        OsdKind::Brightness => "sun.max.fill",
        OsdKind::Contrast => "circle.lefthalf.filled",
        OsdKind::Muted => "speaker.slash.fill",
        OsdKind::Volume if pct == 0 => "speaker.fill",
        OsdKind::Volume if pct < 34 => "speaker.wave.1.fill",
        OsdKind::Volume if pct < 67 => "speaker.wave.2.fill",
        OsdKind::Volume => "speaker.wave.3.fill",
    }
}

fn rgba(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)
}

fn make_box(mtm: MainThreadMarker, frame: NSRect, fill: &NSColor) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setCornerRadius(BAR_H / 2.0);
    b.setFillColor(fill);
    b
}

fn build(mtm: MainThreadMarker) -> Osd {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIZE, SIZE));
    let panel: Retained<NSPanel> = unsafe {
        msg_send![
            NSPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setIgnoresMouseEvents(true);
    panel.setHasShadow(false);
    panel.setLevel(LEVEL);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );

    let fx = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    fx.setMaterial(NSVisualEffectMaterial::HUDWindow);
    fx.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    fx.setState(NSVisualEffectState::Active);
    fx.setWantsLayer(true);
    unsafe {
        let layer: *mut AnyObject = msg_send![&*fx, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: 18.0f64];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }

    let icon = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::new((SIZE - 96.0) / 2.0, 66.0), NSSize::new(96.0, 96.0)));
    icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    icon.setContentTintColor(Some(&rgba(1.0, 1.0, 1.0, 0.9)));
    fx.addSubview(&icon);

    let track = make_box(mtm, NSRect::new(NSPoint::new(BAR_X, BAR_Y), NSSize::new(BAR_W, BAR_H)), &rgba(1.0, 1.0, 1.0, 0.22));
    fx.addSubview(&track);
    let fill = make_box(mtm, NSRect::new(NSPoint::new(BAR_X, BAR_Y), NSSize::new(0.0, BAR_H)), &rgba(1.0, 1.0, 1.0, 0.92));
    fx.addSubview(&fill);

    let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
    content.addSubview(&fx);
    panel.setContentView(Some(&content));
    Osd { panel, icon, fill }
}

fn apply(mtm: MainThreadMarker, fb: &Feedback) {
    OSD.with(|cell| {
        let mut cell = cell.borrow_mut();
        let osd = cell.get_or_insert_with(|| build(mtm));
        if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(symbol(fb.kind, fb.pct)), None) {
            osd.icon.setImage(Some(&img));
        }
        let pct = if fb.kind == OsdKind::Muted { 0 } else { fb.pct.min(100) };
        osd.fill.setFrame(NSRect::new(NSPoint::new(BAR_X, BAR_Y), NSSize::new(BAR_W * pct as f64 / 100.0, BAR_H)));
        osd.fill.setHidden(pct == 0);
        // Global top-left → Cocoa bottom-left coordinates.
        let primary_h = unsafe { CGDisplayBounds(CGMainDisplayID()) }.size.height;
        let b = fb.bounds;
        let x = b.x as f64 + (b.width as f64 - SIZE) / 2.0;
        let top = b.y as f64 + b.height as f64 * 0.86 - SIZE;
        let y = primary_h - (top + SIZE);
        osd.panel.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(SIZE, SIZE)), true);
        osd.panel.setAlphaValue(1.0);
        osd.panel.orderFrontRegardless();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols() {
        assert_eq!(symbol(OsdKind::Brightness, 10), "sun.min.fill");
        assert_eq!(symbol(OsdKind::Brightness, 90), "sun.max.fill");
        assert_eq!(symbol(OsdKind::Volume, 0), "speaker.fill");
        assert_eq!(symbol(OsdKind::Volume, 50), "speaker.wave.2.fill");
        assert_eq!(symbol(OsdKind::Muted, 50), "speaker.slash.fill");
    }
}
