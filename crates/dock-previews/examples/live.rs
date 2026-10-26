//! Live smoke test on macOS: runs Dock previews with an NSApplication main
//! loop and drives it with synthetic mouse events over the Dock icon of an
//! app (TextEdit by default) whose test windows the script opened itself.
//!
//! ```sh
//! cargo run -p dock-previews --example live -- --out DIR            # scripted
//! cargo run -p dock-previews --example live -- --manual 60          # try it by hand for 60 s
//! ```
//!
//! The script opens `dock-a.txt`, `dock-b.txt`, `dock-c.txt` (in DIR) in
//! TextEdit, hovers the TextEdit Dock icon, renders the live panel to PNGs,
//! and only ever clicks the tiles of those three windows: focus, minimize
//! (then restore by clicking the tile), scroll, Esc, close. It restores the
//! pointer and closes its windows at the end.

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use dock_previews::geom::RectF;
    use dock_previews::preview::{current_model, render_live_png};
    use dock_previews::{Config, DockPreviews};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    type Ref = *const c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pt {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: Ref;
        fn CFRunLoopRunInMode(mode: Ref, secs: f64, ret: u8) -> i32;
        fn CFRelease(r: Ref);
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(src: Ref) -> Ref;
        fn CGEventGetLocation(e: Ref) -> Pt;
        fn CGEventCreateMouseEvent(src: Ref, ty: u32, p: Pt, button: u32) -> Ref;
        fn CGEventCreateScrollWheelEvent(src: Ref, units: u32, count: u32, w1: i32, ...) -> Ref;
        fn CGEventCreateKeyboardEvent(src: Ref, key: u16, down: bool) -> Ref;
        fn CGEventPost(tap: u32, e: Ref);
    }

    fn pump(ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 0) };
        }
    }

    fn cursor() -> Pt {
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    fn post(e: Ref) {
        unsafe {
            CGEventPost(0, e);
            CFRelease(e);
        }
    }

    fn move_to(x: f64, y: f64) {
        post(unsafe { CGEventCreateMouseEvent(std::ptr::null(), 5, Pt { x, y }, 0) });
    }

    /// Glide from the current position to (x, y) in `steps` moves.
    fn glide(x: f64, y: f64, steps: usize) {
        let p = cursor();
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            move_to(p.x + (x - p.x) * t, p.y + (y - p.y) * t);
            pump(12);
        }
    }

    fn click(x: f64, y: f64, button: u32) {
        let (down, up) = match button {
            0 => (1, 2),
            _ => (25, 26),
        };
        post(unsafe { CGEventCreateMouseEvent(std::ptr::null(), down, Pt { x, y }, button) });
        pump(40);
        post(unsafe { CGEventCreateMouseEvent(std::ptr::null(), up, Pt { x, y }, button) });
    }

    fn scroll(lines: i32) {
        post(unsafe { CGEventCreateScrollWheelEvent(std::ptr::null(), 1, 1, lines) });
    }

    fn key(code: u16) {
        post(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, true) });
        pump(20);
        post(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, false) });
    }

    fn osa(script: &str) -> String {
        Command::new("osascript")
            .args(["-e", script])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string() + String::from_utf8_lossy(&o.stderr).trim())
            .unwrap_or_default()
    }

    /// TextEdit windows as "name:minimized" (read only).
    fn textedit_windows() -> String {
        osa(r#"tell application "System Events" to tell process "TextEdit" to get {name, value of attribute "AXMinimized"} of every window"#)
    }

    fn front_window() -> String {
        osa(r#"tell application "System Events" to tell (first process whose frontmost is true) to get {name, name of front window}"#)
    }

    /// Global center of the tile whose title is `title`, from the live model.
    fn tile_point(title: &str, part: &str) -> Option<(f64, f64)> {
        let m = current_model()?;
        let i = m.items.iter().position(|t| t.title == title)?;
        let t = m.layout.tiles[i];
        let r: RectF = match part {
            "close" => t.close,
            "minimize" => t.minimize,
            _ => t.thumb,
        };
        let (cx, cy) = r.center();
        Some((m.frame.x + cx, m.frame.y + cy))
    }

    fn check(ok: bool, what: &str, fails: &mut Vec<String>) {
        println!("[{}] {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            fails.push(what.to_string());
        }
    }

    pub fn main() -> anyhow::Result<()> {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).try_init();
        let args: Vec<String> = std::env::args().collect();
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        println!("permissions: {:?}", dock_previews::permissions());
        let handle = DockPreviews::start(Config::default())?;

        if let Some(i) = args.iter().position(|a| a == "--manual") {
            let secs: u64 = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(60);
            println!("hover Dock icons for {secs} s ...");
            pump(secs * 1000);
            handle.stop();
            return Ok(());
        }

        let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1).cloned()).unwrap_or_else(|| ".".into());
        std::fs::create_dir_all(&out)?;
        let names = ["dock-a.txt", "dock-b.txt", "dock-c.txt"];
        let files: Vec<String> = names.iter().map(|n| format!("{out}/{n}")).collect();
        for (f, body) in files.iter().zip(["Alpha — Dock previews test", "Beta — Dock previews test", "Gamma — Dock previews test"]) {
            std::fs::write(f, format!("{body}\n").repeat(12))?;
        }
        let mut fails = Vec::new();
        let saved = cursor();
        Command::new("open").arg("-a").arg("TextEdit").args(&files).status()?;
        pump(2500);
        println!("TextEdit windows: {}", textedit_windows());

        let dock = dock_previews::preview::read_dock().ok_or_else(|| anyhow::anyhow!("can't read the Dock"))?;
        println!("dock: {:?}, {} items, visible {}", dock.orientation, dock.items.len(), dock.is_visible());
        let icon = dock.items.iter().find(|i| i.path.ends_with("/TextEdit.app")).ok_or_else(|| anyhow::anyhow!("TextEdit not in the Dock"))?.frame;
        let (ix, iy) = icon.center();

        // 1. Hover delay: not shown after 150 ms, shown after ~400 ms.
        glide(ix, iy, 15);
        pump(150);
        check(!handle.is_shown(), "panel not shown before the hover delay", &mut fails);
        pump(450);
        check(handle.is_shown(), "panel shown after the hover delay", &mut fails);
        pump(1200); // thumbnails
        let m = current_model();
        check(m.as_ref().is_some_and(|m| names.iter().all(|n| m.items.iter().any(|t| t.title == *n))), "all three test windows listed", &mut fails);
        if let Some(m) = &m {
            println!("panel at {:?}, {} tiles, icon_mode {}", m.frame, m.items.len(), m.icon_mode);
            check(m.frame.bottom() <= icon.y, "panel above the icon", &mut fails);
        }
        render_live_png(&format!("{out}/live-hover-icon.png"))?;

        // 2. Move into the panel (through the bridge) onto dock-b: stays open, hover shows buttons.
        if let Some((bx, by)) = tile_point("dock-b.txt", "thumb") {
            glide(bx, by, 25);
            pump(400);
            check(handle.is_shown(), "panel stays open moving icon → panel", &mut fails);
            render_live_png(&format!("{out}/live-hover-tile.png"))?;
        }

        // 3. Minimize dock-b with its button, then restore it by clicking the tile.
        if let Some((mx, my)) = tile_point("dock-b.txt", "minimize") {
            glide(mx, my, 6);
            pump(200);
            click(mx, my, 0);
            pump(1800);
            let w = textedit_windows();
            println!("after minimize: {w}");
            check(handle.is_shown(), "panel still open after minimize", &mut fails);
            check(current_model().is_some_and(|m| m.items.iter().any(|t| t.title == "dock-b.txt" && t.badge == Some("Minimized"))), "dock-b tile marked Minimized", &mut fails);
            render_live_png(&format!("{out}/live-minimized.png"))?;
        }
        if let Some((bx, by)) = tile_point("dock-b.txt", "thumb") {
            glide(bx, by, 6);
            pump(150);
            click(bx, by, 0);
            pump(1500);
            check(!handle.is_shown(), "clicking a tile hides the panel", &mut fails);
            let f = front_window();
            println!("front: {f}");
            check(f.contains("TextEdit") && f.contains("dock-b.txt"), "clicked (minimized) window restored and focused", &mut fails);
        }

        // 4. Leave → hides; re-hover → shows; Esc → hides and stays hidden on the icon.
        glide(ix, iy - 300.0, 8);
        pump(600);
        check(!handle.is_shown(), "panel hidden after leaving", &mut fails);
        glide(ix, iy, 8);
        pump(700);
        check(handle.is_shown(), "panel shown again on re-hover", &mut fails);
        key(53);
        pump(300);
        check(!handle.is_shown(), "Esc hides the panel", &mut fails);
        pump(800);
        check(!handle.is_shown(), "stays hidden while still on the icon", &mut fails);

        // 5. Scroll to cycle (selection follows, windows raise).
        glide(ix + 30.0, iy - 300.0, 6);
        pump(300);
        glide(ix, iy, 6);
        pump(900);
        if let Some((ax, ay)) = tile_point("dock-a.txt", "thumb") {
            glide(ax, ay, 20);
            pump(300);
            scroll(-1);
            pump(500);
            let sel = current_model().and_then(|m| m.selected);
            println!("selected after scroll: {sel:?}, front {}", front_window());
            check(sel.is_some(), "scroll selects a tile", &mut fails);
            render_live_png(&format!("{out}/live-scroll.png"))?;
        }

        // 6. Close dock-c with the close button; middle-click closes dock-a.
        if let Some((cx, cy)) = tile_point("dock-c.txt", "close") {
            glide(cx, cy, 10);
            pump(250);
            click(cx, cy, 0);
            pump(1500);
            let w = textedit_windows();
            println!("after close: {w}");
            check(!w.contains("dock-c.txt"), "close button closed dock-c", &mut fails);
        }
        if let Some((ax, ay)) = tile_point("dock-a.txt", "thumb") {
            glide(ax, ay, 10);
            pump(250);
            click(ax, ay, 2);
            pump(1500);
            let w = textedit_windows();
            println!("after middle-click: {w}");
            check(!w.contains("dock-a.txt"), "middle-click closed dock-a", &mut fails);
        }

        // Leave to the side, well away from the Dock.
        let away = current_model().map(|m| (m.frame.x - 60.0).max(5.0)).unwrap_or(5.0);
        glide(away, 200.0, 10);
        pump(600);
        check(!handle.is_shown(), "panel hidden after leaving to the side", &mut fails);

        // Clean up: pointer back, close our remaining window(s), stop.
        println!("restoring the pointer to ({:.0}, {:.0})", saved.x, saved.y);
        glide(saved.x, saved.y, 10);
        pump(300);
        for n in names {
            osa(&format!(r#"tell application "TextEdit" to close (every document whose name is "{n}") saving no"#));
        }
        pump(500);
        println!("TextEdit windows after cleanup: {}", textedit_windows());
        handle.stop();
        if fails.is_empty() {
            println!("ALL PASSED");
        } else {
            println!("FAILED: {fails:?}");
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
