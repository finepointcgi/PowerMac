//! Run the notch shelf for N seconds with NSApplication on the main thread.
//!
//! ```text
//! cargo run -p notch-shelf --example shelf_demo -- [SECS] [--data DIR] [--sample]
//!     [--render DIR] [--hover] [--virtual]
//! ```
//! * `--data DIR`   shelf data dir (default: a temp folder)
//! * `--sample`     put a few sample files / a link / a text snippet on it
//! * `--render DIR` script the states (expanded, drop target, collapsed,
//!   battery peek) and render each panel offscreen to PNGs in DIR
//! * `--hover`      move the pointer onto the notch, render, move it back
//! * `--virtual`    also put a virtual notch on screens without one
//! * `--widgets DIR` render the Home view (widgets) with demo data in
//!   several states (music, timer, calendar, weather, stats, toggles, live
//!   activities) to PNGs in DIR. Uses fake data only: no permissions, no
//!   network, no playback control.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use notch_shelf::{Config, NewItem, NotchShelf, QuickAction, QuickToggle, ScreenMode, StartOptions, View, WidgetConfig, WidgetKind};

struct Args {
    secs: f64,
    data: PathBuf,
    sample: bool,
    render: Option<PathBuf>,
    hover: bool,
    virt: bool,
    widgets: Option<PathBuf>,
    live: Option<PathBuf>,
}

fn args() -> Args {
    let mut a = Args { secs: 20.0, data: std::env::temp_dir().join("notch-shelf-demo"), sample: false, render: None, hover: false, virt: false, widgets: None, live: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--data" => a.data = it.next().map(PathBuf::from).unwrap_or(a.data),
            "--sample" => a.sample = true,
            "--render" => a.render = it.next().map(PathBuf::from),
            "--hover" => a.hover = true,
            "--virtual" => a.virt = true,
            "--widgets" => a.widgets = it.next().map(PathBuf::from),
            "--live" => a.live = it.next().map(PathBuf::from),
            n => a.secs = n.parse().unwrap_or(a.secs),
        }
    }
    a
}

fn samples(dir: &std::path::Path) -> Vec<NewItem> {
    let d = dir.join("samples");
    let _ = std::fs::create_dir_all(&d);
    let txt = d.join("Meeting notes.txt");
    let _ = std::fs::write(&txt, "Notch shelf demo\n");
    let md = d.join("README.md");
    let _ = std::fs::write(&md, "# Demo\n");
    let mut v = vec![NewItem::File(txt), NewItem::File(md)];
    // A real image and an app from the system, if present.
    for p in ["/System/Library/Desktop Pictures/.thumbnails/Sequoia Sunrise.heic", "/System/Library/CoreServices/Finder.app", "/Applications/Safari.app"] {
        if std::path::Path::new(p).exists() {
            v.push(NewItem::File(p.into()));
        }
    }
    if let Some(img) = std::fs::read_dir("/System/Library/Desktop Pictures/.thumbnails")
        .ok()
        .and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "heic" || e == "jpg" || e == "png")))
    {
        v.push(NewItem::File(img));
    }
    v.push(NewItem::Url { url: "https://www.rust-lang.org/learn".into(), title: Some("Learn Rust".into()) });
    v.push(NewItem::Text("Remember to ship the notch shelf before Friday".into()));
    v
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
        fn CGEventCreate(src: *const std::ffi::c_void) -> *const std::ffi::c_void;
        fn CGEventGetLocation(e: *const std::ffi::c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(p: *const std::ffi::c_void);
    }

    pub fn cursor() -> CGPoint {
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    pub fn warp(p: CGPoint) {
        unsafe {
            CGWarpMouseCursorPosition(p);
        }
    }

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

/// Fake host controls, like the ones PowerMac registers.
fn demo_options() -> StartOptions {
    let awake = Arc::new(AtomicBool::new(true));
    let dnd = Arc::new(AtomicBool::new(false));
    let (a1, a2, d1, d2) = (awake.clone(), awake, dnd.clone(), dnd);
    StartOptions {
        toggles: vec![
            QuickToggle::new("awake", "Keep Awake", "cup.and.saucer.fill", move || a1.load(Ordering::Relaxed), move |v| a2.store(v, Ordering::Relaxed)),
            QuickToggle::new("zones", "FancyZones", "rectangle.split.3x1.fill", move || d1.load(Ordering::Relaxed), move |v| d2.store(v, Ordering::Relaxed)),
        ],
        actions: vec![
            QuickAction::new("color", "Pick Color", "eyedropper", || println!("action: pick color")),
            QuickAction::new("ocr", "Extract Text", "text.viewfinder", || println!("action: extract text")),
            QuickAction::new("clip", "Clipboard History", "list.clipboard", || println!("action: clipboard")),
            QuickAction::new("lock", "Lock Screen", "lock.fill", || println!("action: lock")),
        ],
    }
}

fn widgets(kinds: &[WidgetKind]) -> Vec<WidgetConfig> {
    let mut v: Vec<WidgetConfig> = kinds.iter().map(|k| WidgetConfig { kind: *k, enabled: true }).collect();
    for k in WidgetKind::ALL {
        if !kinds.contains(&k) {
            v.push(WidgetConfig { kind: k, enabled: false });
        }
    }
    v
}

fn render_widgets(dir: PathBuf, data: PathBuf) {
    use WidgetKind as K;
    let wait = |s: f64| sleep(Duration::from_secs_f64(s));
    let base = Config { enabled: true, widgets: widgets(&[K::NowPlaying, K::Battery, K::Timer, K::QuickToggles]), ..Config::default() };
    let h = NotchShelf::start_with(base.clone(), data, demo_options()).expect("start");
    if h.items().is_empty() {
        h.add(vec![NewItem::Url { url: "https://www.rust-lang.org/learn".into(), title: Some("Learn Rust".into()) }, NewItem::Text("Ship the notch widgets".into())]);
    }
    wait(0.6);
    h.debug_sync("demo");
    h.debug_sync("note:Buy oat milk\nCall Alex about the demo\nRenew passport before March");
    wait(0.4);
    h.set_view(View::Home);
    h.expand();
    wait(1.4);
    h.render_png(dir.join("01-home-default"));
    wait(0.4);
    // Timer running + a hovered control.
    h.debug_sync("timer:1500:347");
    h.debug_sync("hoverControl:2");
    wait(0.5);
    h.render_png(dir.join("02-home-timer-running"));
    wait(0.3);
    h.debug_sync("noHover");
    // Information widgets.
    h.update(Config { widgets: widgets(&[K::NowPlaying, K::Calendar, K::Weather, K::SystemStats]), ..base.clone() });
    wait(1.2);
    h.debug_sync("demo");
    h.expand();
    wait(1.2);
    h.render_png(dir.join("03-home-info"));
    wait(0.3);
    // Tools: stopwatch, note, mirror, battery.
    h.debug_sync("stopwatch:83.4");
    h.debug_sync("pause");
    h.update(Config { widgets: widgets(&[K::Timer, K::Note, K::Calendar, K::Mirror, K::Battery]), ..base.clone() });
    wait(1.2);
    h.debug_sync("demo");
    h.debug_sync("pause");
    h.debug_sync("calendar:ask");
    h.expand();
    wait(1.2);
    h.render_png(dir.join("04-home-tools"));
    wait(0.3);
    // Everything (scrolls).
    h.update(Config { widgets: WidgetKind::ALL.iter().map(|k| WidgetConfig { kind: *k, enabled: true }).collect(), ..base.clone() });
    wait(1.2);
    h.debug_sync("demo");
    h.debug_sync("timerReset");
    h.expand();
    wait(1.2);
    h.render_png(dir.join("05-home-all"));
    wait(0.3);
    // Shelf view with the switcher.
    h.set_view(View::Shelf);
    wait(0.8);
    h.render_png(dir.join("06-shelf-view"));
    // Switch animation frames.
    h.set_view(View::Home);
    let t0 = std::time::Instant::now();
    for ms in [60u64, 140, 260] {
        if let Some(d) = Duration::from_millis(ms).checked_sub(t0.elapsed()) {
            sleep(d);
        }
        h.render_png(dir.join(format!("07-switch-{ms:03}ms")));
    }
    wait(0.5);
    // Live activities on the collapsed notch.
    h.update(base.clone());
    wait(1.2);
    h.debug_sync("demo");
    h.debug_sync("timerReset");
    h.collapse();
    wait(1.2);
    h.render_png(dir.join("08-live-music"));
    wait(0.3);
    h.debug_sync("timer:600:212");
    wait(1.0);
    h.render_png(dir.join("09-live-timer"));
    wait(0.3);
    h.debug_sync("timerDone");
    wait(0.8);
    h.render_png(dir.join("10-live-timer-done"));
    h.expand();
    wait(1.2);
    h.render_png(dir.join("11-home-timer-done"));
    h.debug_sync("timerReset");
    h.collapse();
    wait(0.6);
    h.stop();
    wait(0.2);
}

/// Real providers (stats, battery, now playing without AppleScript),
/// opened by hovering the notch with a synthetic pointer move; the pointer
/// is put back afterwards. Also exercises the note editor's key focus.
#[cfg(target_os = "macos")]
fn live_check(dir: PathBuf, data: PathBuf) {
    use WidgetKind as K;
    let wait = |s: f64| sleep(Duration::from_secs_f64(s));
    let cfg = Config { enabled: true, widgets: widgets(&[K::NowPlaying, K::SystemStats, K::Battery, K::Timer, K::Note]), default_view: notch_shelf::DefaultView::Home, ..Config::default() };
    let h = NotchShelf::start_with(cfg, data, demo_options()).expect("start");
    wait(3.0); // let the stats history fill a little
    let orig = mac::cursor();
    mac::warp(mac::CGPoint { x: 756.0, y: 6.0 });
    wait(2.5);
    h.render_png(dir.join("live-home"));
    wait(0.5);
    h.debug_sync("editNote");
    wait(0.6);
    h.render_png(dir.join("live-note-editing"));
    wait(0.3);
    h.debug_sync("endEdit");
    wait(0.3);
    mac::warp(orig);
    wait(1.2);
    h.render_png(dir.join("live-after"));
    println!("status: {:?}", h.widget_status());
    h.stop();
    wait(0.3);
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = args();
    #[cfg(target_os = "macos")]
    if let Some(dir) = a.live.clone() {
        mac::run_app(move || live_check(dir, a.data));
        return;
    }
    if let Some(dir) = a.widgets.clone() {
        #[cfg(target_os = "macos")]
        mac::run_app(move || render_widgets(dir, a.data));
        #[cfg(not(target_os = "macos"))]
        render_widgets(dir, a.data);
        return;
    }
    let cfg = Config {
        enabled: true,
        virtual_notch_on_non_notch_screens: a.virt,
        screens: if a.virt { ScreenMode::All } else { ScreenMode::NotchOnly },
        ..Config::default()
    };
    println!("has_notch: {}  data: {}", notch_shelf::has_notch(), a.data.display());
    let handle = NotchShelf::start(cfg, a.data.clone()).expect("start");
    if a.sample && handle.items().is_empty() {
        let ids = handle.add(samples(&a.data));
        println!("added {} sample items", ids.len());
    }
    let work = move || {
        let wait = |s: f64| sleep(Duration::from_secs_f64(s));
        wait(0.8);
        if let Some(dir) = &a.render {
            handle.expand();
            wait(1.5);
            handle.render_png(dir.join("expanded"));
            wait(0.4);
            handle.debug("dropHover");
            wait(0.6);
            handle.render_png(dir.join("drop-target"));
            wait(0.4);
            handle.debug("noDropHover");
            handle.collapse();
            wait(1.0);
            handle.render_png(dir.join("collapsed"));
            wait(0.3);
            handle.debug("peek");
            wait(1.0);
            handle.render_png(dir.join("peek"));
            wait(3.0);
            // Frames of the opening spring.
            handle.expand();
            let t0 = std::time::Instant::now();
            for ms in [40u64, 80, 120, 160, 220, 300] {
                let at = Duration::from_millis(ms);
                if let Some(d) = at.checked_sub(t0.elapsed()) {
                    sleep(d);
                }
                handle.render_png(dir.join(format!("opening-{ms:03}ms")));
            }
            wait(1.0);
            handle.collapse();
            wait(0.8);
        }
        #[cfg(target_os = "macos")]
        if a.hover {
            let orig = mac::cursor();
            mac::warp(mac::CGPoint { x: 756.0, y: 8.0 });
            wait(0.9);
            if let Some(dir) = &a.render {
                handle.render_png(dir.join("hover"));
            }
            // Pointer over the first tile (hover highlight + remove badge),
            // with the first two tiles selected.
            handle.debug("select:2");
            mac::warp(mac::CGPoint { x: 502.0, y: 86.0 });
            wait(0.6);
            if let Some(dir) = &a.render {
                handle.render_png(dir.join("hover-tile"));
            }
            wait(0.3);
            handle.debug("select:0");
            mac::warp(orig);
            wait(1.0);
            if let Some(dir) = &a.render {
                handle.render_png(dir.join("after-hover"));
            }
            wait(0.3);
        }
        let left = a.secs - 0.8;
        if a.render.is_none() && !a.hover && left > 0.0 {
            wait(left);
        }
        println!("items: {}", handle.items().len());
        handle.stop();
        wait(0.2);
    };
    #[cfg(target_os = "macos")]
    mac::run_app(work);
    #[cfg(not(target_os = "macos"))]
    work();
}
