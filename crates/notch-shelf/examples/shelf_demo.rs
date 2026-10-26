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

use notch_shelf::{AudioOutput, Config, FakeSound, NewItem, NotchShelf, QuickAction, QuickToggle, ScreenMode, StartOptions, View, WidgetInstance, WidgetKind, WidgetPage, WidgetSize};

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

/// Fake host controls, like the ones PowerMac registers, and a fake sound
/// provider.
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
        sound: Some(FakeSound::new(vec![AudioOutput::new("builtin", "MacBook Pro Speakers"), AudioOutput::new("airpods", "AirPods Pro")], "builtin", 0.5)),
    }
}

/// Instances of `kinds` on page 0 (sizes like the old fixed cards).
fn widgets(kinds: &[WidgetKind]) -> Vec<WidgetInstance> {
    kinds
        .iter()
        .map(|k| {
            let size = match k {
                WidgetKind::Weather | WidgetKind::Battery | WidgetKind::Mirror => WidgetSize::Small,
                _ => WidgetSize::Medium,
            };
            WidgetInstance::new(*k).sized(size)
        })
        .collect()
}

/// An instance with a fixed id (debug commands refer to it), size, page
/// and settings.
fn inst(id: &str, kind: WidgetKind, size: WidgetSize, page: usize, settings: serde_json::Value) -> WidgetInstance {
    let mut w = WidgetInstance::new(kind).sized(size).on_page(page);
    w.id = id.into();
    let mut s = notch_shelf::default_settings(kind);
    if let (Some(o), serde_json::Value::Object(extra)) = (s.as_object_mut(), settings) {
        o.extend(extra);
    }
    w.settings = s;
    w
}

/// Sample scripts and xbar plugins for the renders (written into the data
/// dir; they only print canned output or read `df` / `sysctl`).
fn write_samples(data: &std::path::Path) -> (PathBuf, PathBuf, PathBuf) {
    let scripts = data.join("samples/scripts");
    let plugins = data.join("samples/plugins");
    let single = data.join("samples/plugin-one");
    for d in [&scripts, &plugins, &single] {
        let _ = std::fs::create_dir_all(d);
    }
    let files: &[(&PathBuf, &str, &str)] = &[
        (&scripts, "disk.sh", "#!/bin/sh\ndf -h / | awk 'NR==2 {print $5; print $4 \" free of \" $2}'\n"),
        (&scripts, "deploys.sh", "#!/bin/sh\ncat <<'JSON'\n{\"title\":\"Deploys\",\"value\":\"98.2%\",\"subtitle\":\"success · last 7 days\",\"color\":\"#30d158\",\"sf_symbol\":\"shippingbox.fill\"}\nJSON\n"),
        (&scripts, "prs.sh", "#!/bin/sh\ncat <<'JSON'\n{\"title\":\"Pull Requests\",\"sf_symbol\":\"arrow.triangle.pull\",\"color\":\"purple\",\"items\":[\n{\"title\":\"Widget platform: instances & pages\",\"subtitle\":\"#412\",\"sf_symbol\":\"checkmark.circle.fill\",\"color\":\"green\",\"action\":\"https://github.com\"},\n{\"title\":\"Fix crash in notch layout\",\"subtitle\":\"#409\",\"sf_symbol\":\"xmark.circle.fill\",\"color\":\"red\",\"action\":\"https://github.com\"},\n{\"title\":\"xbar parser: submenus\",\"subtitle\":\"#407\",\"sf_symbol\":\"clock.fill\",\"color\":\"yellow\"},\n{\"title\":\"Sound provider trait\",\"subtitle\":\"#405\",\"sf_symbol\":\"checkmark.circle.fill\",\"color\":\"green\"},\n{\"title\":\"World clocks\",\"subtitle\":\"#401\",\"sf_symbol\":\"checkmark.circle.fill\",\"color\":\"green\"},\n{\"title\":\"Reminders via EventKit\",\"subtitle\":\"#399\",\"sf_symbol\":\"clock.fill\",\"color\":\"yellow\"}]}\nJSON\n"),
        (&scripts, "uptime.sh", "#!/bin/sh\necho \"Up $(uptime | sed -E 's/.*up ([^,]*),.*/\\1/')\"\necho \"$(sysctl -n machdep.cpu.brand_string) · $(sw_vers -productName) $(sw_vers -productVersion)\"\n"),
        (&scripts, "fail.sh", "#!/bin/sh\necho 'curl: (6) Could not resolve host: status.internal' >&2\nexit 6\n"),
        (&plugins, "weather.5m.sh", "#!/bin/bash\necho '☀️ 21°C | color=orange'\necho 'Rain at 17:00'\necho '---'\necho 'Cupertino, CA'\necho 'Feels like 23° · Humidity 41% | color=gray'\necho 'Open forecast | href=https://open-meteo.com sfimage=safari'\n"),
        (&plugins, "github.10m.sh", "#!/bin/bash\necho '🔔 3'\necho '---'\necho 'Notifications | href=https://github.com/notifications sfimage=bell.fill'\necho 'rust-lang/rust'\necho '--Fix ICE in borrowck | href=https://github.com/rust-lang/rust/pull/1'\n"),
        (&plugins, "cpu.10s.sh", "#!/bin/bash\necho \"Load $(sysctl -n vm.loadavg | awk '{print $2}') | font=Menlo\"\necho '---'\necho \"Activity Monitor | bash=/usr/bin/open param1=-a param2='Activity Monitor' terminal=false\"\n"),
        (&single, "tasks.1m.sh", "#!/bin/bash\necho '✔︎ 4 tasks'\necho '---'\necho 'Today | color=#8e8e93'\necho 'Write the widget docs | checked=true'\necho 'Review the xbar parser | href=https://example.com/1'\necho 'Render more PNGs | sfimage=photo.on.rectangle'\necho '---'\necho 'Ship it | sfimage=paperplane.fill color=#0a84ff href=https://example.com/ship'\necho 'Later | color=gray'\necho '--Refactor scheduler'\necho '--Windows port'\n"),
    ];
    for (dir, name, body) in files {
        let p = dir.join(name);
        let _ = std::fs::write(&p, body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }
    (scripts, plugins, single)
}

fn render_widgets(dir: PathBuf, data: PathBuf) {
    use serde_json::json;
    use WidgetKind as K;
    use WidgetSize::{Large as L, Medium as M, Small as S};
    let wait = |s: f64| sleep(Duration::from_secs_f64(s));
    let (scripts, plugins, single) = write_samples(&data);
    let sp = |p: &PathBuf, f: &str| p.join(f).to_string_lossy().into_owned();
    let base = Config { enabled: true, widgets: widgets(&[K::NowPlaying, K::Battery, K::Timer, K::QuickToggles]), ..Config::default() };
    let h = NotchShelf::start_with(base.clone(), data.clone(), demo_options()).expect("start");
    if h.items().is_empty() {
        h.add(vec![NewItem::Url { url: "https://www.rust-lang.org/learn".into(), title: Some("Learn Rust".into()) }, NewItem::Text("Ship the notch widgets".into())]);
    }
    let show = |cfg: &Config, settle: f64| {
        h.update(cfg.clone());
        wait(1.0);
        h.debug_sync("demo");
        h.set_view(View::Home);
        h.expand();
        wait(settle);
    };
    let shot = |name: &str| {
        h.render_png(dir.join(name));
        wait(0.35);
    };
    wait(0.6);
    // ----- The classic single page ---------------------------------------
    show(&base, 1.4);
    h.debug_sync("note:Buy oat milk\nCall Alex about the demo\nRenew passport before March");
    shot("01-home-default");
    h.debug_sync("timer:1500:347");
    h.debug_sync("hoverControl:2");
    wait(0.4);
    shot("02-home-timer-running");
    h.debug_sync("noHover");
    h.debug_sync("timerReset");
    show(&Config { widgets: widgets(&[K::NowPlaying, K::Calendar, K::Weather, K::SystemStats]), ..base.clone() }, 1.2);
    shot("03-home-info");
    // ----- Pages: Work / Media / Dev -------------------------------------
    let pages = vec![WidgetPage::named("Work"), WidgetPage::named("Media"), WidgetPage::named("Dev")];
    let three = Config {
        pages: pages.clone(),
        widgets: vec![
            inst("cal", K::Calendar, M, 0, json!({})),
            inst("rem", K::Reminders, M, 0, json!({})),
            inst("clk", K::WorldClock, M, 0, json!({})),
            inst("np", K::NowPlaying, M, 1, json!({})),
            inst("snd", K::Sound, M, 1, json!({})),
            inst("bt", K::Bluetooth, M, 1, json!({})),
            inst("disk", K::Script, S, 2, json!({"command": sp(&scripts, "disk.sh"), "title": "Disk", "symbol": "internaldrive.fill", "interval": 300})),
            inst("prs", K::Script, M, 2, json!({"command": sp(&scripts, "prs.sh"), "symbol": "arrow.triangle.pull", "interval": 600})),
            inst("xbar", K::Xbar, M, 2, json!({"path": single.to_string_lossy()})),
        ],
        ..base.clone()
    };
    show(&three, 1.6);
    h.set_page(0);
    wait(1.2);
    shot("10-pages-work");
    h.set_page(1);
    let t0 = std::time::Instant::now();
    for ms in [70u64, 150, 260] {
        if let Some(d) = Duration::from_millis(ms).checked_sub(t0.elapsed()) {
            sleep(d);
        }
        h.render_png(dir.join(format!("11-pages-slide-{ms:03}ms")));
    }
    wait(1.0);
    shot("12-pages-media");
    h.set_page(2);
    wait(2.5);
    shot("13-pages-dev");
    h.debug_sync("hover:item:1");
    wait(0.3);
    shot("14-dev-hover-item");
    h.debug_sync("noHover");
    h.debug_sync("scroll:60");
    wait(0.3);
    shot("15-dev-list-scrolled");
    h.debug_sync("scroll:0");
    // ----- Build your own, mixed sizes ------------------------------------
    let web_body = r#"{"bitcoin":{"usd":67212.43,"usd_24h_change":2.1834},"updated":1792990000}"#;
    let byo = Config {
        pages: vec![WidgetPage::named("Custom")],
        widgets: vec![
            inst("deploy", K::Script, S, 0, json!({"command": sp(&scripts, "deploys.sh"), "interval": 600})),
            inst("web", K::Web, M, 0, json!({"url": "https://example.invalid/price", "title": "Bitcoin", "symbol": "bitcoinsign.circle.fill",
                "template": "${{bitcoin.usd | round:0}}", "subtitle": "{{bitcoin.usd_24h_change | round:2}}% today · {{updated | time}}",
                "valuePath": "bitcoin.usd_24h_change", "min": -5, "max": 5})),
            inst("plug", K::Xbar, M, 0, json!({"path": sp(&plugins, "weather.5m.sh")})),
            inst("up", K::Script, L, 0, json!({"command": sp(&scripts, "uptime.sh"), "format": "text", "symbol": "desktopcomputer"})),
        ],
        ..base.clone()
    };
    show(&byo, 2.5);
    h.debug_sync(&format!("webBody:web|{web_body}"));
    wait(0.5);
    shot("20-custom-mixed-sizes");
    let byo2 = Config {
        pages: vec![WidgetPage::named("Tools")],
        widgets: vec![
            inst("sc", K::Shortcuts, M, 0, json!({"shortcuts": ["Log Water", "Focus 25", "Toggle VPN", "Morning Routine", "Send ETA"]})),
            inst("ln", K::Launcher, L, 0, json!({"items": [
                {"kind": "app", "target": "Safari"}, {"kind": "app", "target": "com.apple.Notes"}, {"kind": "app", "target": "Calendar"},
                {"kind": "folder", "target": "~/Downloads"}, {"kind": "url", "target": "https://github.com/notifications", "label": "GitHub"},
                {"kind": "command", "target": "open -a Terminal ~", "label": "Shell", "symbol": "terminal.fill"},
                {"kind": "app", "target": "System Settings"}, {"kind": "app", "target": "Music"}]})),
            inst("bat", K::Battery, S, 0, json!({})),
        ],
        ..base.clone()
    };
    show(&byo2, 1.6);
    shot("21-shortcuts-launcher");
    h.debug_sync("hover:launch:3");
    wait(0.3);
    shot("22-launcher-hover");
    h.debug_sync("noHover");
    // ----- Clocks ------------------------------------------------------------
    let zones = json!([{"zone": "America/Los_Angeles", "label": "Cupertino"}, {"zone": "America/New_York", "label": "New York"}, {"zone": "Europe/London", "label": "London"},
        {"zone": "Europe/Berlin", "label": "Berlin"}, {"zone": "Asia/Kolkata", "label": "Bengaluru"}, {"zone": "Asia/Tokyo", "label": "Tokyo"}]);
    let clocks = Config {
        pages: vec![WidgetPage::named("Clocks")],
        widgets: vec![
            inst("ca", K::WorldClock, L, 0, json!({"clocks": zones, "style": "analog", "seconds": true})),
            inst("cd", K::WorldClock, S, 0, json!({"clocks": [{"zone": "Asia/Tokyo", "label": "Tokyo"}, {"zone": "Europe/London", "label": "London"}]})),
            inst("c1", K::WorldClock, S, 0, json!({"clocks": [{"zone": "Australia/Sydney", "label": "Sydney"}], "hours": "12"})),
        ],
        ..base.clone()
    };
    show(&clocks, 1.4);
    shot("30-world-clocks");
    // ----- System -----------------------------------------------------------
    let sys = Config {
        pages: vec![WidgetPage::named("System")],
        widgets: vec![inst("btl", K::Bluetooth, L, 0, json!({})), inst("snd2", K::Sound, L, 0, json!({}))],
        ..base.clone()
    };
    show(&sys, 1.4);
    shot("31-bluetooth-sound-large");
    let sys2 = Config {
        pages: vec![WidgetPage::named("Small")],
        widgets: vec![
            inst("bts", K::Bluetooth, S, 0, json!({"devices": ["AirPods"]})),
            inst("rems", K::Reminders, L, 0, json!({})),
            inst("w", K::Weather, S, 0, json!({})),
            inst("ctl", K::QuickToggles, S, 0, json!({"controls": ["awake", "color", "ocr", "lock"]})),
        ],
        ..base.clone()
    };
    show(&sys2, 1.4);
    h.debug_sync("tick:1");
    wait(0.15);
    shot("32-small-sizes-reminder-tick");
    // ----- Errors -------------------------------------------------------------
    let errs = Config {
        pages: vec![WidgetPage::named("Errors"), WidgetPage::named("More")],
        widgets: vec![
            inst("bad", K::Script, M, 0, json!({"command": sp(&scripts, "fail.sh"), "title": "Status page"})),
            inst("stale", K::Web, M, 0, json!({"url": "https://example.invalid/price", "title": "Bitcoin", "symbol": "bitcoinsign.circle.fill", "template": "${{bitcoin.usd | round:0}}", "subtitle": "{{bitcoin.usd_24h_change | round:2}}% today"})),
            inst("rem2", K::Reminders, M, 0, json!({})),
            inst("missing", K::Xbar, S, 1, json!({"path": "~/no/such/plugin.1m.sh"})),
        ],
        ..base.clone()
    };
    show(&errs, 2.0);
    h.debug_sync(&format!("webBody:stale|{web_body}"));
    h.debug_sync("error:stale|HTTP 503");
    h.debug_sync("reminders:denied");
    wait(0.5);
    shot("40-errors");
    h.set_page(1);
    wait(1.2);
    shot("40b-errors-missing-plugin");
    // ----- Many pages: dots ------------------------------------------------------
    let many = Config {
        pages: ["Home", "Work", "Media", "Development", "Monitoring", "Personal"].iter().map(|n| WidgetPage::named(n)).collect(),
        widgets: (0..6).map(|i| inst(&format!("n{i}"), [K::Timer, K::Calendar, K::NowPlaying, K::Script, K::SystemStats, K::Note][i], M, i, json!({"command": sp(&scripts, "disk.sh")}))).collect(),
        ..base.clone()
    };
    show(&many, 1.2);
    h.set_page(3);
    wait(1.5);
    shot("41-many-pages-dots");
    // ----- Live activities ---------------------------------------------------------
    let live = Config {
        pages: vec![WidgetPage::named("Live")],
        widgets: vec![inst("lv", K::Script, M, 0, json!({"command": sp(&scripts, "disk.sh"), "title": "Disk", "symbol": "internaldrive.fill", "live": true}))],
        ..base.clone()
    };
    show(&live, 1.5);
    h.collapse();
    wait(1.4);
    shot("50-live-custom");
    show(&base, 1.0);
    h.debug_sync("timerReset");
    h.collapse();
    wait(1.2);
    shot("51-live-music");
    h.debug_sync("timer:600:212");
    wait(1.0);
    shot("52-live-timer");
    // Shelf view with the switcher.
    h.expand();
    h.set_view(View::Shelf);
    wait(1.0);
    shot("60-shelf-view");
    h.set_view(View::Home);
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
