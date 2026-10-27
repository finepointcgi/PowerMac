//! Run drag shelves with NSApplication on the main thread.
//!
//! ```text
//! cargo run -p drag-shelf --example drag_shelf_demo -- [--data DIR] [--render DIR] [--live DIR] [SECS]
//! ```
//! * `--data DIR`   shelf data dir (default: a temp folder)
//! * `--render DIR` open a few shelves with sample items in several states
//!   (one file, many files fanned, the grid, drop targets, a status
//!   message) and render each offscreen to PNGs under DIR
//! * `--live DIR`   end-to-end check in a test window of its own: posts a
//!   mouse-down in the window, drags a text snippet out of it, shakes, and
//!   drops the snippet on the shelf that opened; the pointer is put back.
//!   Writes a report and renders to DIR. Needs Accessibility (posting
//!   events) for the terminal.
//! * `SECS`         otherwise: keep a sample shelf open this long

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use drag_shelf::{Config, DragShelf, Handle, NewItem, Point};

struct Args {
    secs: f64,
    data: PathBuf,
    render: Option<PathBuf>,
    live: Option<PathBuf>,
}

fn args() -> Args {
    let mut a = Args { secs: 15.0, data: std::env::temp_dir().join("drag-shelf-demo"), render: None, live: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--data" => a.data = it.next().map(PathBuf::from).unwrap_or(a.data),
            "--render" => a.render = it.next().map(PathBuf::from),
            "--live" => a.live = it.next().map(PathBuf::from),
            n => a.secs = n.parse().unwrap_or(a.secs),
        }
    }
    a
}

/// Sample files in `dir/samples` (plus a system picture and an app).
fn samples(dir: &Path) -> Vec<NewItem> {
    let d = dir.join("samples");
    let _ = std::fs::create_dir_all(d.join("Vacation Photos"));
    let txt = d.join("Meeting notes.txt");
    let _ = std::fs::write(&txt, "Agenda\n\n1. Ship the drag shelf\n2. Review the notch widgets\n3. Plan the 1.2 release\n\nOwner: Ada\n");
    let md = d.join("README.md");
    let _ = std::fs::write(&md, "# Drag Shelf\n\nShake while dragging to park files.\n");
    let mut v = vec![];
    let pics: Vec<PathBuf> = std::fs::read_dir("/System/Library/Desktop Pictures/.thumbnails")
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "heic" || e == "jpg" || e == "png")).collect())
        .unwrap_or_default();
    for (k, p) in pics.iter().take(2).enumerate() {
        let to = d.join(format!("Wallpaper {}.{}", k + 1, p.extension().unwrap().to_string_lossy()));
        let _ = std::fs::copy(p, &to);
        v.push(NewItem::File(to));
    }
    v.push(NewItem::File(txt));
    v.push(NewItem::File(d.join("Vacation Photos")));
    if Path::new("/System/Applications/Safari.app").exists() {
        v.push(NewItem::File("/System/Applications/Safari.app".into()));
    } else if Path::new("/Applications/Safari.app").exists() {
        v.push(NewItem::File("/Applications/Safari.app".into()));
    }
    v.push(NewItem::File(md));
    v.push(NewItem::Url { url: "https://www.rust-lang.org/learn".into(), title: Some("Learn Rust".into()) });
    v.push(NewItem::Text("Remember to send the slides to the team before Friday's review".into()));
    v
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    pub fn run_app(work: impl FnOnce() + Send + 'static) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        std::thread::spawn(move || {
            work();
            std::process::exit(0);
        });
        app.run();
    }
}

fn wait(s: f64) {
    sleep(Duration::from_secs_f64(s));
}

/// Render all shelves into `dir` and name the PNGs (`names[n]` for
/// shelf n; `None` skips it).
fn shot(h: &Handle, dir: &Path, names: &[Option<&str>]) {
    let tmp = dir.join(".tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    h.render_png(tmp.clone());
    for _ in 0..50 {
        wait(0.05);
        if names.iter().enumerate().filter(|(_, n)| n.is_some()).all(|(k, _)| tmp.join(format!("drag-shelf-{k}.png")).exists()) {
            break;
        }
    }
    wait(0.1);
    for (k, n) in names.iter().enumerate() {
        if let Some(n) = n {
            let _ = std::fs::rename(tmp.join(format!("drag-shelf-{k}.png")), dir.join(format!("{n}.png")));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

fn render_states(h: &Handle, data: &Path, dir: &Path) {
    let items = samples(data);
    let n = items.len();
    h.debug("fresh");
    let one = h.new_shelf(Some(Point::new(260.0, 320.0)), vec![items[0].clone()]).unwrap();
    let _many = h.new_shelf(Some(Point::new(520.0, 320.0)), items[..6].to_vec()).unwrap();
    let _grid = h.new_shelf(Some(Point::new(900.0, 340.0)), items.clone()).unwrap();
    let _empty = h.new_shelf(Some(Point::new(260.0, 640.0)), vec![]).unwrap();
    let _target = h.new_shelf(Some(Point::new(520.0, 640.0)), items[1..4].to_vec()).unwrap();
    let _text = h.new_shelf(Some(Point::new(900.0, 700.0)), vec![items[1].clone(), items[n - 2].clone(), items[n - 1].clone()]).unwrap();
    let _link = h.new_shelf(Some(Point::new(1150.0, 700.0)), vec![items[0].clone(), items[n - 1].clone(), items[n - 2].clone()]).unwrap();
    let _ = one;
    wait(1.8); // Quick Look thumbnails
    h.debug("grid:2");
    wait(0.9);
    for (sub, accent) in [("system", "accent:system"), ("blue", "accent:blue")] {
        let d = dir.join(sub);
        let _ = std::fs::create_dir_all(&d);
        h.debug(accent);
        h.debug("noDropHover");
        h.debug("select:2:2");
        h.debug("hover:2:5");
        wait(0.5);
        shot(h, &d, &[Some("one-file"), Some("many-fanned"), Some("grid"), Some("empty"), Some("with-items"), Some("text-on-top"), Some("link-on-top")]);
        h.debug("dropHover:3");
        wait(0.4);
        shot(h, &d, &[None, None, None, Some("drop-target-empty"), None, None, None]);
        h.debug("noDropHover");
        h.debug("dropHover:4");
        wait(0.4);
        shot(h, &d, &[None, None, None, None, Some("drop-target-items"), None, None]);
        h.debug("noDropHover");
        h.debug("flash:1:Copied 6 items to Desktop");
        h.debug("hover:1:toggle");
        wait(0.4);
        shot(h, &d, &[None, Some("status-message"), None, None, None, None, None]);
        h.debug("flash:1:");
    }
    println!("rendered to {}", dir.display());
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = args();
    let _ = std::fs::remove_dir_all(&a.data);
    let cfg = Config { enabled: true, max_shelves: 10, ..Config::default() };
    #[cfg(target_os = "macos")]
    mac::run_app(move || {
        let h = DragShelf::start(cfg, a.data.clone()).expect("start");
        wait(0.5);
        if let Some(dir) = &a.render {
            render_states(&h, &a.data, dir);
        } else if let Some(dir) = &a.live {
            live::run(&h, dir);
        } else {
            h.new_shelf(Some(Point::new(500.0, 400.0)), samples(&a.data));
            wait(a.secs);
        }
        h.stop();
        wait(0.3);
    });
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, cfg);
        println!("drag shelves are macOS-only");
    }
}

#[cfg(target_os = "macos")]
mod live {
    //! The end-to-end check: a test window of our own that drags a text
    //! snippet when dragged in, synthetic mouse events to drag and shake,
    //! and a drop on the shelf that opened.

    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use drag_shelf::{Handle, ItemKind};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSBackingStoreType, NSColor, NSEvent, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

    use super::wait;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(src: *const c_void) -> *const c_void;
        fn CGEventGetLocation(e: *const c_void) -> CGPoint;
        fn CGEventCreateMouseEvent(src: *const c_void, ty: u32, p: CGPoint, button: u32) -> *const c_void;
        fn CGEventPost(tap: u32, e: *const c_void);
        fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
        fn CGEventSetIntegerValueField(e: *const c_void, field: u32, v: i64);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(p: *const c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
        fn CFDictionaryGetValue(d: *const c_void, k: *const c_void) -> *const c_void;
        fn CFBooleanGetValue(b: *const c_void) -> u8;
    }

    /// The login window / lock screen is up: posted events would go there.
    fn screen_locked() -> bool {
        unsafe {
            let d = CGSessionCopyCurrentDictionary();
            if d.is_null() {
                return true;
            }
            let key = NSString::from_str("CGSSessionScreenIsLocked");
            let v = CFDictionaryGetValue(d, Retained::as_ptr(&key) as *const c_void);
            let locked = !v.is_null() && CFBooleanGetValue(v) != 0;
            CFRelease(d);
            locked
        }
    }

    static DRAG_STARTED: AtomicBool = AtomicBool::new(false);
    static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);

    define_class!(
        /// Drags the text "PowerMac drag shelf live test" when dragged in.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        struct SourceView;

        impl SourceView {
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
                true
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, _e: &NSEvent) {
                MOUSE_DOWN.store(true, Ordering::SeqCst);
            }

            #[unsafe(method(mouseDragged:))]
            fn mouse_dragged(&self, e: &NSEvent) {
                if DRAG_STARTED.swap(true, Ordering::SeqCst) {
                    return;
                }
                unsafe {
                    let text = NSString::from_str("PowerMac drag shelf live test");
                    let a: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(NSDraggingItem), alloc];
                    let di: Retained<AnyObject> = msg_send![a, initWithPasteboardWriter: &*text];
                    let img: Option<Retained<AnyObject>> = msg_send![objc2::class!(NSImage), imageWithSystemSymbolName: &*NSString::from_str("text.quote"), accessibilityDescription: std::ptr::null::<AnyObject>()];
                    let p = self.convertPoint_fromView(e.locationInWindow(), None);
                    let frame = NSRect::new(NSPoint::new(p.x - 16.0, p.y - 16.0), NSSize::new(32.0, 32.0));
                    let _: () = msg_send![&*di, setDraggingFrame: frame, contents: img.as_deref().map(|i| i as *const AnyObject).unwrap_or(std::ptr::null())];
                    let arr = NSArray::from_retained_slice(&[di]);
                    let _: Retained<AnyObject> = msg_send![self, beginDraggingSessionWithItems: &*arr, event: e, source: self];
                }
            }

            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn source_mask(&self, _s: &AnyObject, _ctx: isize) -> usize {
                1
            }
        }
    );

    fn cursor() -> CGPoint {
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    fn post(ty: u32, p: CGPoint) {
        unsafe {
            let e = CGEventCreateMouseEvent(std::ptr::null(), ty, p, 0);
            // kCGMouseEventClickState = 1
            CGEventSetIntegerValueField(e, 1, 1);
            CGEventPost(0, e); // kCGHIDEventTap
            CFRelease(e);
        }
    }

    const DOWN: u32 = 1;
    const UP: u32 = 2;
    const DRAGGED: u32 = 6;

    pub fn run(h: &Handle, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let mut report = String::new();
        if screen_locked() {
            println!("the screen is locked: not posting any events (live test skipped)");
            let _ = std::fs::write(dir.join("live-report.txt"), "skipped: screen locked\n");
            return;
        }
        let trusted = unsafe { AXIsProcessTrusted() };
        for _ in 0..40 {
            if pm_hook::status().is_active() {
                break;
            }
            wait(0.1);
        }
        report.push_str(&format!("accessibility: {trusted}; input hook: {:?}; listening: {}\n", pm_hook::status().state, h.listening()));
        let ok = Arc::new(AtomicBool::new(false));
        // The test window (on the main thread), away from the menu bar.
        let (tx, rx) = std::sync::mpsc::channel::<(f64, f64, f64, f64)>();
        pm_shelf_core::mac::ffi::dispatch_main(move || {
            let mtm = MainThreadMarker::new().unwrap();
            let screens = objc2_app_kit::NSScreen::screens(mtm);
            let main = screens.iter().next().unwrap();
            let f = main.frame();
            let (w, hh) = (420.0, 260.0);
            let x = f.origin.x + f.size.width / 2.0 - w / 2.0;
            let y = f.origin.y + f.size.height / 2.0 - hh / 2.0 - 80.0;
            let win = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(x, y), NSSize::new(w, hh)),
                    NSWindowStyleMask::Titled,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { win.setReleasedWhenClosed(false) };
            win.setTitle(&NSString::from_str("Drag Shelf live test"));
            win.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
            let this = SourceView::alloc(mtm).set_ivars(());
            let v: Retained<SourceView> = unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, hh))] };
            let nv: &NSView = &v;
            win.setContentView(Some(nv));
            win.setLevel(19);
            win.orderFrontRegardless();
            std::mem::forget(win);
            std::mem::forget(v);
            let ph = f.size.height;
            let _ = tx.send((x, ph - (y + hh), w, hh));
        });
        let (wx, wy, ww, wh) = rx.recv().unwrap();
        wait(0.6);
        let saved = cursor();
        let start = CGPoint { x: wx + ww / 2.0, y: wy + wh / 2.0 + 20.0 };
        report.push_str(&format!("test window at ({wx:.0}, {wy:.0}) {ww:.0}x{wh:.0}; pointer was at ({:.0}, {:.0})\n", saved.x, saved.y));
        let before = h.shelves().len();
        unsafe { CGWarpMouseCursorPosition(start) };
        wait(0.15);
        post(DOWN, start);
        wait(0.08);
        // Drag a little to start the drag session.
        let mut p = start;
        for k in 1..=8 {
            p = CGPoint { x: start.x + k as f64 * 3.0, y: start.y };
            post(DRAGGED, p);
            wait(0.016);
        }
        wait(0.25);
        report.push_str(&format!("mouse down reached the test window: {}\n", MOUSE_DOWN.load(Ordering::SeqCst)));
        report.push_str(&format!("drag session started in the test window: {}\n", DRAG_STARTED.load(Ordering::SeqCst)));
        // Shake: 6 strokes of 130 pt, 70 ms each.
        let base = p.x;
        for s in 0..6 {
            let (a, b) = if s % 2 == 0 { (base, base + 130.0) } else { (base + 130.0, base) };
            for k in 1..=7 {
                let x = a + (b - a) * k as f64 / 7.0;
                p = CGPoint { x, y: start.y };
                post(DRAGGED, p);
                wait(0.010);
            }
        }
        // Give the shelf time to open, then wiggle onto it and release.
        wait(0.6);
        let after = h.shelves();
        let opened = after.len() > before;
        report.push_str(&format!("shelf opened by the shake: {opened} ({} -> {} shelves)\n", before, after.len()));
        if let Some(s) = after.last() {
            let c = CGPoint { x: s.origin.x + 104.0, y: s.origin.y + 118.0 };
            for k in 1..=10 {
                let q = CGPoint { x: p.x + (c.x - p.x) * k as f64 / 10.0, y: p.y + (c.y - p.y) * k as f64 / 10.0 };
                post(DRAGGED, q);
                wait(0.02);
            }
            wait(0.3);
            post(UP, c);
        } else {
            post(UP, p);
        }
        wait(0.8);
        unsafe { CGWarpMouseCursorPosition(saved) };
        let shelves = h.shelves();
        let dropped = shelves.last().is_some_and(|s| s.items.iter().any(|i| matches!(&i.kind, ItemKind::Text { text } if text.contains("live test"))));
        report.push_str(&format!("text dropped on the new shelf: {dropped}\n"));
        ok.store(opened && dropped, Ordering::SeqCst);
        h.render_png(dir.join("live"));
        wait(0.6);
        let end = cursor();
        report.push_str(&format!("pointer restored to ({:.0}, {:.0})\n", end.x, end.y));
        report.push_str(&format!("result: {}\n", if ok.load(Ordering::SeqCst) { "PASS" } else { "FAIL" }));
        print!("{report}");
        let _ = std::fs::write(dir.join("live-report.txt"), report);
    }
}
