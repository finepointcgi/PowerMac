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

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use notch_shelf::{Config, NewItem, NotchShelf, ScreenMode};

struct Args {
    secs: f64,
    data: PathBuf,
    sample: bool,
    render: Option<PathBuf>,
    hover: bool,
    virt: bool,
}

fn args() -> Args {
    let mut a = Args { secs: 20.0, data: std::env::temp_dir().join("notch-shelf-demo"), sample: false, render: None, hover: false, virt: false };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--data" => a.data = it.next().map(PathBuf::from).unwrap_or(a.data),
            "--sample" => a.sample = true,
            "--render" => a.render = it.next().map(PathBuf::from),
            "--hover" => a.hover = true,
            "--virtual" => a.virt = true,
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

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = args();
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
