//! A small HUD ("Copied 42 words") near the bottom of the screen with the
//! pointer; it fades out after a moment. Callable from any thread.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSImage, NSImageView, NSScreen,
    NSTextField, NSTitlePosition, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::main_thread::dispatch_main;

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}
static GENERATION: AtomicU64 = AtomicU64::new(0);

const SHOW_MS: u64 = 1700;

pub fn show(text: String) {
    let g = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    dispatch_main(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            present(mtm, &text);
        }
    });
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(SHOW_MS));
        const STEPS: u64 = 8;
        for i in 1..=STEPS {
            if GENERATION.load(Ordering::SeqCst) != g {
                return;
            }
            let alpha = 1.0 - i as f64 / STEPS as f64;
            dispatch_main(move || {
                if GENERATION.load(Ordering::SeqCst) != g {
                    return;
                }
                WINDOW.with(|w| {
                    if let Some(w) = w.borrow().as_ref() {
                        if alpha <= 0.0 {
                            w.orderOut(None);
                        } else {
                            w.setAlphaValue(alpha);
                        }
                    }
                });
            });
            std::thread::sleep(Duration::from_millis(25));
        }
    });
}

/// The toast's content view and size.
fn build(mtm: MainThreadMarker, text: &str) -> (Retained<NSView>, f64, f64) {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize_weight(14.0, unsafe {
        objc2_app_kit::NSFontWeightMedium
    })));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.sizeToFit();
    let ts = label.frame().size;
    let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("text.viewfinder"),
        None,
    );
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let (pad_x, pad_y) = (18.0, 11.0);
    let w = (ts.width + icon_w + 2.0 * pad_x).ceil();
    let h = (ts.height.max(18.0) + 2.0 * pad_y).ceil();
    let content = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
    );
    let bg = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
    );
    bg.setBoxType(NSBoxType::Custom);
    bg.setTitlePosition(NSTitlePosition::NoTitle);
    bg.setBorderWidth(0.0);
    bg.setCornerRadius(h / 2.0);
    bg.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.11, 0.11, 0.13, 0.9,
    ));
    content.addSubview(&bg);
    let mut x = pad_x;
    if let Some(icon) = icon {
        let iv = NSImageView::imageViewWithImage(&icon, mtm);
        iv.setContentTintColor(Some(&NSColor::whiteColor()));
        iv.setFrame(NSRect::new(
            NSPoint::new(x, (h - 18.0) / 2.0),
            NSSize::new(18.0, 18.0),
        ));
        content.addSubview(&iv);
        x += icon_w;
    }
    label.setFrame(NSRect::new(NSPoint::new(x, (h - ts.height) / 2.0), ts));
    content.addSubview(&label);
    (content, w, h)
}

fn present(mtm: MainThreadMarker, text: &str) {
    let (content, w, h) = build(mtm, text);

    // The screen with the pointer.
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let screen = screens
        .iter()
        .find(|s| {
            let f = s.frame();
            mouse.x >= f.origin.x
                && mouse.x < f.origin.x + f.size.width
                && mouse.y >= f.origin.y
                && mouse.y < f.origin.y + f.size.height
        })
        .or_else(|| screens.iter().next());
    let Some(screen) = screen else { return };
    let vf = screen.visibleFrame();
    let frame = NSRect::new(
        NSPoint::new(vf.origin.x + (vf.size.width - w) / 2.0, vf.origin.y + 90.0),
        NSSize::new(w, h),
    );

    WINDOW.with(|cell| {
        let mut cell = cell.borrow_mut();
        let window = cell
            .get_or_insert_with(|| {
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
                window.setHasShadow(true);
                window.setLevel(1001);
                window.setCollectionBehavior(
                    NSWindowCollectionBehavior::CanJoinAllSpaces
                        | NSWindowCollectionBehavior::Stationary
                        | NSWindowCollectionBehavior::IgnoresCycle
                        | NSWindowCollectionBehavior::FullScreenAuxiliary,
                );
                window
            })
            .clone();
        window.setContentView(Some(&content));
        window.setFrame_display(frame, true);
        window.setAlphaValue(1.0);
        window.orderFrontRegardless();
    });
}

/// Render the toast into a PNG over a light background (offscreen check).
pub fn debug_render_png(mtm: MainThreadMarker, text: &str, path: &str) -> bool {
    use objc2_app_kit::NSBitmapImageFileType;
    let (content, w, h) = build(mtm, text);
    let pad = 24.0;
    let bounds = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(w + 2.0 * pad, h + 2.0 * pad),
    );
    let root = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    let bg = NSBox::initWithFrame(NSBox::alloc(mtm), bounds);
    bg.setBoxType(NSBoxType::Custom);
    bg.setTitlePosition(NSTitlePosition::NoTitle);
    bg.setBorderWidth(0.0);
    bg.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.93, 0.93, 0.95, 1.0,
    ));
    root.addSubview(&bg);
    content.setFrameOrigin(NSPoint::new(pad, pad));
    root.addSubview(&content);
    (|| {
        let rep = root.bitmapImageRepForCachingDisplayInRect(bounds)?;
        root.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let data = unsafe {
            rep.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &objc2_foundation::NSDictionary::new(),
            )
        }?;
        Some(data.writeToFile_atomically(&NSString::from_str(path), true))
    })()
    .unwrap_or(false)
}
