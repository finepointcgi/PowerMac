//! Run the menu bar manager with NSApplication on the main thread.
//!
//! ```text
//! cargo run -p menubar-manager --example menubar_demo -- [SECS] [--self-test] [--bar] [--hover] [--list]
//! ```
//! * default: adds two test status items ("PM-A", "PM-B") plus the manager's
//!   chevron and separators for SECS seconds (Cmd-drag icons across the
//!   separators, click the chevron), then restores the menu bar.
//! * `--list`      print every menu bar item with its section and exit
//! * `--self-test` scripted check that only touches the demo's own test
//!   items: moves them between sections with synthesized Cmd-drags, hides /
//!   shows, presses one through the floating bar, verifies that other apps'
//!   items didn't move, then restores everything.
//! * `--bar`       use the floating bar instead of expanding in place
//! * `--hover`     show on hover

use std::thread::sleep;
use std::time::Duration;

use menubar_manager::{Config, MenuBarItem, MenuBarManager, MoveTarget, Section};

struct Args {
    secs: f64,
    self_test: bool,
    bar: bool,
    hover: bool,
    list: bool,
    hide_a: bool,
}

fn args() -> Args {
    let mut a = Args { secs: 30.0, self_test: false, bar: false, hover: false, list: false, hide_a: false };
    for s in std::env::args().skip(1) {
        match s.as_str() {
            "--self-test" => a.self_test = true,
            "--bar" => a.bar = true,
            "--hover" => a.hover = true,
            "--list" => a.list = true,
            "--hide-a" => a.hide_a = true,
            n => a.secs = n.parse().unwrap_or(a.secs),
        }
    }
    a
}

fn print_items(tag: &str, items: &[MenuBarItem]) {
    println!("--- {tag}: {} items", items.len());
    for i in items {
        println!(
            "  {:>13?} x={:>8.1} w={:>5.1} {:<28} {}{}",
            i.section,
            i.frame.x,
            i.frame.w,
            i.id,
            i.title.as_deref().unwrap_or(""),
            if i.own { " (own)" } else { "" }
        );
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{class, define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSString;

    pub static CLICKS: AtomicUsize = AtomicUsize::new(0);

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "MenuBarDemoTarget"]
        pub struct DemoTarget;

        impl DemoTarget {
            #[unsafe(method(clicked:))]
            fn clicked(&self, _s: Option<&AnyObject>) {
                CLICKS.fetch_add(1, Ordering::SeqCst);
                println!("  [test item clicked]");
            }
        }
    );

    thread_local! {
        static KEEP: std::cell::RefCell<Vec<Retained<AnyObject>>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Our own test status items (created before the manager, so they start
    /// in the visible section).
    pub fn add_test_items(names: &[&str]) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = DemoTarget::alloc(mtm).set_ivars(());
        let target: Retained<DemoTarget> = unsafe { msg_send![super(this), init] };
        unsafe {
            let bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
            for n in names {
                let item: Retained<AnyObject> = msg_send![bar, statusItemWithLength: -1.0f64];
                let b: *mut AnyObject = msg_send![&*item, button];
                let _: () = msg_send![b, setTitle: &*NSString::from_str(n)];
                let _: () = msg_send![b, setTarget: &*target];
                let _: () = msg_send![b, setAction: sel!(clicked:)];
                KEEP.with(|k| k.borrow_mut().push(item));
            }
        }
        KEEP.with(|k| k.borrow_mut().push(Retained::into_super(target).into()));
    }

    /// Forget the positions macOS saved for the demo's own status items
    /// (items without an autosave name get "Item-N" and are remembered too).
    /// Only touches this demo's own defaults domain.
    pub fn forget_positions() {
        unsafe {
            let d: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
            let dict: Retained<AnyObject> = msg_send![d, dictionaryRepresentation];
            let keys: Retained<AnyObject> = msg_send![&*dict, allKeys];
            let n: usize = msg_send![&*keys, count];
            for i in 0..n {
                let k: *mut NSString = msg_send![&*keys, objectAtIndex: i];
                if (*k).to_string().starts_with("NSStatusItem ") {
                    let _: () = msg_send![d, removeObjectForKey: k];
                }
            }
        }
    }

    pub fn remove_test_items() {
        KEEP.with(|k| {
            for it in k.borrow_mut().drain(..) {
                unsafe {
                    let is_item: bool = msg_send![&*it, isKindOfClass: class!(NSStatusItem)];
                    if is_item {
                        let bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
                        let _: () = msg_send![bar, removeStatusItem: &*it];
                    }
                }
            }
        });
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
        fn CGEventCreate(src: *const std::ffi::c_void) -> *const std::ffi::c_void;
        fn CGEventGetLocation(e: *const std::ffi::c_void) -> CGPoint;
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

    /// Move the pointer (no click).
    pub fn warp(x: f64, y: f64) {
        unsafe {
            CGWarpMouseCursorPosition(CGPoint { x, y });
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

    pub fn on_main(f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        dispatch(move || {
            f();
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
    }

    fn dispatch(f: impl FnOnce() + Send + 'static) {
        extern "C" {
            static _dispatch_main_q: std::ffi::c_void;
            fn dispatch_async_f(q: *const std::ffi::c_void, ctx: *mut std::ffi::c_void, work: extern "C" fn(*mut std::ffi::c_void));
        }
        extern "C" fn tramp(ctx: *mut std::ffi::c_void) {
            let f: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(ctx as *mut Box<dyn FnOnce()>) };
            f();
        }
        let b: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
        unsafe { dispatch_async_f(&_dispatch_main_q, Box::into_raw(b) as *mut _, tramp) };
    }
}

/// Other apps' items left to right (ids and x).
fn others(items: &[MenuBarItem]) -> Vec<(String, f64)> {
    // macOS 26+'s own overflow chevron comes and goes with what fits.
    let mut v: Vec<(String, f64)> = items.iter().filter(|i| !i.own && !i.id.contains("overflow")).map(|i| (i.id.clone(), i.frame.x)).collect();
    v.sort_by(|a, b| a.1.total_cmp(&b.1));
    v
}

/// Same items in the same order at about the same places (the clock's
/// width changes shift everything a little).
fn same_layout(a: &[(String, f64)], b: &[(String, f64)]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && (x.1 - y.1).abs() <= 12.0)
}

/// Titles of our own items the menu bar really shows (hit test), and the
/// number of other apps' items it shows.
fn really_shown() -> (Vec<String>, usize) {
    let me = std::process::id() as i32;
    let v = menubar_manager::visible_status_items();
    let own = v.iter().filter(|i| i.pid == me).filter_map(|i| i.title.clone()).collect();
    (own, v.iter().filter(|i| i.pid != me).count())
}

fn find<'a>(items: &'a [MenuBarItem], title: &str) -> Option<&'a MenuBarItem> {
    items.iter().find(|i| i.own && i.title.as_deref() == Some(title))
}

#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn self_test(h: &menubar_manager::Handle, cfg: &Config, before_others: Vec<(String, f64)>) -> bool {
    let wait = |s: f64| sleep(Duration::from_secs_f64(s));
    let mut ok = true;
    let mut check = |name: &str, cond: bool| {
        println!("[{}] {name}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };
    println!("fitted mode (macOS 26+): {}  permissions: {:?}", h.fitted_mode(), menubar_manager::permissions());
    wait(2.0);
    let items = h.items();
    print_items("started (collapsed)", &items);
    let a = find(&items, "PM-A").cloned();
    let b = find(&items, "PM-B").cloned();
    check("test items listed", a.is_some() && b.is_some());
    let (Some(a), Some(b)) = (a, b) else { return false };
    check("test items start visible", a.section == Section::Visible && b.section == Section::Visible);
    check("other apps' items unchanged after start", same_layout(&others(&items), &before_others));
    let (own0, others0) = really_shown();
    println!("really shown (own): {own0:?}, other apps' items shown: {others0}");
    check("menu bar shows PM-A and PM-B", own0.contains(&"PM-A".to_string()) && own0.contains(&"PM-B".to_string()));

    // Move A into the hidden section.
    let r = h.move_item(&a.id, MoveTarget::Section(Section::Hidden));
    println!("move A → hidden: {r:?}");
    wait(1.5);
    let items = h.items();
    print_items("after moving A to hidden (collapsed again)", &items);
    let a2 = find(&items, "PM-A").cloned();
    check("A is in the hidden section", a2.as_ref().is_some_and(|i| i.section == Section::Hidden));
    check("other apps' items unchanged after move", same_layout(&others(&items), &before_others));
    let (own1, others1) = really_shown();
    println!("really shown (own): {own1:?}, other apps' items shown: {others1}");
    check("collapsed: PM-A really hidden, PM-B shown", !own1.contains(&"PM-A".to_string()) && own1.contains(&"PM-B".to_string()));
    check("collapsed: other apps' items all still shown", others1 == others0);
    let nh = h.items_hidden_by_notch();
    println!("hidden by notch / overflow: {:?}", nh.iter().map(|i| &i.id).collect::<Vec<_>>());
    check("nothing that should be visible is hidden", nh.iter().all(|i| i.id.contains("overflow")) );

    // Show on hover, then auto-rehide (moves the pointer, no clicks).
    #[cfg(target_os = "macos")]
    {
        let orig = mac::cursor();
        let chevron_x = menubar_manager::visible_status_items().iter().find(|i| i.pid == std::process::id() as i32 && i.title.as_deref().is_some_and(|t| t.starts_with("Show hidden"))).map(|i| i.x);
        if let Some(cx) = chevron_x {
            h.update(Config { show_on_hover: true, hover_delay_ms: 200, ..cfg.clone() });
            wait(0.3);
            mac::warp(cx - 60.0, 12.0);
            wait(1.0);
            let shown = h.expansion();
            mac::warp(700.0, 500.0);
            wait(1.5);
            let after = h.expansion();
            println!("hover: shown {shown:?}, after leaving {after:?}");
            check("hover over the empty menu bar shows, leaving hides", shown == menubar_manager::Expansion::Hidden && after == menubar_manager::Expansion::Collapsed);
        } else {
            check("chevron found for the hover test", false);
        }
        h.update(Config { auto_rehide: true, rehide_delay_secs: 2, ..cfg.clone() });
        wait(0.3);
        h.show_hidden();
        wait(0.8);
        let shown = h.expansion();
        wait(2.5);
        let after = h.expansion();
        println!("auto-rehide: {shown:?} then {after:?}");
        check("auto-rehide after 2 s", shown == menubar_manager::Expansion::Hidden && after == menubar_manager::Expansion::Collapsed);
        mac::warp(orig.x, orig.y);
        h.update(cfg.clone());
        wait(1.0);
    }

    h.show_hidden();
    wait(1.5);
    let items = h.items();
    print_items("hidden section shown", &items);
    let a3 = find(&items, "PM-A").cloned();
    check("A on screen while shown", a3.as_ref().is_some_and(|i| i.section == Section::Hidden && i.frame.x > 0.0));
    let (own2, _) = really_shown();
    println!("really shown (own): {own2:?}");
    check("shown: PM-A really shown", own2.contains(&"PM-A".to_string()));
    h.hide();
    wait(1.5);

    // B into always-hidden.
    let r = h.move_item(&b.id, MoveTarget::Section(Section::AlwaysHidden));
    println!("move B → always hidden: {r:?}");
    wait(1.0);
    h.show_hidden();
    wait(1.5);
    let items = h.items();
    print_items("hidden shown, always-hidden not", &items);
    check("B in always-hidden", find(&items, "PM-B").is_some_and(|i| i.section == Section::AlwaysHidden));
    let (own3, _) = really_shown();
    println!("really shown (own): {own3:?}");
    check("hidden shown: PM-A shown, PM-B (always hidden) not", own3.contains(&"PM-A".to_string()) && !own3.contains(&"PM-B".to_string()));
    h.show_all();
    wait(1.5);
    let items = h.items();
    print_items("everything shown", &items);
    check("A hidden, B always-hidden (all shown)", find(&items, "PM-A").is_some_and(|i| i.section == Section::Hidden) && find(&items, "PM-B").is_some_and(|i| i.section == Section::AlwaysHidden));
    let (own4, _) = really_shown();
    println!("really shown (own): {own4:?}");
    check("all shown: PM-A and PM-B shown", own4.contains(&"PM-A".to_string()) && own4.contains(&"PM-B".to_string()));
    println!("notch-hidden now: {:?}", h.items_hidden_by_notch().iter().map(|i| &i.id).collect::<Vec<_>>());
    h.hide();
    wait(1.5);

    // Press A from the collapsed state (what a floating-bar click does).
    #[cfg(target_os = "macos")]
    {
        let before = mac::CLICKS.load(std::sync::atomic::Ordering::SeqCst);
        h.show_bar(false);
        wait(1.0);
        h.press_item(&a.id);
        wait(2.0);
        let after = mac::CLICKS.load(std::sync::atomic::Ordering::SeqCst);
        check("pressing a hidden item reaches it", after > before);
        h.hide();
        wait(1.0);
    }

    // Back to visible.
    let r1 = h.move_item(&a.id, MoveTarget::Section(Section::Visible));
    let r2 = h.move_item(&b.id, MoveTarget::Section(Section::Visible));
    println!("moves back: {r1:?} {r2:?}");
    wait(1.5);
    let items = h.items();
    print_items("restored arrangement", &items);
    check("test items visible again", find(&items, "PM-A").is_some_and(|i| i.section == Section::Visible) && find(&items, "PM-B").is_some_and(|i| i.section == Section::Visible));
    check("other apps' items unchanged at the end", same_layout(&others(&items), &before_others));
    let (own5, others5) = really_shown();
    check("end: PM-A, PM-B and other apps' items shown", own5.contains(&"PM-A".to_string()) && own5.contains(&"PM-B".to_string()) && others5 == others0);
    ok
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = args();
    if a.list {
        let work = || {
            let h = MenuBarManager::start(Config::default()).expect("start");
            let t0 = std::time::Instant::now();
            let items = h.items();
            println!("listing took {:?}", t0.elapsed());
            let t0 = std::time::Instant::now();
            let _ = h.items();
            println!("second listing took {:?}", t0.elapsed());
            print_items("menu bar (manager off)", &items);
            println!("hidden by notch: {:?}", menubar_manager::items_hidden_by_notch().iter().map(|i| &i.id).collect::<Vec<_>>());
            h.stop();
        };
        #[cfg(target_os = "macos")]
        mac::run_app(work);
        #[cfg(not(target_os = "macos"))]
        work();
        return;
    }
    #[cfg(target_os = "macos")]
    {
        menubar_manager::reset_positions();
        mac::forget_positions();
        mac::add_test_items(&["PM-A", "PM-B"]);
    }
    let cfg = Config {
        enabled: true,
        auto_rehide: !a.self_test,
        rehide_on_focus_change: !a.self_test,
        rehide_delay_secs: 8,
        show_on_hover: a.hover,
        use_floating_bar: a.bar,
        ..Config::default()
    };
    let work = move || {
        // Other apps' items before we add anything (manager disabled).
        let probe = MenuBarManager::start(Config::default()).expect("start");
        sleep(Duration::from_millis(300));
        let before = others(&probe.items());
        probe.stop();
        let cfg2 = cfg.clone();
        let h = MenuBarManager::start(cfg).expect("start");
        let mut ok = true;
        if a.self_test {
            ok = self_test(&h, &cfg2, before.clone());
        } else {
            sleep(Duration::from_secs(2));
            if a.hide_a {
                if let Some(it) = find(&h.items(), "PM-A").cloned() {
                    println!("move A → hidden: {:?}", h.move_item(&it.id, MoveTarget::Section(Section::Hidden)));
                }
                sleep(Duration::from_secs(1));
                if a.bar {
                    println!("opening the floating bar");
                    h.toggle();
                }
            }
            print_items("running", &h.items());
            println!("Cmd-drag icons across the separators; click the chevron (Option-click: always-hidden too).");
            sleep(Duration::from_secs_f64((a.secs - 2.0).max(0.0)));
        }
        if a.hide_a {
            if let Some(it) = find(&h.items(), "PM-A").cloned() {
                println!("move A → visible: {:?}", h.move_item(&it.id, MoveTarget::Section(Section::Visible)));
            }
        }
        h.stop();
        sleep(Duration::from_millis(500));
        #[cfg(target_os = "macos")]
        mac::on_main(|| {
            mac::remove_test_items();
            menubar_manager::reset_positions();
            mac::forget_positions();
        });
        sleep(Duration::from_millis(500));
        let probe = MenuBarManager::start(Config::default()).expect("start");
        sleep(Duration::from_millis(300));
        let after = probe.items();
        probe.stop();
        let restored = same_layout(&others(&after), &before);
        println!("[{}] menu bar restored (other apps' items where they were, no own items left: {})", if restored { "PASS" } else { "FAIL" }, !after.iter().any(|i| i.own));
        if !restored {
            println!("before: {before:?}\nafter:  {:?}", others(&after));
        }
        if a.self_test {
            println!("SELF-TEST {}", if ok && restored { "PASSED" } else { "FAILED" });
        }
    };
    #[cfg(target_os = "macos")]
    mac::run_app(work);
    #[cfg(not(target_os = "macos"))]
    work();
}
